//! Shared list-and-detail layout used by the tasks/plans/skills tabs.
//!
//! All three show the same shape: a left-hand selectable list, a right-hand
//! detail pane that auto-fetches the highlighted item. The per-type bits
//! (model, endpoints, line/meta formatting) live in a [`ListDetailModel`]
//! impl; everything else — selection, fetch bookkeeping, key handling, and
//! painting — is generic here. `notifications` reuses the painter + `handle_nav`
//! but keeps its own state (answer/dismiss flow).

use std::collections::HashMap;
use std::marker::PhantomData;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};

use crate::api::types::HttpReq;
use crate::instance::InstanceId;
use crate::ui::markdown::render_markdown;
use crate::view::{View, ViewCtx, ViewRenderCtx};

pub use crate::ui::truncate;

#[derive(Debug, Clone, Copy)]
pub enum Pane {
    List,
    Detail,
}

/// A `key   value` metadata row for a detail pane, with the key padded to
/// `pad` columns and dimmed.
pub fn meta_line(key: &str, value: &str, pad: usize) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{key:<pad$} "),
            Style::default().add_modifier(Modifier::DIM),
        ),
        Span::raw(value.to_string()),
    ])
}

/// Result of feeding a key to the shared list navigation.
pub enum NavOutcome {
    /// Selection moved (or a jump key fired) — refetch detail if applicable.
    Moved,
    /// Pane focus toggled.
    Switched,
    /// Not a navigation key — the caller may handle it.
    Ignored,
}

/// Handle the navigation keys common to every list+detail view: up/down/jk,
/// g/G, and the List↔Detail pane toggle.
pub fn handle_nav(code: KeyCode, pane: &mut Pane, selected: &mut usize, len: usize) -> NavOutcome {
    match (code, &pane) {
        (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
            *selected = selected.saturating_sub(1);
            NavOutcome::Moved
        }
        (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
            if *selected + 1 < len {
                *selected += 1;
            }
            NavOutcome::Moved
        }
        (KeyCode::Char('g'), _) => {
            *selected = 0;
            NavOutcome::Moved
        }
        (KeyCode::Char('G'), _) => {
            *selected = len.saturating_sub(1);
            NavOutcome::Moved
        }
        (KeyCode::Right, Pane::List) | (KeyCode::Enter, Pane::List) => {
            *pane = Pane::Detail;
            NavOutcome::Switched
        }
        (KeyCode::Left, Pane::Detail) | (KeyCode::Esc, Pane::Detail) => {
            *pane = Pane::List;
            NavOutcome::Switched
        }
        _ => NavOutcome::Ignored,
    }
}

// ---------------------------------------------------------------------------
// Generic list+detail view
// ---------------------------------------------------------------------------

/// Per-type behaviour for a [`ListDetail`] tab. All methods are pure/static —
/// the generic owns the state.
pub trait ListDetailModel: 'static {
    type Item: Clone + 'static;

    const ID: &'static str;
    const TITLE: &'static str;
    const EMPTY_TITLE: &'static str;

    fn list_req() -> HttpReq;
    fn detail_req(id: &str) -> HttpReq;
    fn item_id(item: &Self::Item) -> &str;
    /// Which instance this item came from (for routing + badges).
    fn item_instance(item: &Self::Item) -> InstanceId;
    /// Stamp the origin instance at ingest.
    fn set_instance(item: &mut Self::Item, instance: InstanceId);
    fn list_line(item: &Self::Item) -> Line<'static>;
    fn detail_title(item: &Self::Item) -> &str;
    fn detail_meta(item: &Self::Item) -> Vec<Line<'static>>;
    fn detail_body(item: &Self::Item) -> Option<&str>;

    /// Whether the item is "active" (shown by default). Inactive items are
    /// hidden until the user toggles "show all" (`a`). Default: always active.
    fn is_active(_item: &Self::Item) -> bool {
        true
    }

    /// Sort key (a timestamp string) — the merged list is ordered by this,
    /// descending, so items interleave by time rather than grouping by
    /// instance. Empty sorts last. Default: no ordering.
    fn sort_key(_item: &Self::Item) -> String {
        String::new()
    }

    /// Map a key press on the selected item to an action request (e.g. approve
    /// a plan), routed to the item's instance. Default: no item actions.
    fn key_action(_item: &Self::Item, _key: KeyCode) -> Option<HttpReq> {
        None
    }

    /// Fold a freshly-fetched detail into what the list row already had (e.g.
    /// carry over stats the detail endpoint omits). Default: take the detail.
    fn merge_detail(_row: Option<&Self::Item>, detail: Self::Item) -> Self::Item {
        detail
    }
}

