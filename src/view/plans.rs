//! Plans tab — read-only list + detail (see [`crate::view::list_detail`]).

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::api::types::HttpReq;
use crate::model::Plan;
use crate::view::list_detail::{ListDetail, ListDetailModel, meta_line, truncate};

pub type PlansView = ListDetail<PlanModel>;

pub struct PlanModel;

impl ListDetailModel for PlanModel {
    type Item = Plan;
    const ID: &'static str = "plans";
    const TITLE: &'static str = "Plans";
    const EMPTY_TITLE: &'static str = "(no plan selected)";

    fn list_req() -> HttpReq {
        HttpReq::ListPlans
    }
    fn detail_req(id: &str) -> HttpReq {
        HttpReq::GetPlan {
            plan_id: id.to_string(),
        }
    }
    fn item_id(p: &Plan) -> &str {
        &p.id
    }
    fn detail_title(p: &Plan) -> &str {
        p.title.as_deref().unwrap_or(if p.id.is_empty() {
            "(no plan)"
        } else {
            p.id.as_str()
        })
    }
    fn detail_body(p: &Plan) -> Option<&str> {
        p.content.as_deref()
    }

    fn list_line(p: &Plan) -> Line<'static> {
        let (glyph, glyph_style) = match p.status.as_str() {
            "approved" | "implementing" => ("✓", Style::default().fg(Color::Green)),
            "pending" | "proposed" => ("◷", Style::default().fg(Color::Yellow)),
            "declined" | "revision_requested" => ("⨯", Style::default().fg(Color::Red)),
            _ => ("•", Style::default().add_modifier(Modifier::DIM)),
        };
        let label = p.title.clone().unwrap_or_else(|| p.id.clone());
        Line::from(vec![
            Span::styled(glyph.to_string(), glyph_style),
            Span::raw(" "),
            Span::styled(truncate(&label, 32), Style::default().fg(Color::White)),
        ])
    }

    fn detail_meta(p: &Plan) -> Vec<Line<'static>> {
        let mut out = vec![meta_line("status", &p.status, 10)];
        if let Some(t) = p.task_id.as_deref() {
            out.push(meta_line("task", t, 10));
        }
        if let Some(r) = p.runtime.as_deref() {
            out.push(meta_line("runtime", r, 10));
        }
        if let Some(c) = p.created_at.as_deref() {
            out.push(meta_line("created", c, 10));
        }
        if let Some(f) = p.feedback.as_deref()
            && !f.is_empty()
        {
            out.push(meta_line("feedback", f, 10));
        }
        out
    }
}
