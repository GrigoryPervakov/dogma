//! The pure-ish `update` function.

use crossterm::event::{Event as TermEvent, KeyCode, KeyEvent, KeyModifiers};

use crate::api::types::{HttpReq, HttpResultKind, WsClientMsg, WsConnEvent, WsServerMsg};
use crate::app::action::Action;
use crate::app::command::run_command;
use crate::app::event::{AppEvent, ConnEvent};
use crate::app::state::{App, Mode, WsConnState};
use crate::instance::InstanceId;
use crate::view::ViewCtx;
use crate::view::chat::{AgentStatus, ChatView};

pub fn update(app: &mut App, event: AppEvent) -> Vec<Action> {
    let mut actions: Vec<Action> = Vec::new();

    match event {
        AppEvent::Tick => {
            // Re-render for spinners, and let the chat watchdog resync a
            // session that's gone quiet mid-stream — per instance, since each
            // has its own WS connection.
            let connected: Vec<bool> = app
                .instances
                .iter()
                .map(|i| matches!(i.ws, WsConnState::Connected))
                .collect();
            let ids = app.instance_ids.clone();
            if let Some(view) = chat_view_mut(app) {
                let mut ctx = ViewCtx {
                    app_actions: &mut actions,
                    instances: &ids,
                };
                view.tick_watchdog(&connected, &mut ctx);
            }
            app.mark_dirty();
        }
        AppEvent::Term(TermEvent::Key(k)) => {
            // Filter out KeyEventKind::Release on Windows / certain terms.
            if matches!(k.kind, crossterm::event::KeyEventKind::Release) {
                return actions;
            }
            handle_key(app, k, &mut actions);
            app.mark_dirty();
        }
        AppEvent::Term(TermEvent::Resize(_, _)) => {
            app.mark_dirty();
        }
        AppEvent::Term(TermEvent::Paste(text)) => {
            handle_paste(app, text);
            app.mark_dirty();
        }
        AppEvent::Term(_) => { /* ignore mouse / focus for v1 */ }
        AppEvent::Inst { instance, ev } => {
            match *ev {
                ConnEvent::Wire(msg) => actions.extend(handle_wire(app, instance, msg)),
                ConnEvent::WireConn(c) => actions.extend(handle_conn(app, instance, c)),
                ConnEvent::Http(res) => actions.extend(handle_http(app, instance, res)),
            }
            app.mark_dirty();
        }
        AppEvent::Fatal(msg) => {
            app.fatal = Some(msg);
            app.should_quit = true;
            app.mark_dirty();
        }
    }
    actions
}

// --------------------------------------------------------------------------
// Key dispatch
// --------------------------------------------------------------------------

fn handle_key(app: &mut App, key: KeyEvent, actions: &mut Vec<Action>) {
    // Global Ctrl+C: stop the running agent if any, otherwise quit the app.
    // Applies in every tier including Insert. Command mode is the only place
    // Ctrl+C is allowed through (Esc dismisses Command; Ctrl+C also bails).
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        if matches!(app.mode, Mode::Command) {
            app.command_buffer.clear();
            app.mode = Mode::Normal;
            return;
        }
        if let Some(chat) = chat_view_mut(app)
            && has_running_agent(chat)
            && let Some(sref) = chat.current_session_ref().cloned()
        {
            actions.push(Action::Ws {
                instance: sref.instance,
                msg: WsClientMsg::Stop {
                    session_id: sref.id,
                },
            });
            return;
        }
        actions.push(Action::Quit);
        return;
    }

    // Modal layers first.
    match app.mode {
        Mode::Help => {
            // Any key dismisses help.
            app.mode = Mode::Normal;
            return;
        }
        Mode::Command => {
            handle_command_key(app, key, actions);
            return;
        }
        Mode::Normal => {}
    }

    // Global Normal-mode keys. Skipped while the active view owns every key
    // (insert tier, poll answering) so `?` / `:` / `q` reach the view.
    let view_owns_keys = app.views[app.current_view].consumes_global_shortcuts();
    match (key.code, key.modifiers) {
        (KeyCode::Char('?'), m) if m.is_empty() && !view_owns_keys => {
            app.mode = Mode::Help;
            return;
        }
        (KeyCode::Char(':'), m) if m.is_empty() && !view_owns_keys => {
            app.mode = Mode::Command;
            app.command_buffer.clear();
            return;
        }
        // Cycle to the next/previous *supported* view (stubs are skipped).
        (KeyCode::Tab, KeyModifiers::NONE) => {
            cycle_view(app, 1, actions);
            return;
        }
        (KeyCode::BackTab, _) => {
            cycle_view(app, -1, actions);
            return;
        }
        // Only quit at top-level — let the active view veto when it owns keys.
        (KeyCode::Char('q'), KeyModifiers::NONE) if !view_owns_keys => {
            actions.push(Action::Quit);
            return;
        }
        _ => {}
    }

    // Forward to the active view.
    let ids = app.instance_ids.clone();
    let idx = app.current_view;
    let mut ctx = ViewCtx {
        app_actions: actions,
        instances: &ids,
    };
    app.views[idx].handle_key(key, &mut ctx);
}

