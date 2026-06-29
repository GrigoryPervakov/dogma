//! `:` command parser + dispatch helpers.

use crate::app::action::Action;
use crate::app::state::{App, Mode};
use crate::view::ViewCtx;
use crate::view::chat::ChatCommand;

/// Completable command names (canonical forms). Tab names use the plural;
/// resolution also accepts the singular (toggling a trailing `s`).
pub const COMMANDS: &[&str] = &[
    "chat", "notifs", "tasks", "plans", "skills", "new", "fork", "resume", "rename", "delete",
    "reload", "help", "quit", "cron", "sources", "memory", "diag",
];

/// Commands whose canonical name starts with `prefix` (case-insensitive). An
/// empty prefix returns the full menu (typing `:` then nothing shows all).
pub fn completions(prefix: &str) -> Vec<&'static str> {
    let p = prefix.to_lowercase();
    COMMANDS
        .iter()
        .copied()
        .filter(|c| c.starts_with(&p))
        .collect()
}

/// Longest common prefix shared by every completion of `prefix` — the target
/// for Tab completion (>= `prefix`, or the full command when unique).
pub fn complete_prefix(prefix: &str) -> Option<String> {
    let matches = completions(prefix);
    let (first, rest) = matches.split_first()?;
    let mut lcp = (*first).to_string();
    for m in rest {
        while !m.starts_with(&lcp) {
            lcp.pop();
        }
    }
    Some(lcp)
}

pub fn run_command(app: &mut App, line: &str) -> Vec<Action> {
    let line = line.trim();
    if line.is_empty() {
        return Vec::new();
    }
    // Split into name + arg-rest.
    let (name, rest) = match line.split_once(' ') {
        Some((n, r)) => (n, r.trim()),
        None => (line, ""),
    };

    match name {
        "q" | "quit" | "exit" => vec![Action::Quit],
        "help" | "?" => {
            app.mode = Mode::Help;
            app.mark_dirty();
            Vec::new()
        }
        "reload" => {
            // Rebuild the current chat from scratch (unsticks a hung stream),
            // then refresh the sessions list on every instance.
            let mut actions = chat_command(app, ChatCommand::Reload);
            for &instance in &app.instance_ids {
                actions.push(Action::Http {
                    instance,
                    req: crate::api::types::HttpReq::ListSessions,
                });
            }
            actions
        }
        "new" => {
            let actions = chat_command(app, ChatCommand::NewChat(opt_string(rest)));
            // With several instances connected, ask which one before typing.
            if app.instances.len() > 1 {
                let ids = app.instance_ids.clone();
                if let Some(chat) = crate::app::update::chat_view_mut(app) {
                    chat.show_new_chat_picker(&ids);
                }
            }
            actions
        }
        "fork" => chat_command(app, ChatCommand::Fork(opt_string(rest))),
        "resume" => chat_command(app, ChatCommand::Resume),
        "rename" => chat_command(app, ChatCommand::Rename(opt_string(rest))),
        "delete" => chat_command(app, ChatCommand::Delete),
        // Otherwise: jump to a tab by its id (chat, notifs, tasks, plans,
        // skills, cron, sources, memory, diag).
        other => switch_to_view(app, other),
    }
}

/// Switch the active tab to the view whose `id()` matches `name`, firing its
/// `on_focus` (e.g. lazy list fetch). Unknown names are a no-op.
fn switch_to_view(app: &mut App, name: &str) -> Vec<Action> {
    // Accept singular or plural: exact id, else toggle a trailing `s`.
    let alt = match name.strip_suffix('s') {
        Some(stripped) => stripped.to_string(),
        None => format!("{name}s"),
    };
    let Some(idx) = app
        .views
        .iter()
        .position(|v| v.id() == name || v.id() == alt)
    else {
        return Vec::new();
    };
    app.current_view = idx;
    let mut actions = Vec::new();
    let ids = app.instance_ids.clone();
    let mut ctx = ViewCtx {
        app_actions: &mut actions,
        instances: &ids,
    };
    app.views[idx].on_focus(&mut ctx);
    app.mark_dirty();
    actions
}

fn chat_command(app: &mut App, cmd: ChatCommand) -> Vec<Action> {
    match crate::app::update::chat_view_mut(app) {
        Some(view) => view.run_command(cmd),
        None => Vec::new(),
    }
}

fn opt_string(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::run_command;
    use crate::api::types::Token;
    use crate::app::state::App;

    fn app() -> App {
        App::new("http://test".into(), Token::empty())
    }

    #[test]
    fn switches_to_notifs_tab_by_name() {
        let mut app = app();
        assert_eq!(app.views[app.current_view].id(), "chat");
        run_command(&mut app, "notifs");
        assert_eq!(app.views[app.current_view].id(), "notifs");
    }

    #[test]
    fn switches_to_each_supported_tab() {
        let mut app = app();
        for id in ["tasks", "plans", "skills", "chat"] {
            run_command(&mut app, id);
            assert_eq!(app.views[app.current_view].id(), id);
        }
    }

    #[test]
    fn unknown_command_does_not_switch() {
        let mut app = app();
        run_command(&mut app, "bogus");
        assert_eq!(app.views[app.current_view].id(), "chat");
    }

    #[test]
    fn singular_form_switches_tab() {
        // Singular resolves to the plural tab.
        for (singular, id) in [
            ("task", "tasks"),
            ("plan", "plans"),
            ("skill", "skills"),
            ("notif", "notifs"),
        ] {
            let mut app = app();
            super::run_command(&mut app, singular);
            assert_eq!(app.views[app.current_view].id(), id, "{singular}");
        }
    }

    #[test]
    fn completions_prefix_match_and_lcp() {
        use super::{complete_prefix, completions};
        assert_eq!(completions("ta"), vec!["tasks"]);
        assert_eq!(complete_prefix("ta").as_deref(), Some("tasks"));
        // `s` matches skills + sources → LCP is just "s".
        let m = completions("s");
        assert!(m.contains(&"skills") && m.contains(&"sources"));
        assert_eq!(complete_prefix("s").as_deref(), Some("s"));
        // empty → full menu; unknown → none.
        assert_eq!(completions(""), super::COMMANDS.to_vec());
        assert!(completions("zzz").is_empty());
        assert_eq!(complete_prefix("zzz"), None);
    }
}
