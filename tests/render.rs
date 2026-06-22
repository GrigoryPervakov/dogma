//! Render-buffer snapshot tests (Tier 1 from the test proposal).
//!
//! Each test builds an `App` in a known state, renders it through
//! ratatui's `TestBackend` to a fixed-size in-memory buffer, then snapshots
//! the rendered text via `insta`. Run with `cargo test --test render`.
//! Update accepted snapshots with `cargo insta review`.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use dogma::api::types::Token;
use dogma::app::state::{App, Mode};
use dogma::model::{Block, Message, Plan, Role, Session, Skill, Task, ToolCall, ToolCallStatus};
use dogma::view::chat::state::SessionKey;
use dogma::view::chat::{AgentStatus, ChatView, FocusTier};
use dogma::view::plans::PlansView;
use dogma::view::skills::SkillsView;
use dogma::view::tasks::TasksView;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn render_buf(app: &mut App, w: u16, h: u16) -> String {
    let backend = TestBackend::new(w, h);
    let mut term = Terminal::new(backend).expect("terminal");
    term.draw(|f| dogma::ui::render(app, f)).expect("draw");
    buffer_to_string(term.backend().buffer())
}

fn buffer_to_string(buf: &Buffer) -> String {
    let mut out = String::new();
    let area = buf.area();
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let s = cell.symbol();
            if s.is_empty() {
                out.push(' ');
            } else {
                out.push_str(s);
            }
        }
        out.push('\n');
    }
    out
}

fn chat_mut(app: &mut App) -> &mut ChatView {
    app.views[0]
        .as_any_mut()
        .downcast_mut::<ChatView>()
        .expect("ChatView is the first view")
}

fn user_session(id: &str, title: &str) -> Session {
    Session {
        id: id.into(),
        title: Some(title.into()),
        starred: false,
        created_at: Some("2026-04-29 10:00:00".into()),
        updated_at: Some("2026-04-29 14:32:00".into()),
        is_running: false,
        status: Default::default(),
        source: Some("web".into()),
        message_count: 5,
        total_cost_usd: 0.0,
    }
}

fn cron_session(id: &str, title: &str) -> Session {
    Session {
        id: id.into(),
        title: Some(title.into()),
        starred: false,
        created_at: Some("2026-04-29 04:00:00".into()),
        updated_at: Some("2026-04-29 04:01:00".into()),
        is_running: false,
        status: Default::default(),
        source: Some("cron".into()),
        message_count: 2,
        total_cost_usd: 0.0,
    }
}

fn user_msg(session_id: &str, content: &str) -> Message {
    let mut m = Message::new_user(session_id.into(), content.into());
    m.created_at = Some("2026-04-29 14:32:00".into());
    m
}

fn assistant_msg(session_id: &str, blocks: Vec<Block>) -> Message {
    let mut m = Message::new_streaming_assistant(session_id.into());
    m.role = Role::Assistant;
    m.blocks = blocks;
    m.created_at = Some("2026-04-29 14:32:18".into());
    m
}

fn text_block(s: &str) -> Block {
    Block::Text { content: s.into() }
}

fn edit_tool(file: &str, old: &str, new: &str) -> Block {
    Block::ToolCall(ToolCall {
        tool_use_id: "tool_e1".into(),
        tool: "Edit".into(),
        input: serde_json::json!({
            "file_path": file,
            "old_string": old,
            "new_string": new,
        }),
        result: Some("OK".into()),
        is_error: false,
        status: ToolCallStatus::Complete,
        parent_tool_use_id: None,
    })
}

fn bash_tool(cmd: &str, output: &str) -> Block {
    Block::ToolCall(ToolCall {
        tool_use_id: "tool_b1".into(),
        tool: "Bash".into(),
        input: serde_json::json!({"command": cmd}),
        result: Some(output.into()),
        is_error: false,
        status: ToolCallStatus::Complete,
        parent_tool_use_id: None,
    })
}

/// Build an App with the given sessions list.
fn app_with_sessions(sessions: Vec<Session>) -> App {
    let mut app = App::new("http://test".into(), Token::empty());
    let chat = chat_mut(&mut app);
    chat.sessions = sessions;
    chat.sessions_loaded = true;
    app
}

/// Mutate the chat view by calling the closure.
fn with_chat<F: FnOnce(&mut ChatView)>(app: &mut App, f: F) {
    let chat = chat_mut(app);
    f(chat);
}

