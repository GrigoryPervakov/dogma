//! Render the real dogma UI (via ratatui's `TestBackend`, the same path the
//! snapshot tests use) with anonymized fake data, and emit an 800x600 SVG of a
//! terminal window. CI renders this on every push and publishes the PNG to
//! GitHub Pages (see .github/workflows/screenshot.yml); to preview locally:
//!
//!   cargo run --example screenshot > /tmp/dogma.svg

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

use dogma::app::state::{App, InstanceMeta, WsConnState};
use dogma::instance::InstanceId;
use dogma::model::{Block, Message, Role, Session, ToolCall, ToolCallStatus};
use dogma::view::chat::state::{SessionKey, SessionRef};
use dogma::view::chat::{ChatView, FocusTier};

// 80 cols x 27 rows of terminal, drawn into the body of an 800x600 window.
const COLS: u16 = 80;
const ROWS: u16 = 27;
const CW: f64 = 9.6;
const CH: f64 = 20.0;
const GRID_X: f64 = 16.0;
const GRID_Y: f64 = 46.0;

const BG: &str = "#11111b"; // window / terminal background
const FG: &str = "#cdd6f4"; // default foreground

fn main() {
    // Two connected instances so the screenshot shows the merged, per-instance
    // badged UI ("local" + "vm").
    let mut app = App::with_instances(vec![
        InstanceMeta::new(
            InstanceId(0),
            "local".into(),
            "http://local.dev:8900".into(),
        ),
        InstanceMeta::new(InstanceId(1), "vm".into(), "http://vm.dev:8900".into()),
    ]);
    for inst in &mut app.instances {
        inst.ws = WsConnState::Connected;
    }
    seed(chat_mut(&mut app));
    let backend = TestBackend::new(COLS, ROWS);
    let mut term = Terminal::new(backend).expect("terminal");
    term.draw(|f| dogma::ui::render(&mut app, f)).expect("draw");
    print!("{}", buffer_to_svg(term.backend().buffer()));
}

fn chat_mut(app: &mut App) -> &mut ChatView {
    app.views[0]
        .as_any_mut()
        .downcast_mut::<ChatView>()
        .expect("ChatView is first")
}

fn session(id: &str, title: &str, source: &str, inst: InstanceId) -> Session {
    let mut s: Session = serde_json::from_value(serde_json::json!({
        "id": id, "title": title, "source": source,
        "updated_at": "2026-04-29 14:32:00",
    }))
    .unwrap();
    s.instance = inst;
    s
}

fn user_msg(content: &str) -> Message {
    let mut m = Message::new_user("s1".into(), content.into());
    m.created_at = Some("2026-04-29 14:31:00".into());
    m
}

fn assistant(blocks: Vec<Block>) -> Message {
    let mut m = Message::new_streaming_assistant("s1".into());
    m.role = Role::Assistant;
    m.blocks = blocks;
    m.created_at = Some("2026-04-29 14:32:18".into());
    m
}

fn tool(name: &str, input: serde_json::Value, result: Option<&str>) -> Block {
    Block::ToolCall(ToolCall {
        tool_use_id: format!("tu-{name}"),
        tool: name.into(),
        input,
        result: result.map(str::to_string),
        is_error: false,
        status: ToolCallStatus::Complete,
        parent_tool_use_id: None,
    })
}

fn seed(chat: &mut ChatView) {
    let local = InstanceId(0);
    let vm = InstanceId(1);
    chat.sessions = vec![
        session("s1", "refactor the parser", "web", local),
        session("s2", "api client retry/backoff", "web", vm),
        session("s3", "write integration tests", "web", local),
        session("cron:pr", "Cron: pr-dashboard", "cron", vm),
    ];
    chat.sessions_loaded = true;
    let s1 = SessionRef::new(local, "s1");
    chat.history.insert(
        s1.clone(),
        vec![
            user_msg("Look at src/parser.rs and tell me if the error handling is consistent."),
            assistant(vec![
                Block::Text {
                    content: "Consistent in 12 of 13 sites. The outlier is `src/lexer.rs`: it \
                              returns `Ok(())` where it should return `Err(Invalid)`."
                        .into(),
                },
                tool(
                    "Bash",
                    serde_json::json!({ "command": "cargo check" }),
                    Some("Finished `dev` profile in 1.2s\n"),
                ),
                tool(
                    "Edit",
                    serde_json::json!({
                        "file_path": "src/lexer.rs",
                        "old_string": "Ok(())",
                        "new_string": "Err(Invalid)",
                    }),
                    Some("OK"),
                ),
            ]),
        ],
    );
    chat.current = SessionKey::Real(s1.clone());
    chat.sessions_selected = 1;
    chat.focus = FocusTier::ChatBlocks;
    let last = chat.history[&s1]
        .iter()
        .map(|m| m.blocks.len())
        .sum::<usize>()
        - 1;
    chat.ui
        .entry(SessionKey::Real(s1))
        .or_default()
        .selected_block = Some(last);
}

// ---------------------------------------------------------------------------
// Buffer -> SVG
// ---------------------------------------------------------------------------

