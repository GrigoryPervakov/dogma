//! The streaming reducer — applies WS server messages to ChatView state.
//!
//! Idempotent on tool_use_id so reconnect-replay (`session_status.buffered_events`)
//! produces the same state as a single live stream.

use serde_json::Value;

use crate::api::types::WsServerMsg;
use crate::model::{Block, Message, ToolCall, ToolCallStatus};
use crate::view::ViewCtx;

use super::state::{
    AgentStatus, ChatView, FocusTier, PendingInteraction, SessionKey, SubAgentPanel,
};

pub fn apply(view: &mut ChatView, msg: &WsServerMsg, ctx: &mut ViewCtx) {
    let _ = ctx;
    match msg {
        WsServerMsg::SessionSwitched { session_id } => {
            // Server told us our active session. If we don't have a current
            // session yet, adopt it.
            if matches!(view.current, SessionKey::NewChat) && !session_id.is_empty() {
                view.current = SessionKey::Real(session_id.clone());
            }
        }

        WsServerMsg::SessionStatus {
            session_id,
            is_running,
            buffered_events,
            ..
        } => {
            // Replay buffered events through the same reducer for idempotency.
            for ev in buffered_events {
                apply(view, ev, ctx);
            }
            if !is_running {
                view.agent_status
                    .insert(session_id.clone(), AgentStatus::Idle);
            }
        }

        WsServerMsg::Token {
            session_id,
            content,
            parent_tool_use_id,
        } => {
            if let Some(parent) = parent_tool_use_id.as_ref() {
                push_text_to_panel(view, parent, content);
            } else {
                let m = stream_msg_mut(view, session_id);
                append_text(m, content);
                view.agent_status
                    .insert(session_id.clone(), AgentStatus::Writing);
            }
        }

        WsServerMsg::Thinking {
            session_id,
            content,
            parent_tool_use_id,
        } => {
            if let Some(parent) = parent_tool_use_id.as_ref() {
                push_thinking_to_panel(view, parent, content);
            } else {
                let m = stream_msg_mut(view, session_id);
                append_thinking(m, content);
                view.agent_status
                    .insert(session_id.clone(), AgentStatus::Thinking);
            }
        }

        WsServerMsg::ToolUse {
            session_id,
            tool,
            input,
            tool_use_id,
            parent_tool_use_id,
        } => {
            let id = tool_use_id.clone().unwrap_or_default();
            let parent = parent_tool_use_id.clone();

            if let Some(parent_id) = parent {
                add_tool_call_to_panel(view, &parent_id, &id, tool, input);
            } else {
                let m = stream_msg_mut(view, session_id);
                upsert_tool_call(m, &id, tool, input, None, false, ToolCallStatus::Streaming);
                view.agent_status
                    .insert(session_id.clone(), AgentStatus::Tool(tool.clone()));
            }
        }

        WsServerMsg::ToolResult {
            session_id,
            tool_use_id,
            result,
            is_error,
            parent_tool_use_id,
        } => {
            let id = tool_use_id.clone().unwrap_or_default();
            let err = is_error.unwrap_or(false);
            if let Some(parent_id) = parent_tool_use_id.as_ref() {
                set_tool_result_in_panel(view, parent_id, &id, result, err);
            } else {
                let m = stream_msg_mut(view, session_id);
                set_tool_result(m, &id, result, err);
            }
        }

        WsServerMsg::Done {
            session_id,
            usage,
            max_context_tokens,
            num_turns,
        } => {
            // Move streaming into history.
            if let Some(streaming) = view.streaming.remove(session_id) {
                view.history
                    .entry(session_id.clone())
                    .or_default()
                    .push(streaming);
            }
            view.agent_status
                .insert(session_id.clone(), AgentStatus::Idle);
            // Update context usage.
            let entry = view.context.entry(session_id.clone()).or_default();
            if let Some(u) = usage.clone() {
                entry.last = Some(u);
            }
            if let Some(m) = max_context_tokens {
                entry.max_context_tokens = Some(*m);
            }
            if let Some(n) = num_turns {
                entry.num_turns = Some(*n);
            }
            // If follow-tail is on, selection moves to the new last item.
            if let SessionKey::Real(id) = &view.current
                && id == session_id
            {
                let total = super::items::count(view.current_history(), view.current_streaming());
                let key = SessionKey::Real(id.clone());
                let ui = view.ui.entry(key).or_default();
                if ui.follow_tail && total > 0 {
                    ui.selected_block = Some(total - 1);
                }
            }
        }

        WsServerMsg::Stopped { session_id } => {
            if let Some(streaming) = view.streaming.get_mut(session_id) {
                append_text(streaming, "\n[stopped]");
            }
            if let Some(streaming) = view.streaming.remove(session_id) {
                view.history
                    .entry(session_id.clone())
                    .or_default()
                    .push(streaming);
            }
            view.agent_status
                .insert(session_id.clone(), AgentStatus::Idle);
        }

        WsServerMsg::Error { session_id, error } => {
            if let Some(streaming) = view.streaming.remove(session_id) {
                view.history
                    .entry(session_id.clone())
                    .or_default()
                    .push(streaming);
            }
            view.history
                .entry(session_id.clone())
                .or_default()
                .push(error_message(session_id, error));
            view.agent_status
                .insert(session_id.clone(), AgentStatus::Idle);
        }

        WsServerMsg::SessionUpdated { session_id, title } => {
            if let Some(t) = title.as_ref()
                && let Some(s) = view.sessions.iter_mut().find(|s| &s.id == session_id)
            {
                s.title = Some(t.clone());
            }
        }

        WsServerMsg::SessionRunning {
            session_id,
            is_running,
        } => {
            if let Some(s) = view.sessions.iter_mut().find(|s| &s.id == session_id) {
                s.is_running = *is_running;
            }
        }

        WsServerMsg::Interaction {
            session_id,
            interaction_id,
            interaction_type,
            tool_name,
            tool_input,
        } => {
            // Ignore an interaction we've already answered/denied — buffered
            // events replay it on session re-entry and must not re-open the poll.
            if !view.answered_interactions.contains(interaction_id) {
                view.pending_interaction.insert(
                    session_id.clone(),
                    PendingInteraction {
                        session_id: session_id.clone(),
                        interaction_id: interaction_id.clone(),
                        interaction_type: interaction_type.clone(),
                        tool_name: tool_name.clone(),
                        tool_input: tool_input.clone(),
                    },
                );
                // Build the poll selection state and, for a question waiting on
                // the session we're viewing, jump straight to answering it.
                view.sync_poll();
                if view.active_poll().is_some() {
                    view.focus = FocusTier::Poll;
                }
            }
        }

        WsServerMsg::SubagentStart {
            tool_use_id,
            subagent_type,
            description,
            ..
        } => {
            // Open or update a side panel for this sub-agent.
            if !view
                .side_panel
                .panels
                .iter()
                .any(|p| p.tool_use_id == *tool_use_id)
            {
                view.side_panel.panels.push(SubAgentPanel {
                    tool_use_id: tool_use_id.clone(),
                    kind: subagent_type.clone(),
                    description: description.clone(),
                    blocks: Vec::new(),
                    running: true,
                });
                view.side_panel.visible = true;
            }
        }

        WsServerMsg::SubagentComplete { tool_use_id, .. } => {
            if let Some(p) = view
                .side_panel
                .panels
                .iter_mut()
                .find(|p| p.tool_use_id == *tool_use_id)
            {
                p.running = false;
            }
        }

        WsServerMsg::PlanUpdate { .. } => {
            // For v1, treat plan content as a normal text update —
            // future: dedicated plan panel + ExitPlanMode flow.
        }

        WsServerMsg::AnswerInjected {
            session_id,
            content,
            ..
        } => {
            view.history
                .entry(session_id.clone())
                .or_default()
                .push(Message::new_user(session_id.clone(), content.clone()));
        }

        WsServerMsg::BackgroundTasksUpdate { session_id, tasks } => {
            view.background_tasks
                .insert(session_id.clone(), tasks.clone());
        }

        WsServerMsg::SessionResumed { .. }
        | WsServerMsg::SessionForked { .. }
        | WsServerMsg::SessionArchived { .. }
        | WsServerMsg::FileChanged { .. }
        | WsServerMsg::HoaProgress { .. }
        | WsServerMsg::Notification(_)
        | WsServerMsg::NotificationAnswered(_)
        | WsServerMsg::Pong
        | WsServerMsg::Unknown => { /* ignored in v1 */ }
    }

    // After any state change, if we're at ChatBlocks tier with follow_tail
    // on, re-pin the selection to the newest visible block.
    update_follow_tail_selection(view);
}