/// Focus the tab backed by view type `V` (by type, not index) and return it
/// mutably — robust to nav-rail reordering.
fn focus_view_mut<V: 'static>(app: &mut App) -> &mut V {
    let idx = app
        .views
        .iter()
        .position(|v| v.as_any().downcast_ref::<V>().is_some())
        .expect("view present");
    app.current_view = idx;
    app.views[idx].as_any_mut().downcast_mut::<V>().unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn boot_sessions_focus_user_and_system_groups() {
    let mut app = app_with_sessions(vec![
        user_session("s1", "refactor the parser"),
        user_session("s2", "tui layout design"),
        cron_session("cron:pr-state", "Cron: pr-state"),
        cron_session("cron:issue-tracker", "Cron: issue-tracker"),
    ]);
    insta::assert_snapshot!(render_buf(&mut app, 80, 16));
}

#[test]
fn chatblocks_with_user_and_assistant_messages_selected_last() {
    let mut app = app_with_sessions(vec![user_session("s1", "design TUI")]);
    with_chat(&mut app, |chat| {
        chat.history.insert(
            "s1".into(),
            vec![
                user_msg("s1", "Take a look at src/parser.rs and tell me if the pattern is consistent."),
                assistant_msg(
                    "s1",
                    vec![
                        text_block("Consistent in 12 of 13 sites. The outlier is src/lexer.rs: it returns `Ok(())` when it should return `Err(Invalid)`."),
                        bash_tool("cargo check", "Finished `dev` profile in 1.2s\n"),
                    ],
                ),
            ],
        );
        chat.current = SessionKey::Real("s1".into());
        chat.sessions_selected = 1;
        chat.focus = FocusTier::ChatBlocks;
        // Simulate the load: select last item.
        let last = chat.history["s1"]
            .iter()
            .map(|m| m.blocks.len())
            .sum::<usize>()
            - 1;
        chat.ui
            .entry(SessionKey::Real("s1".into()))
            .or_default()
            .selected_block = Some(last);
    });
    insta::assert_snapshot!(render_buf(&mut app, 100, 24));
}

#[test]
fn streaming_session_frames_chat_and_hides_input() {
    let mut app = app_with_sessions(vec![user_session("s1", "live")]);
    with_chat(&mut app, |chat| {
        chat.history.insert(
            "s1".into(),
            vec![
                user_msg("s1", "Run the check."),
                assistant_msg(
                    "s1",
                    vec![
                        text_block("Running it now."),
                        bash_tool("cargo check", "ok\n"),
                    ],
                ),
            ],
        );
        chat.current = SessionKey::Real("s1".into());
        chat.sessions_selected = 1;
        chat.focus = FocusTier::ChatBlocks;
        // Actively streaming → green frame, no input box, uniform blocks.
        chat.agent_status.insert("s1".into(), AgentStatus::Writing);
    });
    insta::assert_snapshot!(render_buf(&mut app, 100, 24));
}

#[test]
fn tasks_panel_renders_at_bottom() {
    let mut app = app_with_sessions(vec![user_session("s1", "tasks demo")]);
    let task = |id: &str, name: &str, input: serde_json::Value, result: Option<&str>| {
        Block::ToolCall(ToolCall {
            tool_use_id: id.into(),
            tool: name.into(),
            input,
            result: result.map(str::to_string),
            is_error: false,
            status: ToolCallStatus::Complete,
            parent_tool_use_id: None,
        })
    };
    with_chat(&mut app, |chat| {
        chat.history.insert(
            "s1".into(),
            vec![assistant_msg(
                "s1",
                vec![
                    text_block("Planning the work."),
                    task(
                        "t1",
                        "TaskCreate",
                        serde_json::json!({"subject": "Read the file", "activeForm": "Reading the file"}),
                        Some("Task #1 created successfully: Read the file"),
                    ),
                    task(
                        "t2",
                        "TaskCreate",
                        serde_json::json!({"subject": "Write the fix"}),
                        Some("Task #2 created successfully: Write the fix"),
                    ),
                    task("t3", "TaskUpdate", serde_json::json!({"taskId": "1", "status": "in_progress"}), None),
                ],
            )],
        );
        chat.current = SessionKey::Real("s1".into());
        chat.sessions_selected = 1;
        chat.focus = FocusTier::ChatBlocks;
    });
    insta::assert_snapshot!(render_buf(&mut app, 100, 24));
}

