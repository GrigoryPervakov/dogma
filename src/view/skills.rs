//! Skills tab — read-only list + detail (see [`crate::view::list_detail`]).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::api::types::HttpReq;
use crate::model::Skill;
use crate::view::list_detail::{ListDetail, ListDetailModel, meta_line, truncate};

pub type SkillsView = ListDetail<SkillModel>;

pub struct SkillModel;

impl ListDetailModel for SkillModel {
    type Item = Skill;
    const ID: &'static str = "skills";
    const TITLE: &'static str = "Skills";
    const EMPTY_TITLE: &'static str = "(no skill)";

    fn list_req() -> HttpReq {
        HttpReq::ListSkills
    }
    fn detail_req(id: &str) -> HttpReq {
        HttpReq::GetSkill {
            skill_id: id.to_string(),
        }
    }
    fn item_id(s: &Skill) -> &str {
        &s.id
    }
    fn detail_title(s: &Skill) -> &str {
        &s.name
    }
    fn detail_body(s: &Skill) -> Option<&str> {
        s.content.as_deref()
    }

    /// The detail endpoint nests usage stats under `stats`, so the flat
    /// usage_count/last_used_at arrive empty — carry them from the list row.
    fn merge_detail(row: Option<&Skill>, mut s: Skill) -> Skill {
        if let Some(row) = row {
            s.usage_count = s.usage_count.or(row.usage_count);
            s.last_used_at = s.last_used_at.clone().or_else(|| row.last_used_at.clone());
        }
        s
    }

    fn list_line(s: &Skill) -> Line<'static> {
        let glyph = if s.enabled { "●" } else { "○" };
        let glyph_style = if s.enabled {
            Style::default().fg(Color::Green)
        } else {
            Style::default().add_modifier(Modifier::DIM)
        };
        Line::from(vec![
            Span::styled(glyph.to_string(), glyph_style),
            Span::raw(" "),
            Span::styled(truncate(&s.name, 32), Style::default().fg(Color::White)),
        ])
    }

    fn detail_meta(s: &Skill) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        if let Some(d) = s.description.as_deref()
            && !d.is_empty()
        {
            out.push(meta_line("description", d, 12));
        }
        if let Some(v) = s.version.as_deref() {
            out.push(meta_line("version", v, 12));
        }
        out.push(meta_line(
            "enabled",
            if s.enabled { "yes" } else { "no" },
            12,
        ));
        if let Some(u) = s.usage_count {
            out.push(meta_line("usage", &u.to_string(), 12));
        }
        if let Some(t) = s.last_used_at.as_deref() {
            out.push(meta_line("last used", t, 12));
        }
        out
    }
}