/// A bracketed paste lands in the chat composer when it's the active view and
/// the input is focused. Multi-line content stays one message (the terminal
/// batches it into a single event, so embedded newlines don't submit).
fn handle_paste(app: &mut App, text: String) {
    if !matches!(app.mode, Mode::Normal) {
        return;
    }
    if app.views[app.current_view].id() != "chat" {
        return;
    }
    if let Some(chat) = chat_view_mut(app) {
        chat.paste_into_input(&text);
    }
}

fn handle_command_key(app: &mut App, key: KeyEvent, actions: &mut Vec<Action>) {
    match key.code {
        KeyCode::Esc => {
            app.command_buffer.clear();
            app.mode = Mode::Normal;
        }
        KeyCode::Enter => {
            let line = std::mem::take(&mut app.command_buffer);
            app.mode = Mode::Normal;
            actions.extend(run_command(app, &line));
        }
        KeyCode::Backspace => {
            app.command_buffer.pop();
        }
        KeyCode::Tab => {
            // Complete the command token to the longest common prefix of the
            // matches. Skip once an argument is being typed.
            if !app.command_buffer.contains(' ')
                && let Some(c) = crate::app::command::complete_prefix(&app.command_buffer)
            {
                app.command_buffer = c;
            }
        }
        KeyCode::Char(c) => {
            app.command_buffer.push(c);
        }
        _ => {}
    }
}

// --------------------------------------------------------------------------
// Wire → state
// --------------------------------------------------------------------------

fn handle_wire(app: &mut App, instance: InstanceId, msg: WsServerMsg) -> Vec<Action> {
    let mut actions = Vec::new();
    let ids = app.instance_ids.clone();
    if let Some(view) = chat_view_mut(app) {
        let mut ctx = ViewCtx {
            app_actions: &mut actions,
            instances: &ids,
        };
        view.apply_wire(instance, &msg, &mut ctx);
    }
    actions
}

fn handle_conn(app: &mut App, instance: InstanceId, c: WsConnEvent) -> Vec<Action> {
    let ws = match c {
        WsConnEvent::Connecting => WsConnState::Connecting,
        WsConnEvent::Connected => WsConnState::Connected,
        WsConnEvent::Disconnected {
            reason,
            retry_in_ms,
        } => match retry_in_ms {
            Some(ms) => WsConnState::Reconnecting {
                retry_in_ms: ms,
                reason,
            },
            None => WsConnState::Disconnected,
        },
        WsConnEvent::AuthRejected => WsConnState::AuthRejected,
    };
    // Transition into Connected (from anything else) → refetch this instance's
    // lists so the merged views are current after an outage / first connect.
    let was_connected = app
        .instances
        .get(instance.index())
        .map(|m| matches!(m.ws, WsConnState::Connected))
        .unwrap_or(false);
    let became_connected = matches!(ws, WsConnState::Connected) && !was_connected;
    if let Some(meta) = app.instance_mut(instance) {
        meta.ws = ws;
    }
    if became_connected {
        vec![
            Action::Http {
                instance,
                req: HttpReq::ListSessions,
            },
            Action::Http {
                instance,
                req: HttpReq::ListNotifications,
            },
            Action::Http {
                instance,
                req: HttpReq::ListModels,
            },
        ]
    } else {
        Vec::new()
    }
}

