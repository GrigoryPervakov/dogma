//! Syntax highlighting for code blocks via syntect → ratatui Spans.

use once_cell::sync::Lazy;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Style as SyntectStyle, ThemeSet};
use syntect::parsing::SyntaxSet;
use syntect::util::LinesWithEndings;

static SYNTAXES: Lazy<SyntaxSet> = Lazy::new(SyntaxSet::load_defaults_newlines);
static THEMES: Lazy<ThemeSet> = Lazy::new(ThemeSet::load_defaults);

const DEFAULT_THEME: &str = "base16-ocean.dark";

pub fn highlight_code<'a>(code: &'a str, lang: Option<&str>) -> Vec<Line<'a>> {
    let syntax = lang
        .and_then(|l| SYNTAXES.find_syntax_by_token(l))
        .or_else(|| SYNTAXES.find_syntax_by_first_line(code))
        .unwrap_or_else(|| SYNTAXES.find_syntax_plain_text());

    let theme = THEMES
        .themes
        .get(DEFAULT_THEME)
        .or_else(|| THEMES.themes.values().next())
        .expect("at least one default theme");

    let mut h = HighlightLines::new(syntax, theme);
    let mut out: Vec<Line<'a>> = Vec::new();

    for line in LinesWithEndings::from(code) {
        let ranges = match h.highlight_line(line, &SYNTAXES) {
            Ok(r) => r,
            Err(_) => {
                out.push(Line::from(line.trim_end_matches('\n').to_string()));
                continue;
            }
        };
        let spans: Vec<Span<'a>> = ranges
            .into_iter()
            .map(|(style, slice)| {
                let s = slice.trim_end_matches('\n');
                Span::styled(s.to_string(), syntect_to_rt(style))
            })
            .collect();
        out.push(Line::from(spans));
    }
    out
}

fn syntect_to_rt(s: SyntectStyle) -> Style {
    let mut style = Style::default().fg(Color::Rgb(s.foreground.r, s.foreground.g, s.foreground.b));
    if s.font_style.contains(FontStyle::BOLD) {
        style = style.add_modifier(ratatui::style::Modifier::BOLD);
    }
    if s.font_style.contains(FontStyle::ITALIC) {
        style = style.add_modifier(ratatui::style::Modifier::ITALIC);
    }
    if s.font_style.contains(FontStyle::UNDERLINE) {
        style = style.add_modifier(ratatui::style::Modifier::UNDERLINED);
    }
    style
}