#[test]
fn block_interior_zooms_into_edit_diff() {
    let mut app = app_with_sessions(vec![user_session("s1", "edit example")]);
    with_chat(&mut app, |chat| {
        chat.history.insert(
            "s1".into(),
            vec![assistant_msg(
                "s1",
                vec![edit_tool(
                    "/path/to/file.rs",
                    "let foo = 1;\nlet bar = 2;\nlet baz = 3;",
                    "let foo = 10;\nlet bar = 20;\nlet baz = 30;\nlet quux = 40;",
                )],
            )],
        );
        chat.current = SessionKey::Real("s1".into());
        chat.sessions_selected = 1;
        chat.focus = FocusTier::BlockInterior;
        chat.ui
            .entry(SessionKey::Real("s1".into()))
            .or_default()
            .selected_block = Some(0);
    });
    insta::assert_snapshot!(render_buf(&mut app, 80, 18));
}

#[test]
fn empty_session_shows_placeholder() {
    let mut app = app_with_sessions(vec![user_session("s1", "fresh chat")]);
    with_chat(&mut app, |chat| {
        chat.history.insert("s1".into(), vec![]);
        chat.current = SessionKey::Real("s1".into());
        chat.sessions_selected = 1;
        chat.focus = FocusTier::ChatBlocks;
    });
    insta::assert_snapshot!(render_buf(&mut app, 80, 12));
}

#[test]
fn command_mode_shows_buffer_in_statusbar() {
    let mut app = app_with_sessions(vec![user_session("s1", "session")]);
    with_chat(&mut app, |chat| {
        chat.focus = FocusTier::Sessions;
    });
    app.mode = Mode::Command;
    app.command_buffer = "fork \"new branch\"".into();
    insta::assert_snapshot!(render_buf(&mut app, 80, 12));
}

#[test]
fn command_mode_shows_autocomplete_menu() {
    let mut app = app_with_sessions(vec![user_session("s1", "session")]);
    app.mode = Mode::Command;
    // `s` prefix → skills + sources, with the matched prefix highlighted.
    app.command_buffer = "s".into();
    insta::assert_snapshot!(render_buf(&mut app, 80, 12));
}

#[test]
fn help_overlay_renders_centered() {
    let mut app = app_with_sessions(vec![]);
    app.mode = Mode::Help;
    insta::assert_snapshot!(render_buf(&mut app, 100, 30));
}

#[test]
fn empty_state_no_sessions() {
    let mut app = app_with_sessions(vec![]);
    insta::assert_snapshot!(render_buf(&mut app, 80, 16));
}

#[test]
fn tasks_tab_list_and_detail() {
    let mut app = App::new("http://test".into(), Token::empty());
    {
        let v = focus_view_mut::<TasksView>(&mut app);
        v.apply_list_loaded(Ok(vec![
            Task {
                id: "t1".into(),
                title: "Fix flaky webhook test".into(),
                status: "in_progress".into(),
                source: Some("github".into()),
                source_url: Some("https://github.com/x/y/issues/42".into()),
                deadline: Some("2026-05-02 12:00".into()),
                created_at: Some("2026-04-25 10:00:00".into()),
                updated_at: Some("2026-04-29 09:00:00".into()),
                content: None,
            },
            Task {
                id: "t2".into(),
                title: "Review PR #175".into(),
                status: "pending".into(),
                source: None,
                source_url: None,
                deadline: None,
                created_at: Some("2026-04-28 08:00:00".into()),
                updated_at: Some("2026-04-29 09:00:00".into()),
                content: None,
            },
            Task {
                id: "t3".into(),
                title: "Rebase main".into(),
                status: "done".into(),
                source: None,
                source_url: None,
                deadline: None,
                created_at: Some("2026-04-26 12:00:00".into()),
                updated_at: Some("2026-04-29 08:30:00".into()),
                content: None,
            },
        ]));
        v.apply_detail_loaded(
            "t1",
            Ok(Task {
                id: "t1".into(),
                title: "Fix flaky webhook test".into(),
                status: "in_progress".into(),
                source: Some("github".into()),
                source_url: Some("https://github.com/x/y/issues/42".into()),
                deadline: Some("2026-05-02 12:00".into()),
                created_at: Some("2026-04-25 10:00:00".into()),
                updated_at: Some("2026-04-29 09:00:00".into()),
                content: Some(
                    "## Repro\n\nThe webhook test fails ~30% of runs:\n\n```\n--- FAIL: TestWebhookFlow\n```\n\n## Plan\n\n- [ ] Check timing assumptions\n- [ ] Add retry on transient errors"
                        .into(),
                ),
            }),
        );
    }
    insta::assert_snapshot!(render_buf(&mut app, 100, 24));
}