/// Detail/loading maps are keyed by `(instance, id)` since ids are only unique
/// within one instance.
type DetailKey = (InstanceId, String);

pub struct ListDetail<M: ListDetailModel> {
    items: Vec<M::Item>,
    detail: HashMap<DetailKey, M::Item>,
    selected: usize,
    focused_pane: Pane,
    loaded: bool,
    loading: bool,
    detail_loading: HashMap<DetailKey, bool>,
    last_error: Option<String>,
    /// When false (default), only `M::is_active` items are shown; `a` toggles.
    show_all: bool,
    _m: PhantomData<M>,
}

impl<M: ListDetailModel> Default for ListDetail<M> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M: ListDetailModel> ListDetail<M> {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            detail: HashMap::new(),
            selected: 0,
            focused_pane: Pane::List,
            loaded: false,
            loading: false,
            detail_loading: HashMap::new(),
            last_error: None,
            show_all: false,
            _m: PhantomData,
        }
    }

    /// The rows currently on screen: active-only unless `show_all`. Preserves
    /// the time-sorted order of `items`.
    fn shown(&self) -> Vec<&M::Item> {
        self.items
            .iter()
            .filter(|it| self.show_all || M::is_active(it))
            .collect()
    }

    /// The selected item, preferring the detail-fetched copy (it carries the
    /// `content` body) over the list row.
    fn current(&self) -> Option<&M::Item> {
        let shown = self.shown();
        let item = *shown.get(self.selected)?;
        let key = (M::item_instance(item), M::item_id(item).to_string());
        Some(self.detail.get(&key).unwrap_or(item))
    }

    fn ensure_detail_fetched(&mut self, ctx: &mut ViewCtx) {
        let key = match self.shown().get(self.selected) {
            Some(&item) => (M::item_instance(item), M::item_id(item).to_string()),
            None => return,
        };
        if self.detail.contains_key(&key) || self.detail_loading.get(&key).copied().unwrap_or(false)
        {
            return;
        }
        self.detail_loading.insert(key.clone(), true);
        ctx.http(key.0, M::detail_req(&key.1));
    }

    /// Re-clamp the selection after the visible set changes (filter toggle,
    /// list reload).
    fn clamp_selection(&mut self) {
        let n = self.shown().len();
        self.selected = self.selected.min(n.saturating_sub(1));
    }

    fn reload(&mut self, ctx: &mut ViewCtx) {
        self.loading = true;
        self.last_error = None;
        self.detail.clear();
        self.detail_loading.clear();
        ctx.http_all(M::list_req());
    }

    /// Merge a freshly-loaded list from one instance: replace that instance's
    /// rows, keep the others. Items are stamped with their origin instance.
    pub fn apply_list_loaded(
        &mut self,
        instance: InstanceId,
        r: std::result::Result<Vec<M::Item>, String>,
    ) {
        self.loading = false;
        match r {
            Ok(mut list) => {
                for it in &mut list {
                    M::set_instance(it, instance);
                }
                self.items.retain(|it| M::item_instance(it) != instance);
                self.items.append(&mut list);
                // Interleave by time (newest first) rather than grouping by
                // instance.
                self.items
                    .sort_by(|a, b| M::sort_key(b).cmp(&M::sort_key(a)));
                self.loaded = true;
                self.last_error = None;
                self.clamp_selection();
            }
            Err(e) => self.last_error = Some(e),
        }
    }

    pub fn apply_detail_loaded(
        &mut self,
        instance: InstanceId,
        id: &str,
        r: std::result::Result<M::Item, String>,
    ) {
        let key = (instance, id.to_string());
        self.detail_loading.remove(&key);
        match r {
            Ok(mut item) => {
                M::set_instance(&mut item, instance);
                let row = self
                    .items
                    .iter()
                    .find(|x| M::item_instance(x) == instance && M::item_id(x) == id);
                let merged = M::merge_detail(row, item);
                self.detail.insert(key, merged);
            }
            Err(e) => self.last_error = Some(format!("{}({id}): {e}", M::ID)),
        }
    }

    /// Note the result of an item action (approve/decline). Errors surface in
    /// the header; the caller refetches the list to reflect the new state.
    pub fn apply_action_result(&mut self, id: &str, result: std::result::Result<(), String>) {
        if let Err(e) = result {
            self.last_error = Some(format!("{}({id}): {e}", M::ID));
        }
    }
}