fn handle_http(
    app: &mut App,
    instance: InstanceId,
    res: crate::api::types::HttpResult,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let ids = app.instance_ids.clone();
    let mut ctx = ViewCtx {
        app_actions: &mut actions,
        instances: &ids,
    };
    match res.kind {
        HttpResultKind::Sessions(r) => {
            if let Some(view) = chat_view_mut(app) {
                view.apply_sessions_loaded(instance, r, &mut ctx);
            }
        }
        HttpResultKind::Messages {
            session_id,
            limit,
            result,
        } => {
            if let Some(view) = chat_view_mut(app) {
                view.apply_messages_loaded(instance, &session_id, limit, result, &mut ctx);
            }
        }
        HttpResultKind::SessionCreated {
            pending_content,
            result,
        } => {
            if let Some(view) = chat_view_mut(app) {
                view.apply_session_created(instance, pending_content, result, &mut ctx);
            }
        }
        HttpResultKind::Tasks(r) => {
            if let Some(view) = view_mut::<crate::view::tasks::TasksView>(app) {
                view.apply_list_loaded(instance, r);
            }
        }
        HttpResultKind::TaskDetail { task_id, result } => {
            if let Some(view) = view_mut::<crate::view::tasks::TasksView>(app) {
                view.apply_detail_loaded(instance, &task_id, result);
            }
        }
        HttpResultKind::Plans(r) => {
            if let Some(view) = view_mut::<crate::view::plans::PlansView>(app) {
                view.apply_list_loaded(instance, r);
            }
        }
        HttpResultKind::PlanDetail { plan_id, result } => {
            if let Some(view) = view_mut::<crate::view::plans::PlansView>(app) {
                view.apply_detail_loaded(instance, &plan_id, result);
            }
        }
        HttpResultKind::PlanActed { plan_id, result } => {
            if let Some(view) = view_mut::<crate::view::plans::PlansView>(app) {
                view.apply_action_result(&plan_id, result);
            }
            // The plan's status changed server-side — refetch to reconcile.
            ctx.http(instance, HttpReq::ListPlans);
        }
        HttpResultKind::Skills(r) => {
            if let Some(view) = view_mut::<crate::view::skills::SkillsView>(app) {
                view.apply_list_loaded(instance, r);
            }
        }
        HttpResultKind::SkillDetail { skill_id, result } => {
            if let Some(view) = view_mut::<crate::view::skills::SkillsView>(app) {
                view.apply_detail_loaded(instance, &skill_id, result);
            }
        }
        HttpResultKind::Notifications(r) => {
            if let Some(view) = view_mut::<crate::view::notifications::NotificationsView>(app) {
                view.apply_list_loaded(instance, r);
            }
        }
        HttpResultKind::NotificationAnswered { id, answer, result } => {
            if let Some(view) = view_mut::<crate::view::notifications::NotificationsView>(app) {
                view.apply_answered(instance, &id, &answer, result);
            }
        }
        HttpResultKind::NotificationDismissed { id, result } => {
            if let Some(view) = view_mut::<crate::view::notifications::NotificationsView>(app) {
                view.apply_dismissed(instance, &id, result);
            }
        }
        HttpResultKind::ModifiedFiles { session_id, result } => {
            if let Some(view) = chat_view_mut(app) {
                view.apply_modified_files(instance, &session_id, result);
            }
        }
        HttpResultKind::FileDiff {
            session_id,
            path,
            result,
        } => {
            if let Some(view) = chat_view_mut(app) {
                view.apply_file_diff(instance, &session_id, &path, result);
            }
        }
        HttpResultKind::Models(result) => {
            if let Some(view) = chat_view_mut(app) {
                view.apply_models_loaded(instance, result);
            }
        }
    }
    actions
}

/// Switch to the next/previous selectable view and fire its `on_focus`.
fn cycle_view(app: &mut App, dir: isize, actions: &mut Vec<Action>) {
    app.current_view = next_selectable_view(app, dir);
    let ids = app.instance_ids.clone();
    let idx = app.current_view;
    let mut ctx = ViewCtx {
        app_actions: actions,
        instances: &ids,
    };
    app.views[idx].on_focus(&mut ctx);
}