fn update_follow_tail_selection(view: &mut ChatView) {
    if !matches!(view.focus, FocusTier::ChatBlocks) {
        return;
    }
    let total = super::items::count(view.current_history(), view.current_streaming());
    if total == 0 {
        return;
    }
    let key = view.current.clone();
    let ui = view.ui.entry(key).or_default();
    if ui.follow_tail {
        ui.selected_block = Some(total - 1);
        ui.seen_block_count = total;
    }
}

fn stream_msg_mut<'a>(view: &'a mut ChatView, session_id: &str) -> &'a mut Message {
    view.streaming
        .entry(session_id.to_string())
        .or_insert_with(|| Message::new_streaming_assistant(session_id.to_string()))
}

fn append_text(msg: &mut Message, s: &str) {
    if let Some(Block::Text { content }) = msg.blocks.last_mut() {
        content.push_str(s);
        return;
    }
    msg.blocks.push(Block::Text {
        content: s.to_string(),
    });
}

fn append_thinking(msg: &mut Message, s: &str) {
    if let Some(Block::Thinking { content }) = msg.blocks.last_mut() {
        content.push_str(s);
        return;
    }
    msg.blocks.push(Block::Thinking {
        content: s.to_string(),
    });
}

fn upsert_tool_call(
    msg: &mut Message,
    id: &str,
    tool: &str,
    input: &Value,
    result: Option<&str>,
    is_error: bool,
    status: ToolCallStatus,
) {
    if let Some(existing) = msg.blocks.iter_mut().find_map(|b| match b {
        Block::ToolCall(tc) if tc.tool_use_id == id => Some(tc),
        _ => None,
    }) {
        existing.tool = tool.to_string();
        existing.input = input.clone();
        if let Some(r) = result {
            existing.result = Some(r.to_string());
        }
        existing.is_error = is_error;
        existing.status = status;
        return;
    }
    msg.blocks.push(Block::ToolCall(ToolCall {
        tool_use_id: id.to_string(),
        tool: tool.to_string(),
        input: input.clone(),
        result: result.map(str::to_string),
        is_error,
        status,
        parent_tool_use_id: None,
    }));
}