impl<M: ListDetailModel> View for ListDetail<M> {
    fn id(&self) -> &'static str {
        M::ID
    }
    fn title(&self) -> &str {
        M::ID
    }

    fn on_focus(&mut self, ctx: &mut ViewCtx) {
        if !self.loaded && !self.loading {
            self.loading = true;
            ctx.http_all(M::list_req());
        }
        self.ensure_detail_fetched(ctx);
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        let len = self.shown().len();
        match handle_nav(key.code, &mut self.focused_pane, &mut self.selected, len) {
            NavOutcome::Moved => self.ensure_detail_fetched(ctx),
            NavOutcome::Switched => {}
            NavOutcome::Ignored => match key.code {
                KeyCode::Char('r') => self.reload(ctx),
                // Toggle showing inactive (done/declined/etc.) items.
                KeyCode::Char('a') => {
                    self.show_all = !self.show_all;
                    self.clamp_selection();
                    self.ensure_detail_fetched(ctx);
                }
                // Model-specific action on the selected item (e.g. approve a
                // plan), routed to that item's instance.
                other => {
                    let act = self.shown().get(self.selected).copied().and_then(|it| {
                        M::key_action(it, other).map(|req| (M::item_instance(it), req))
                    });
                    if let Some((instance, req)) = act {
                        ctx.http(instance, req);
                    }
                }
            },
        }
    }

    fn render(&mut self, area: Rect, frame: &mut Frame, ctx: ViewRenderCtx<'_>) {
        // Prefix each row with a colored instance sigil when several instances
        // are connected, so merged resources stay visually distinguishable.
        let multi = ctx.multi_instance();
        let shown = self.shown();
        let hidden = self.items.len() - shown.len();
        let items: Vec<Line<'_>> = shown
            .iter()
            .map(|it| {
                let mut line = M::list_line(it);
                if multi {
                    let inst = M::item_instance(it);
                    let label = ctx.instance(inst).map(|m| m.label.as_str()).unwrap_or("");
                    line.spans.insert(
                        0,
                        Span::styled(
                            format!("{} {label} ", crate::ui::theme::instance_sigil(inst)),
                            Style::default().fg(crate::ui::theme::instance_color(inst)),
                        ),
                    );
                }
                line
            })
            .collect();
        let item = self.current();
        let detail_title = item.map(M::detail_title).unwrap_or(M::EMPTY_TITLE);
        let detail_meta = item.map(M::detail_meta).unwrap_or_default();
        let detail_body = item.and_then(M::detail_body);
        let detail_loading = item
            .map(|it| {
                self.detail_loading
                    .get(&(M::item_instance(it), M::item_id(it).to_string()))
                    .copied()
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        render(
            area,
            frame,
            ListDetailRender {
                title: M::TITLE,
                items,
                selected: self.selected,
                focused_pane: self.focused_pane,
                loading: self.loading,
                last_error: self.last_error.as_deref(),
                detail_title,
                detail_meta,
                detail_body,
                detail_loading,
                showing_all: self.show_all,
                hidden,
            },
        );
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// ---------------------------------------------------------------------------
// Painter
// ---------------------------------------------------------------------------

pub struct ListDetailRender<'a> {
    pub title: &'a str,
    pub items: Vec<Line<'a>>,
    pub selected: usize,
    pub focused_pane: Pane,
    pub loading: bool,
    pub last_error: Option<&'a str>,
    pub detail_title: &'a str,
    pub detail_meta: Vec<Line<'a>>,
    pub detail_body: Option<&'a str>,
    pub detail_loading: bool,
    /// Whether the "show all" filter is on (inactive items included).
    pub showing_all: bool,
    /// Count of items hidden by the active-only filter (0 when showing all).
    pub hidden: usize,
}

pub fn render(area: Rect, frame: &mut Frame, ctx: ListDetailRender<'_>) {
    let cols = Layout::horizontal([Constraint::Length(36), Constraint::Min(40)]).split(area);
    render_list(cols[0], frame, &ctx);
    render_detail(cols[1], frame, &ctx);
}

fn render_list(area: Rect, frame: &mut Frame, ctx: &ListDetailRender<'_>) {
    let active = matches!(ctx.focused_pane, Pane::List);

    let header_text = if ctx.loading {
        format!("{} · loading…", ctx.title)
    } else if let Some(e) = ctx.last_error {
        format!("{} · error: {}", ctx.title, truncate(e, 20))
    } else if ctx.showing_all {
        format!("{} ({}) · all · a", ctx.title, ctx.items.len())
    } else if ctx.hidden > 0 {
        format!(
            "{} ({}) · +{} hidden · a",
            ctx.title,
            ctx.items.len(),
            ctx.hidden
        )
    } else {
        format!("{} ({})", ctx.title, ctx.items.len())
    };
    let header_style = if active {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::DIM)
    };
    let layout = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).split(area);

    let header = Paragraph::new(Line::from(Span::styled(header_text, header_style))).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(if active {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default()
            }),
    );
    frame.render_widget(header, layout[0]);

    let items: Vec<ListItem> = ctx.items.iter().map(|l| ListItem::new(l.clone())).collect();

    let (highlight_style, highlight_symbol) = if active {
        (
            Style::default()
                .bg(Color::DarkGray)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            "▶ ",
        )
    } else {
        (
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
            "  ",
        )
    };

    let list = List::new(items)
        .highlight_style(highlight_style)
        .highlight_symbol(highlight_symbol)
        .block(
            Block::default()
                .borders(Borders::RIGHT)
                .border_style(if active {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                }),
        );

    let mut state = ListState::default();
    if !ctx.items.is_empty() {
        state.select(Some(ctx.selected.min(ctx.items.len() - 1)));
    }
    frame.render_stateful_widget(list, layout[1], &mut state);
}