/// Next view index in `dir` (+1/-1) whose `selectable()` is true, wrapping
/// around and skipping stub tabs. Falls back to the current index.
fn next_selectable_view(app: &App, dir: isize) -> usize {
    let n = app.views.len() as isize;
    let mut idx = app.current_view as isize;
    for _ in 0..n {
        idx = (idx + dir).rem_euclid(n);
        if app.views[idx as usize].selectable() {
            return idx as usize;
        }
    }
    app.current_view
}

fn view_mut<V: 'static>(app: &mut App) -> Option<&mut V> {
    app.views
        .iter_mut()
        .find_map(|v| v.as_any_mut().downcast_mut::<V>())
}

pub(crate) fn chat_view_mut(app: &mut App) -> Option<&mut ChatView> {
    app.views
        .iter_mut()
        .find_map(|v| v.as_any_mut().downcast_mut::<ChatView>())
}

fn has_running_agent(chat: &ChatView) -> bool {
    let sref = match chat.current_session_ref() {
        Some(s) => s,
        None => return false,
    };
    if chat.streaming.contains_key(sref) {
        return true;
    }
    !matches!(chat.current_agent_status(), AgentStatus::Idle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::Token;
    use crate::view::chat::FocusTier;

    fn char_key(c: char) -> AppEvent {
        AppEvent::Term(TermEvent::Key(KeyEvent::new(
            KeyCode::Char(c),
            KeyModifiers::NONE,
        )))
    }

    fn app_with_chat_focus(focus: FocusTier) -> App {
        let mut app = App::new("http://test".into(), Token::empty());
        chat_view_mut(&mut app).unwrap().focus = focus;
        app
    }

    #[test]
    fn ws_connected_transition_refetches_then_noop() {
        let mut app = App::new("http://test".into(), Token::empty());
        // Disconnected → Connected refetches this instance's lists.
        let acts = handle_conn(&mut app, InstanceId::PRIMARY, WsConnEvent::Connected);
        assert!(acts.iter().any(|a| matches!(
            a,
            Action::Http { instance, req: HttpReq::ListSessions } if *instance == InstanceId::PRIMARY
        )));
        assert!(acts.iter().any(|a| matches!(
            a,
            Action::Http {
                req: HttpReq::ListNotifications,
                ..
            }
        )));
        // Already connected → no repeated refetch.
        let again = handle_conn(&mut app, InstanceId::PRIMARY, WsConnEvent::Connected);
        assert!(again.is_empty());
    }

    #[test]
    fn printable_shortcuts_are_typed_while_in_insert_tier() {
        for c in ['?', ':'] {
            let mut app = app_with_chat_focus(FocusTier::Insert);
            update(&mut app, char_key(c));
            assert!(
                matches!(app.mode, Mode::Normal),
                "{c} must not trigger a modal while typing",
            );
            let typed = chat_view_mut(&mut app).unwrap().input.lines().join("\n");
            assert!(typed.contains(c), "{c} must be inserted into the prompt");
        }
    }

    #[test]
    fn question_mark_outside_insert_still_opens_help() {
        let mut app = app_with_chat_focus(FocusTier::ChatBlocks);
        update(&mut app, char_key('?'));
        assert!(matches!(app.mode, Mode::Help));
    }

    #[test]
    fn input_tier_keeps_global_commands() {
        // `:` / `?` / `q` remain global commands on the focused input.
        let mut app = app_with_chat_focus(FocusTier::Input);
        update(&mut app, char_key(':'));
        assert!(matches!(app.mode, Mode::Command));
        assert!(matches!(
            chat_view_mut(&mut app).unwrap().focus,
            FocusTier::Input
        ));
    }

    #[test]
    fn input_tier_unmapped_char_starts_typing() {
        // A char no global shortcut claims enters insert and types itself.
        let mut app = app_with_chat_focus(FocusTier::Input);
        update(&mut app, char_key('h'));
        let chat = chat_view_mut(&mut app).unwrap();
        assert!(matches!(chat.focus, FocusTier::Insert));
        assert_eq!(chat.input.lines().join("\n"), "h");
    }
}