fn set_tool_result(msg: &mut Message, id: &str, result: &str, is_error: bool) {
    if let Some(tc) = msg.blocks.iter_mut().find_map(|b| match b {
        Block::ToolCall(tc) if tc.tool_use_id == id => Some(tc),
        _ => None,
    }) {
        tc.result = Some(result.to_string());
        tc.is_error = is_error;
        tc.status = ToolCallStatus::Complete;
    }
}

// ---------- side-panel routing for sub-agent events ------------------------

fn ensure_panel<'a>(view: &'a mut ChatView, parent_id: &str) -> &'a mut SubAgentPanel {
    if !view
        .side_panel
        .panels
        .iter()
        .any(|p| p.tool_use_id == parent_id)
    {
        view.side_panel.panels.push(SubAgentPanel {
            tool_use_id: parent_id.to_string(),
            kind: "Agent".into(),
            description: String::new(),
            blocks: Vec::new(),
            running: true,
        });
        view.side_panel.visible = true;
    }
    view.side_panel
        .panels
        .iter_mut()
        .find(|p| p.tool_use_id == parent_id)
        .expect("panel just inserted")
}

fn push_text_to_panel(view: &mut ChatView, parent_id: &str, content: &str) {
    let p = ensure_panel(view, parent_id);
    if let Some(Block::Text { content: c }) = p.blocks.last_mut() {
        c.push_str(content);
        return;
    }
    p.blocks.push(Block::Text {
        content: content.to_string(),
    });
}