fn buffer_to_svg(buf: &Buffer) -> String {
    let area = buf.area();
    let mut out = String::new();
    out.push_str(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 800 600\" width=\"800\" \
         height=\"600\" font-family=\"ui-monospace,'SF Mono',Menlo,Consolas,monospace\" \
         font-size=\"15\">\n",
    );
    // Window + title bar chrome.
    out.push_str(&format!(
        "<rect width=\"800\" height=\"600\" rx=\"10\" fill=\"{BG}\"/>\n"
    ));
    out.push_str("<rect width=\"800\" height=\"36\" rx=\"10\" fill=\"#181825\"/>\n");
    out.push_str("<rect y=\"18\" width=\"800\" height=\"18\" fill=\"#181825\"/>\n");
    for (i, c) in ["#ff5f56", "#ffbd2e", "#27c93f"].iter().enumerate() {
        out.push_str(&format!(
            "<circle cx=\"{}\" cy=\"18\" r=\"6\" fill=\"{c}\"/>\n",
            22 + i * 20
        ));
    }
    out.push_str(
        "<text x=\"400\" y=\"23\" text-anchor=\"middle\" fill=\"#6c7086\" \
         font-size=\"13\">Dogma: local +1</text>\n",
    );

    // Background rects: merge horizontal runs of identical non-default bg.
    for y in 0..area.height {
        let mut x = 0u16;
        while x < area.width {
            let (fg, bg) = effective(&buf[(x, y)]);
            let _ = fg;
            if let Some(color) = bg.clone() {
                let start = x;
                while x < area.width && effective(&buf[(x, y)]).1 == bg {
                    x += 1;
                }
                out.push_str(&format!(
                    "<rect x=\"{:.2}\" y=\"{:.2}\" width=\"{:.2}\" height=\"{:.2}\" fill=\"{color}\"/>\n",
                    GRID_X + start as f64 * CW,
                    GRID_Y + y as f64 * CH,
                    (x - start) as f64 * CW,
                    CH,
                ));
            } else {
                x += 1;
            }
        }
    }

    // Text: one <text> per non-blank cell, pinned to its exact grid position and
    // stretched to a single cell width. This is what makes it look like a real
    // terminal — every column lands at the same x on every row, so vertical
    // borders line up and box-drawing characters tile seamlessly with no gaps.
    let by = |y: u16| GRID_Y + y as f64 * CH + 14.5;
    for y in 0..area.height {
        for x in 0..area.width {
            let cell = &buf[(x, y)];
            let sym = cell.symbol();
            if sym.is_empty() || sym == " " {
                continue;
            }
            let (fg, _) = effective(cell);
            let fill = fg.unwrap_or_else(|| FG.to_string());
            out.push_str(&format!(
                "<text x=\"{:.2}\" y=\"{:.2}\" fill=\"{fill}\" textLength=\"{CW:.2}\" \
                 lengthAdjust=\"spacingAndGlyphs\"{}>{}</text>\n",
                GRID_X + x as f64 * CW,
                by(y),
                modifier_attrs(cell.modifier),
                escape(sym),
            ));
        }
    }
    out.push_str("</svg>\n");
    out
}

/// (fg, bg) hex after applying REVERSED; `None` means use the terminal default.
fn effective(cell: &ratatui::buffer::Cell) -> (Option<String>, Option<String>) {
    let (mut fg, mut bg) = (cell.fg, cell.bg);
    if cell.modifier.contains(Modifier::REVERSED) {
        std::mem::swap(&mut fg, &mut bg);
        let fg_s = Some(hex(fg).unwrap_or_else(|| BG.to_string()));
        let bg_s = Some(hex(bg).unwrap_or_else(|| FG.to_string()));
        return (fg_s, bg_s);
    }
    (hex(fg), hex(bg))
}

fn modifier_attrs(m: Modifier) -> String {
    let mut s = String::new();
    if m.contains(Modifier::BOLD) {
        s.push_str(" font-weight=\"bold\"");
    }
    if m.contains(Modifier::ITALIC) {
        s.push_str(" font-style=\"italic\"");
    }
    if m.contains(Modifier::DIM) {
        s.push_str(" opacity=\"0.55\"");
    }
    let mut deco = Vec::new();
    if m.contains(Modifier::UNDERLINED) {
        deco.push("underline");
    }
    if m.contains(Modifier::CROSSED_OUT) {
        deco.push("line-through");
    }
    if !deco.is_empty() {
        s.push_str(&format!(" text-decoration=\"{}\"", deco.join(" ")));
    }
    s
}

fn hex(c: Color) -> Option<String> {
    let h = match c {
        Color::Reset => return None,
        Color::Black => "#1d1f2a",
        Color::Red => "#f38ba8",
        Color::Green => "#a6e3a1",
        Color::Yellow => "#f9e2af",
        Color::Blue => "#89b4fa",
        Color::Magenta => "#cba6f7",
        Color::Cyan => "#89dceb",
        Color::Gray => "#bac2de",
        Color::DarkGray => "#6c7086",
        Color::LightRed => "#eba0ac",
        Color::LightGreen => "#94e2d5",
        Color::LightYellow => "#f5e0dc",
        Color::LightBlue => "#74c7ec",
        Color::LightMagenta => "#b4befe",
        Color::LightCyan => "#b4f9f8",
        Color::White => "#cdd6f4",
        Color::Rgb(r, g, b) => return Some(format!("#{r:02x}{g:02x}{b:02x}")),
        Color::Indexed(_) => "#cdd6f4",
    };
    Some(h.to_string())
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