#[test]
fn plans_tab_list_and_detail() {
    let mut app = App::new("http://test".into(), Token::empty());
    {
        let v = focus_view_mut::<PlansView>(&mut app);
        v.apply_list_loaded(Ok(vec![
            Plan {
                id: "p1".into(),
                task_id: Some("t1".into()),
                status: "pending".into(),
                title: Some("Refactor the parser pipeline".into()),
                content: None,
                feedback: None,
                created_at: Some("2026-04-29 11:00:00".into()),
                updated_at: Some("2026-04-29 11:00:00".into()),
                runtime: Some("claude".into()),
            },
            Plan {
                id: "p2".into(),
                task_id: Some("t1".into()),
                status: "approved".into(),
                title: Some("Update webhook validation".into()),
                content: None,
                feedback: None,
                created_at: Some("2026-04-28 09:00:00".into()),
                updated_at: Some("2026-04-29 10:00:00".into()),
                runtime: Some("claude".into()),
            },
        ]));
        v.apply_detail_loaded(
            "p1",
            Ok(Plan {
                id: "p1".into(),
                task_id: Some("t1".into()),
                status: "pending".into(),
                title: Some("Refactor the parser pipeline".into()),
                content: Some(
                    "## Goal\nSplit the monolithic parse loop into named steps.\n\n## Steps\n\n1. Extract `prep` step\n2. Extract `validate` step\n3. Extract `apply` step\n4. Wire the pipeline registry"
                        .into(),
                ),
                feedback: None,
                created_at: Some("2026-04-29 11:00:00".into()),
                updated_at: Some("2026-04-29 11:00:00".into()),
                runtime: Some("claude".into()),
            }),
        );
    }
    insta::assert_snapshot!(render_buf(&mut app, 100, 22));
}

#[test]
fn skills_tab_list_and_detail() {
    let mut app = App::new("http://test".into(), Token::empty());
    {
        let v = focus_view_mut::<SkillsView>(&mut app);
        v.apply_list_loaded(Ok(vec![
            Skill {
                id: "example-dev".into(),
                name: "example-dev".into(),
                description: Some("Example project dev workflow".into()),
                version: Some("1.0".into()),
                enabled: true,
                content: None,
                usage_count: Some(42),
                last_used_at: Some("2026-04-28 14:00:00".into()),
            },
            Skill {
                id: "nerve-dev".into(),
                name: "nerve-dev".into(),
                description: Some("Nerve backend / frontend dev".into()),
                version: Some("1.0".into()),
                enabled: true,
                content: None,
                usage_count: Some(7),
                last_used_at: None,
            },
            Skill {
                id: "old-skill".into(),
                name: "old-skill".into(),
                description: Some("Disabled legacy skill".into()),
                version: Some("0.3".into()),
                enabled: false,
                content: None,
                usage_count: Some(0),
                last_used_at: None,
            },
        ]));
        v.apply_detail_loaded(
            "example-dev",
            Ok(Skill {
                id: "example-dev".into(),
                name: "example-dev".into(),
                description: Some("Example project dev workflow".into()),
                version: Some("1.0".into()),
                enabled: true,
                content: Some(
                    "# Example Dev\n\nUse when working in `~/code/example`.\n\n## Build\n\n- `make build`\n- `make test`\n- `make lint`"
                        .into(),
                ),
                usage_count: Some(42),
                last_used_at: Some("2026-04-28 14:00:00".into()),
            }),
        );
    }
    insta::assert_snapshot!(render_buf(&mut app, 100, 22));
}

#[test]
fn narrow_terminal_hides_sidebar() {
    let mut app = app_with_sessions(vec![user_session("s1", "narrow test")]);
    with_chat(&mut app, |chat| {
        chat.history
            .insert("s1".into(), vec![user_msg("s1", "Quick question.")]);
        chat.current = SessionKey::Real("s1".into());
        chat.sessions_selected = 1;
        chat.focus = FocusTier::ChatBlocks;
    });
    // Below the 80-col threshold: sidebar hides.
    insta::assert_snapshot!(render_buf(&mut app, 70, 14));
}