fn push_thinking_to_panel(view: &mut ChatView, parent_id: &str, content: &str) {
    let p = ensure_panel(view, parent_id);
    if let Some(Block::Thinking { content: c }) = p.blocks.last_mut() {
        c.push_str(content);
        return;
    }
    p.blocks.push(Block::Thinking {
        content: content.to_string(),
    });
}

fn add_tool_call_to_panel(
    view: &mut ChatView,
    parent_id: &str,
    tool_use_id: &str,
    tool: &str,
    input: &Value,
) {
    let p = ensure_panel(view, parent_id);
    if let Some(Block::ToolCall(tc)) = p
        .blocks
        .iter_mut()
        .find(|b| matches!(b, Block::ToolCall(tc) if tc.tool_use_id == tool_use_id))
    {
        tc.tool = tool.to_string();
        tc.input = input.clone();
        return;
    }
    p.blocks.push(Block::ToolCall(ToolCall {
        tool_use_id: tool_use_id.to_string(),
        tool: tool.to_string(),
        input: input.clone(),
        result: None,
        is_error: false,
        status: ToolCallStatus::Streaming,
        parent_tool_use_id: Some(parent_id.to_string()),
    }));
}

fn set_tool_result_in_panel(
    view: &mut ChatView,
    parent_id: &str,
    tool_use_id: &str,
    result: &str,
    is_error: bool,
) {
    let p = ensure_panel(view, parent_id);
    if let Some(Block::ToolCall(tc)) = p
        .blocks
        .iter_mut()
        .find(|b| matches!(b, Block::ToolCall(tc) if tc.tool_use_id == tool_use_id))
    {
        tc.result = Some(result.to_string());
        tc.is_error = is_error;
        tc.status = ToolCallStatus::Complete;
    }
}

fn error_message(session_id: &str, error: &str) -> Message {
    Message {
        id: None,
        session_id: session_id.to_string(),
        role: crate::model::Role::Assistant,
        blocks: vec![Block::Text {
            content: format!("[error] {error}"),
        }],
        content: None,
        thinking: None,
        created_at: Some(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string()),
        channel: Some("tui".into()),
    }
}