fn render_detail(area: Rect, frame: &mut Frame, ctx: &ListDetailRender<'_>) {
    let active = matches!(ctx.focused_pane, Pane::Detail);

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Title
    lines.push(Line::from(Span::styled(
        format!("# {}", ctx.detail_title),
        Style::default().add_modifier(Modifier::BOLD).fg(if active {
            Color::Cyan
        } else {
            Color::White
        }),
    )));
    lines.push(Line::raw(""));

    // Metadata rows (e.g. "Status: pending", "Deadline: 2026-…").
    for meta_line in &ctx.detail_meta {
        let spans: Vec<Span<'static>> = meta_line
            .spans
            .iter()
            .map(|s| Span::styled(s.content.clone().into_owned(), s.style))
            .collect();
        lines.push(Line::from(spans));
    }
    if !ctx.detail_meta.is_empty() {
        lines.push(Line::raw(""));
    }

    // Body
    if ctx.detail_loading {
        lines.push(Line::from(Span::styled(
            "loading detail…",
            Style::default().add_modifier(Modifier::DIM),
        )));
    } else if let Some(body) = ctx.detail_body {
        lines.extend(render_markdown(body));
    } else if !ctx.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "(empty)",
            Style::default().add_modifier(Modifier::DIM),
        )));
    }

    let p = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(p, area);
}