// ---------------------------------------------------------------------------
// Tests — pure reducer scenarios
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::types::WsServerMsg;
    use proptest::prelude::*;

    fn empty_view() -> ChatView {
        ChatView::new()
    }

    fn fake_ctx<'a>(actions: &'a mut Vec<crate::app::action::Action>) -> ViewCtx<'a> {
        ViewCtx {
            app_actions: actions,
        }
    }

    #[test]
    fn token_appends_to_text_block() {
        let mut v = empty_view();
        v.current = SessionKey::Real("s1".into());
        let mut a = Vec::new();
        let mut ctx = fake_ctx(&mut a);
        apply(
            &mut v,
            &WsServerMsg::Token {
                session_id: "s1".into(),
                content: "Hello".into(),
                parent_tool_use_id: None,
            },
            &mut ctx,
        );
        apply(
            &mut v,
            &WsServerMsg::Token {
                session_id: "s1".into(),
                content: " world".into(),
                parent_tool_use_id: None,
            },
            &mut ctx,
        );
        let m = v.streaming.get("s1").unwrap();
        assert_eq!(m.blocks.len(), 1);
        match &m.blocks[0] {
            Block::Text { content } => assert_eq!(content, "Hello world"),
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn tool_use_then_result_consolidates_into_one_block() {
        let mut v = empty_view();
        v.current = SessionKey::Real("s1".into());
        let mut a = Vec::new();
        let mut ctx = fake_ctx(&mut a);
        apply(
            &mut v,
            &WsServerMsg::ToolUse {
                session_id: "s1".into(),
                tool: "Bash".into(),
                input: serde_json::json!({"cmd": "ls"}),
                tool_use_id: Some("t1".into()),
                parent_tool_use_id: None,
            },
            &mut ctx,
        );
        apply(
            &mut v,
            &WsServerMsg::ToolResult {
                session_id: "s1".into(),
                tool_use_id: Some("t1".into()),
                result: "ok".into(),
                is_error: Some(false),
                parent_tool_use_id: None,
            },
            &mut ctx,
        );
        let m = v.streaming.get("s1").unwrap();
        assert_eq!(m.blocks.len(), 1);
        match &m.blocks[0] {
            Block::ToolCall(tc) => {
                assert_eq!(tc.tool_use_id, "t1");
                assert_eq!(tc.result.as_deref(), Some("ok"));
                assert_eq!(tc.status, ToolCallStatus::Complete);
            }
            other => panic!("unexpected block: {other:?}"),
        }
    }

    #[test]
    fn done_moves_streaming_into_history() {
        let mut v = empty_view();
        v.current = SessionKey::Real("s1".into());
        let mut a = Vec::new();
        let mut ctx = fake_ctx(&mut a);
        apply(
            &mut v,
            &WsServerMsg::Token {
                session_id: "s1".into(),
                content: "hi".into(),
                parent_tool_use_id: None,
            },
            &mut ctx,
        );
        apply(
            &mut v,
            &WsServerMsg::Done {
                session_id: "s1".into(),
                usage: None,
                max_context_tokens: Some(200_000),
                num_turns: None,
            },
            &mut ctx,
        );
        assert!(!v.streaming.contains_key("s1"));
        assert_eq!(v.history.get("s1").map(|h| h.len()), Some(1));
    }

    // -----------------------------------------------------------------------
    // Poll (AskUserQuestion) interaction flow
    // -----------------------------------------------------------------------

    fn question_event(session_id: &str, multi: bool) -> WsServerMsg {
        WsServerMsg::Interaction {
            session_id: session_id.into(),
            interaction_id: "iid".into(),
            interaction_type: "question".into(),
            tool_name: "AskUserQuestion".into(),
            tool_input: serde_json::json!({
                "questions": [{
                    "question": "Q", "header": "H", "multiSelect": multi,
                    "options": [
                        {"label": "A", "description": ""},
                        {"label": "B", "description": ""}
                    ]
                }]
            }),
        }
    }

    #[test]
    fn session_runtime_reflects_poll_stream_idle() {
        use crate::view::chat::state::SessionRuntime;
        let mut v = empty_view();
        assert_eq!(v.session_runtime("s1"), SessionRuntime::Idle);

        v.streaming
            .insert("s1".into(), Message::new_streaming_assistant("s1".into()));
        assert_eq!(v.session_runtime("s1"), SessionRuntime::Streaming);
        v.streaming.remove("s1");

        // A waiting poll outranks streaming.
        v.pending_interaction.insert(
            "s1".into(),
            super::PendingInteraction {
                session_id: "s1".into(),
                interaction_id: "i".into(),
                interaction_type: "question".into(),
                tool_name: "AskUserQuestion".into(),
                tool_input: serde_json::json!({}),
            },
        );
        assert_eq!(v.session_runtime("s1"), SessionRuntime::WaitingPoll);
    }

    #[test]
    fn interaction_question_builds_poll_and_focuses() {
        let mut v = empty_view();
        v.current = SessionKey::Real("s1".into());
        let mut a = Vec::new();
        let mut ctx = fake_ctx(&mut a);
        apply(&mut v, &question_event("s1", false), &mut ctx);
        assert!(v.active_poll().is_some());
        assert_eq!(v.focus, FocusTier::Poll);
    }

    #[test]
    fn poll_enter_submits_answer_and_clears_pending() {
        use crate::api::types::WsClientMsg;
        use crate::app::action::Action;
        use crate::view::View;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut v = empty_view();
        v.current = SessionKey::Real("s1".into());
        let mut a = Vec::new();
        {
            let mut ctx = fake_ctx(&mut a);
            apply(&mut v, &question_event("s1", false), &mut ctx);
        }
        a.clear();
        let mut ctx = fake_ctx(&mut a);
        // Highlight option B, then submit (single-select auto-submits).
        v.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut ctx);
        v.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut ctx);

        assert!(
            v.pending_interaction.is_empty(),
            "pending interaction not cleared"
        );
        assert!(v.poll_ui.is_none(), "poll_ui not cleared");
        let answered = a.iter().any(|act| matches!(
            act,
            Action::Ws(WsClientMsg::AnswerInteraction { interaction_id, result: Some(r), denied: false, .. })
                if interaction_id == "iid" && r["Q"] == serde_json::json!("B")
        ));
        assert!(answered, "expected AnswerInteraction with B, got {a:?}");
    }

    #[test]
    fn poll_esc_denies_and_clears_pending() {
        use crate::api::types::WsClientMsg;
        use crate::app::action::Action;
        use crate::view::View;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut v = empty_view();
        v.current = SessionKey::Real("s1".into());
        let mut a = Vec::new();
        {
            let mut ctx = fake_ctx(&mut a);
            apply(&mut v, &question_event("s1", true), &mut ctx);
        }
        a.clear();
        let mut ctx = fake_ctx(&mut a);
        v.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut ctx);

        assert!(v.pending_interaction.is_empty());
        let denied = a.iter().any(|act| {
            matches!(
                act,
                Action::Ws(WsClientMsg::AnswerInteraction {
                    result: None,
                    denied: true,
                    ..
                })
            )
        });
        assert!(denied, "expected denied AnswerInteraction, got {a:?}");
    }

    #[test]
    fn answered_poll_is_not_resurrected_on_replay() {
        use crate::view::View;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut v = empty_view();
        v.current = SessionKey::Real("s1".into());
        {
            let mut a = Vec::new();
            let mut ctx = fake_ctx(&mut a);
            apply(&mut v, &question_event("s1", false), &mut ctx);
            // Answer it.
            v.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut ctx);
        }
        assert!(v.poll_ui.is_none());
        assert!(v.pending_interaction.is_empty());

        // Re-entry replays the same interaction through the reducer.
        let mut a = Vec::new();
        let mut ctx = fake_ctx(&mut a);
        apply(&mut v, &question_event("s1", false), &mut ctx);

        assert!(v.poll_ui.is_none(), "answered poll resurrected");
        assert!(
            v.pending_interaction.is_empty(),
            "pending re-added for answered interaction"
        );
        assert!(v.active_poll().is_none());
        assert_ne!(v.focus, FocusTier::Poll);
    }

    #[test]
    fn new_chat_enter_carries_message_into_create_session() {
        use crate::api::types::HttpReq;
        use crate::app::action::Action;
        use crate::view::View;
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

        let mut v = empty_view(); // current == NewChat
        v.focus = FocusTier::Insert;
        let mut a = Vec::new();
        let mut ctx = fake_ctx(&mut a);
        for c in "hello".chars() {
            v.handle_key(
                KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
                &mut ctx,
            );
        }
        v.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut ctx);

        let carried = a.iter().any(|act| {
            matches!(
                act,
                Action::Http(HttpReq::CreateSession { content: Some(c), .. }) if c == "hello"
            )
        });
        assert!(
            carried,
            "CreateSession must carry the typed message, got {a:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Property tests (proptest-driven)
    // -----------------------------------------------------------------------
    //
    // Generates random sequences of `WsServerMsg` events and asserts the
    // streaming reducer keeps key invariants. Runs ~256 cases per test.

    /// Pool of tool_use_ids that the strategy reuses, so ToolResult events
    /// match real ToolUse events some of the time.
    const TOOL_IDS: &[&str] = &["t1", "t2", "t3"];

    fn small_string() -> impl Strategy<Value = String> {
        "[a-z]{1,8}".prop_map(|s| s)
    }

    fn tool_id() -> impl Strategy<Value = String> {
        prop_oneof![
            Just(TOOL_IDS[0].to_string()),
            Just(TOOL_IDS[1].to_string()),
            Just(TOOL_IDS[2].to_string()),
        ]
    }

    fn ws_event(session_id: String) -> impl Strategy<Value = WsServerMsg> {
        let s1 = session_id.clone();
        let s2 = session_id.clone();
        let s3 = session_id.clone();
        let s4 = session_id.clone();
        let s5 = session_id.clone();
        let s6 = session_id.clone();
        let s7 = session_id.clone();
        prop_oneof![
            small_string().prop_map(move |c| WsServerMsg::Token {
                session_id: s1.clone(),
                content: c,
                parent_tool_use_id: None,
            }),
            small_string().prop_map(move |c| WsServerMsg::Thinking {
                session_id: s2.clone(),
                content: c,
                parent_tool_use_id: None,
            }),
            (small_string(), tool_id()).prop_map(move |(t, id)| WsServerMsg::ToolUse {
                session_id: s3.clone(),
                tool: t,
                input: serde_json::json!({}),
                tool_use_id: Some(id),
                parent_tool_use_id: None,
            }),
            (small_string(), tool_id(), any::<bool>()).prop_map(move |(r, id, err)| {
                WsServerMsg::ToolResult {
                    session_id: s4.clone(),
                    tool_use_id: Some(id),
                    result: r,
                    is_error: Some(err),
                    parent_tool_use_id: None,
                }
            }),
            Just(WsServerMsg::Done {
                session_id: s5.clone(),
                usage: None,
                max_context_tokens: Some(200_000),
                num_turns: Some(1),
            }),
            Just(WsServerMsg::Stopped {
                session_id: s6.clone()
            }),
            small_string().prop_map(move |e| WsServerMsg::Error {
                session_id: s7.clone(),
                error: e,
            }),
        ]
    }

    fn fresh_chat(session_id: &str) -> ChatView {
        let mut v = ChatView::new();
        v.current = SessionKey::Real(session_id.to_string());
        v
    }

    fn run_events(chat: &mut ChatView, events: &[WsServerMsg]) {
        let mut acts = Vec::new();
        let mut ctx = ViewCtx {
            app_actions: &mut acts,
        };
        for ev in events {
            apply(chat, ev, &mut ctx);
        }
    }

    proptest! {
        /// `selected_block` (item index) must always reference a real item
        /// or be `None`, never a stale index past the end.
        #[test]
        fn selection_stays_in_bounds(events in prop::collection::vec(ws_event("s".into()), 0..40)) {
            let mut chat = fresh_chat("s");
            chat.focus = crate::view::chat::FocusTier::ChatBlocks;
            run_events(&mut chat, &events);
            let total = crate::view::chat::items::count(
                chat.current_history(),
                chat.current_streaming(),
            );
            let key = SessionKey::Real("s".into());
            if let Some(s) = chat.ui.get(&key).and_then(|u| u.selected_block) {
                prop_assert!(
                    s < total.max(1),
                    "selected_block {} out of bounds (items={})",
                    s, total
                );
            }
        }

        /// Every `Done` must clear the streaming buffer for that session.
        #[test]
        fn done_always_drains_streaming(events in prop::collection::vec(ws_event("s".into()), 0..40)) {
            let mut chat = fresh_chat("s");
            run_events(&mut chat, &events);
            // Fire one final Done.
            let mut acts = Vec::new();
            let mut ctx = ViewCtx { app_actions: &mut acts };
            apply(
                &mut chat,
                &WsServerMsg::Done {
                    session_id: "s".into(),
                    usage: None,
                    max_context_tokens: None,
                    num_turns: None,
                },
                &mut ctx,
            );
            prop_assert!(
                !chat.streaming.contains_key("s"),
                "streaming buffer not drained after Done"
            );
        }

        /// `tool_use_id` collisions never produce duplicate tool_call blocks.
        /// A second ToolUse with the same id must overwrite the first one
        /// (idempotent on tool_use_id).
        #[test]
        fn tool_use_id_is_unique_per_block(events in prop::collection::vec(ws_event("s".into()), 0..40)) {
            let mut chat = fresh_chat("s");
            run_events(&mut chat, &events);

            // Across all messages currently held (history + streaming),
            // every tool_call block must have a unique tool_use_id.
            let mut all_msgs: Vec<&Message> = Vec::new();
            if let Some(h) = chat.history.get("s") {
                all_msgs.extend(h.iter());
            }
            if let Some(s) = chat.streaming.get("s") {
                all_msgs.push(s);
            }
            for m in &all_msgs {
                let mut seen = std::collections::HashSet::new();
                for b in &m.blocks {
                    if let Block::ToolCall(tc) = b {
                        prop_assert!(
                            seen.insert(tc.tool_use_id.clone()),
                            "duplicate tool_use_id {} in a single message",
                            tc.tool_use_id
                        );
                    }
                }
            }
        }

        /// Replaying a sequence through `SessionStatus::buffered_events`
        /// produces the same streaming-buffer state as feeding the same
        /// events live. (Catches non-idempotent reducer paths.)
        #[test]
        fn buffered_replay_matches_live(events in prop::collection::vec(ws_event("s".into()), 0..30)) {
            let mut live = fresh_chat("s");
            run_events(&mut live, &events);

            let mut replayed = fresh_chat("s");
            let mut acts = Vec::new();
            let mut ctx = ViewCtx { app_actions: &mut acts };
            apply(
                &mut replayed,
                &WsServerMsg::SessionStatus {
                    session_id: "s".into(),
                    is_running: true,
                    status: None,
                    buffered_events: events,
                },
                &mut ctx,
            );

            // Compare the streaming buffer's block shape (we accept that
            // text-block coalescing is the same on both sides).
            let live_stream = live.streaming.get("s");
            let replay_stream = replayed.streaming.get("s");
            prop_assert_eq!(
                live_stream.is_some(),
                replay_stream.is_some(),
                "streaming presence diverged"
            );
            if let (Some(a), Some(b)) = (live_stream, replay_stream) {
                prop_assert_eq!(
                    a.blocks.len(),
                    b.blocks.len(),
                    "streaming block counts diverged"
                );
            }
            // History length must match too.
            let live_hist = live.history.get("s").map(|h| h.len()).unwrap_or(0);
            let replay_hist = replayed.history.get("s").map(|h| h.len()).unwrap_or(0);
            prop_assert_eq!(live_hist, replay_hist, "history length diverged");
        }
    }

    #[test]
    fn replay_is_idempotent() {
        let mut v1 = empty_view();
        v1.current = SessionKey::Real("s1".into());
        let mut a = Vec::new();
        let mut ctx = fake_ctx(&mut a);

        let events = vec![
            WsServerMsg::ToolUse {
                session_id: "s1".into(),
                tool: "Bash".into(),
                input: serde_json::json!({}),
                tool_use_id: Some("t1".into()),
                parent_tool_use_id: None,
            },
            WsServerMsg::ToolResult {
                session_id: "s1".into(),
                tool_use_id: Some("t1".into()),
                result: "done".into(),
                is_error: None,
                parent_tool_use_id: None,
            },
        ];
        for ev in &events {
            apply(&mut v1, ev, &mut ctx);
        }

        let mut v2 = empty_view();
        v2.current = SessionKey::Real("s1".into());
        let mut a2 = Vec::new();
        let mut ctx2 = fake_ctx(&mut a2);
        // Replay through SessionStatus.
        apply(
            &mut v2,
            &WsServerMsg::SessionStatus {
                session_id: "s1".into(),
                is_running: true,
                status: None,
                buffered_events: events,
            },
            &mut ctx2,
        );

        let s1 = v1.streaming.get("s1").unwrap();
        let s2 = v2.streaming.get("s1").unwrap();
        assert_eq!(s1.blocks.len(), s2.blocks.len());
        match (&s1.blocks[0], &s2.blocks[0]) {
            (Block::ToolCall(a), Block::ToolCall(b)) => {
                assert_eq!(a.tool_use_id, b.tool_use_id);
                assert_eq!(a.result, b.result);
                assert_eq!(a.status, b.status);
            }
            _ => panic!("expected matching tool_call blocks"),
        }
    }
}
