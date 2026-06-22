//! ChatView state — focus tiers, draft registry, sessions list, history,
//! streaming reducer.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::ui::textarea::TextArea;

use crate::api::types::{HttpReq, MessagesPayload, WsClientMsg, WsServerMsg};
use crate::app::action::Action;
use crate::model::{Block, ContextUsage, Message, Session};
use crate::view::chat::poll::{Poll, PollUiState};
use crate::view::{View, ViewCtx, ViewRenderCtx};

// ---------------------------------------------------------------------------
// Focus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusTier {
    Sessions,
    ChatBlocks,
    BlockInterior,
    Input,
    Insert,
    /// Answering a pending `AskUserQuestion` poll.
    Poll,
}

// ---------------------------------------------------------------------------
// Drafts
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionKey {
    Real(String),
    NewChat,
}

#[derive(Default, Debug)]
pub struct DraftRegistry {
    drafts: HashMap<SessionKey, String>,
}

impl DraftRegistry {
    pub fn get(&self, k: &SessionKey) -> &str {
        self.drafts.get(k).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn set(&mut self, k: SessionKey, value: String) {
        if value.is_empty() {
            self.drafts.remove(&k);
        } else {
            self.drafts.insert(k, value);
        }
    }

    pub fn clear(&mut self, k: &SessionKey) {
        self.drafts.remove(k);
    }

    pub fn has(&self, k: &SessionKey) -> bool {
        self.drafts.get(k).map(|s| !s.is_empty()).unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Per-session UI state — survives navigation.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct PerSessionUi {
    pub selected_block: Option<usize>,
    pub follow_tail: bool,
    /// Index of the top-most visible block (0 = oldest in `history`).
    pub viewport_top: usize,
    /// Cursor inside the currently-selected block, used at BlockInterior tier.
    pub block_cursor: BlockCursor,
    /// Item count as of the last time the user was at the tail. Lets re-entry
    /// detect blocks added since the user scrolled up (jump) vs none (stay put).
    pub seen_block_count: usize,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BlockCursor {
    /// Logical line index inside the block's rendered Lines.
    pub line: usize,
    /// First visible logical line (scroll offset).
    pub scroll: usize,
}

/// Tracks the "load older messages" state for a session. Pagination on
/// `/api/sessions/{id}/messages` is `limit`-only (no offset/before-id), so
/// "load older" = refetch with a bigger limit and replace history.
#[derive(Debug, Default, Clone)]
pub struct HistoryLoadState {
    /// Last requested `limit` (== ceiling of how many messages we've asked for).
    pub limit: u32,
    /// Server returned fewer than we asked for → we've got everything.
    pub exhausted: bool,
    /// A request is in flight; suppress duplicate triggers.
    pub loading: bool,
}

/// Source values that classify a session as system-managed (cron / hook).
pub fn is_system_session(s: &Session) -> bool {
    matches!(s.source.as_deref(), Some("cron") | Some("hook"))
}

/// Initial limit we ask for when entering a session for the first time.
pub const INITIAL_HISTORY_LIMIT: u32 = 200;
/// How much to grow the requested limit on each "load older" trigger.
pub const HISTORY_PAGE_SIZE: u32 = 500;

/// Cap on a single card's height in the compact list: half the window, floored
/// so short terminals still show a few lines. Shared by the renderer and the
/// viewport math so their height estimates agree.
pub fn block_height_cap(window_h: u16) -> u16 {
    (window_h / 2).max(6)
}

/// Walk forward from `top`, accumulating per-item heights, until the
/// viewport is filled. Returns the index of the last item that fits.
fn compute_last_visible(
    items: &[super::items::ChatItem<'_>],
    area: Rect,
    top: usize,
    can_load: bool,
    loading: bool,
) -> usize {
    if items.is_empty() {
        return 0;
    }
    let hint_h = if (can_load || loading) && top == 0 {
        1
    } else {
        0
    };
    let avail = area.height.saturating_sub(hint_h) as u32;
    let inner_w = area.width.saturating_sub(2).max(1);
    let max_block_h = block_height_cap(area.height);
    let mut used: u32 = 0;
    let mut last = top;
    for (i, item) in items.iter().enumerate().skip(top) {
        let h = super::blocks_height_estimator::item_height(item, inner_w, max_block_h) as u32;
        if used + h > avail {
            if i == top {
                last = i;
            }
            break;
        }
        used += h;
        last = i;
    }
    last
}

/// Walk backward from `bottom`, accumulating heights, returning the smallest
/// `top` such that `bottom` is still in the rendered range.
fn compute_top_for_bottom(
    items: &[super::items::ChatItem<'_>],
    area: Rect,
    bottom: usize,
    _can_load: bool,
    loading: bool,
) -> usize {
    if items.is_empty() {
        return 0;
    }
    let hint_h = if loading { 1 } else { 0 };
    let avail = area.height.saturating_sub(hint_h) as u32;
    let inner_w = area.width.saturating_sub(2).max(1);
    let max_block_h = block_height_cap(area.height);
    let mut used: u32 = 0;
    let mut top;
    let mut i = bottom;
    loop {
        let h = super::blocks_height_estimator::item_height(&items[i], inner_w, max_block_h) as u32;
        if used + h > avail {
            top = i + 1;
            break;
        }
        used += h;
        top = i;
        if i == 0 {
            break;
        }
        i -= 1;
    }
    top.min(items.len() - 1)
}

// ---------------------------------------------------------------------------
// Side panel — sub-agents
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
pub struct SubAgentPanel {
    pub tool_use_id: String,
    pub kind: String, // "Plan" | "Agent" | "Explore" | ...
    pub description: String,
    pub blocks: Vec<Block>,
    pub running: bool,
}

#[derive(Debug, Default)]
pub struct SidePanelState {
    pub panels: Vec<SubAgentPanel>,
    pub visible: bool,
}

// ---------------------------------------------------------------------------
// Pending interaction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PendingInteraction {
    pub session_id: String,
    pub interaction_id: String,
    pub interaction_type: String,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Agent status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub enum AgentStatus {
    #[default]
    Idle,
    Thinking,
    Writing,
    Tool(String),
}

/// Coarse per-session runtime state used for sidebar coloring and the
/// streaming chat frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionRuntime {
    Idle,
    Streaming,
    WaitingPoll,
}

// ---------------------------------------------------------------------------
// ChatView
// ---------------------------------------------------------------------------

pub struct ChatView {
    pub focus: FocusTier,

    // Sessions
    pub sessions: Vec<Session>,
    pub sessions_selected: usize, // 0 == "+ new chat"
    pub sidebar_visible: bool,
    pub sidebar_search: String,
    pub sidebar_search_active: bool,

    // Active session
    pub current: SessionKey,

    // Chat state per session (history + streaming buffer + UI bits)
    pub history: HashMap<String, Vec<Message>>,
    pub streaming: HashMap<String, Message>,
    pub agent_status: HashMap<String, AgentStatus>,
    pub ui: HashMap<SessionKey, PerSessionUi>,

    // Drafts per session
    pub drafts: DraftRegistry,
    pub input: TextArea,

    // Side panel
    pub side_panel: SidePanelState,

    // Pending interaction (one at a time, per session)
    pub pending_interaction: HashMap<String, PendingInteraction>,

    // Selection state for the current session's focused poll, if any.
    pub poll_ui: Option<PollUiState>,

    // Interaction ids already answered/denied — guards against a resolved
    // poll being resurrected when buffered events replay on session re-entry.
    pub answered_interactions: std::collections::HashSet<String>,

    // run_in_background jobs per session (from BackgroundTasksUpdate WS events).
    pub background_tasks: HashMap<String, Vec<serde_json::Value>>,

    // Context usage per session
    pub context: HashMap<String, ContextUsage>,

    // Sessions request flight
    pub sessions_loaded: bool,
    pub last_error: Option<String>,

    // History pagination state per session.
    pub history_load: HashMap<String, HistoryLoadState>,
}

impl Default for ChatView {
    fn default() -> Self {
        Self::new()
    }
}

impl ChatView {
    pub fn new() -> Self {
        let mut input = TextArea::default();
        input.set_cursor_line_style(ratatui::style::Style::default());
        Self {
            focus: FocusTier::Sessions,
            sessions: Vec::new(),
            sessions_selected: 0,
            sidebar_visible: true,
            sidebar_search: String::new(),
            sidebar_search_active: false,
            current: SessionKey::NewChat,
            history: HashMap::new(),
            streaming: HashMap::new(),
            agent_status: HashMap::new(),
            ui: HashMap::new(),
            drafts: DraftRegistry::default(),
            input,
            side_panel: SidePanelState::default(),
            pending_interaction: HashMap::new(),
            poll_ui: None,
            answered_interactions: std::collections::HashSet::new(),
            background_tasks: HashMap::new(),
            context: HashMap::new(),
            sessions_loaded: false,
            last_error: None,
            history_load: HashMap::new(),
        }
    }

    pub fn current_session_id(&self) -> Option<&str> {
        match &self.current {
            SessionKey::Real(s) => Some(s),
            SessionKey::NewChat => None,
        }
    }

    /// Coarse runtime state of a session: waiting on a poll, actively
    /// streaming, or idle.
    pub fn session_runtime(&self, session_id: &str) -> SessionRuntime {
        let waiting = self
            .pending_interaction
            .get(session_id)
            .map(|p| p.interaction_type == "question")
            .unwrap_or(false);
        if waiting {
            return SessionRuntime::WaitingPoll;
        }
        let running = self
            .sessions
            .iter()
            .find(|s| s.id == session_id)
            .map(|s| s.is_running)
            .unwrap_or(false);
        let streaming = running
            || self.streaming.contains_key(session_id)
            || !matches!(
                self.agent_status.get(session_id),
                None | Some(AgentStatus::Idle)
            );
        if streaming {
            SessionRuntime::Streaming
        } else {
            SessionRuntime::Idle
        }
    }

    /// Whether the currently-viewed session is actively streaming — gates the
    /// compact (collapsed + typed-header) view off and the green frame on.
    pub fn is_current_streaming(&self) -> bool {
        self.current_session_id()
            .map(|id| matches!(self.session_runtime(id), SessionRuntime::Streaming))
            .unwrap_or(false)
    }

    pub fn current_history(&self) -> &[Message] {
        match &self.current {
            SessionKey::Real(id) => self.history.get(id).map(|v| v.as_slice()).unwrap_or(&[]),
            SessionKey::NewChat => &[],
        }
    }

    pub fn current_streaming(&self) -> Option<&Message> {
        match &self.current {
            SessionKey::Real(id) => self.streaming.get(id),
            SessionKey::NewChat => None,
        }
    }

    pub fn current_agent_status(&self) -> &AgentStatus {
        static IDLE: AgentStatus = AgentStatus::Idle;
        match &self.current {
            SessionKey::Real(id) => self.agent_status.get(id).unwrap_or(&IDLE),
            SessionKey::NewChat => &IDLE,
        }
    }

    pub fn ui_state(&self, key: &SessionKey) -> PerSessionUi {
        self.ui.get(key).cloned().unwrap_or_default()
    }

    fn ui_mut(&mut self, key: &SessionKey) -> &mut PerSessionUi {
        self.ui.entry(key.clone()).or_default()
    }

    pub fn current_pending(&self) -> Option<&PendingInteraction> {
        match &self.current {
            SessionKey::Real(id) => self.pending_interaction.get(id),
            SessionKey::NewChat => None,
        }
    }

    pub fn current_context(&self) -> Option<&ContextUsage> {
        match &self.current {
            SessionKey::Real(id) => self.context.get(id),
            SessionKey::NewChat => None,
        }
    }

    /// Cumulative cost in USD for the current session (from the sessions
    /// list). Returns 0.0 if unknown.
    pub fn current_cost_usd(&self) -> f64 {
        match &self.current {
            SessionKey::Real(id) => self
                .sessions
                .iter()
                .find(|s| &s.id == id)
                .map(|s| s.total_cost_usd)
                .unwrap_or(0.0),
            SessionKey::NewChat => 0.0,
        }
    }

    pub fn session_count_total(&self) -> usize {
        // +1 for the new-chat row.
        self.sessions.len() + 1
    }

    /// Returns Some(&Session) for an index in the sidebar, or None for the
    /// new-chat pseudo-row.
    pub fn session_at(&self, idx: usize) -> Option<&Session> {
        if idx == 0 {
            None
        } else {
            self.sessions.get(idx - 1)
        }
    }

    fn key_at(&self, idx: usize) -> SessionKey {
        match self.session_at(idx) {
            None => SessionKey::NewChat,
            Some(s) => SessionKey::Real(s.id.clone()),
        }
    }

    fn save_draft(&mut self) {
        let v = self.input.lines().join("\n");
        self.drafts.set(self.current.clone(), v);
    }

    fn load_draft(&mut self) {
        let text = self.drafts.get(&self.current).to_string();
        let lines: Vec<String> = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(|s| s.to_string()).collect()
        };
        self.input = TextArea::new(lines);
        self.input
            .set_cursor_line_style(ratatui::style::Style::default());
    }

    fn switch_to(&mut self, key: SessionKey) {
        self.save_draft();
        self.current = key.clone();
        self.load_draft();

        match key {
            SessionKey::Real(ref id) => {
                // Send switch_session over WS so the daemon routes future
                // events to us. Actual buffered_events come back in
                // session_status.
                // (Push via the ctx in run_command / handle_key paths.)
                let _ = id; // queued by callers
            }
            SessionKey::NewChat => { /* nothing remote yet */ }
        }
        // Keep the poll card consistent with the now-current session (no
        // focus steal — sidebar preview must stay on the sidebar).
        self.sync_poll();
    }

    // ----- public command surface (driven from `:foo` parser) ---------------

    pub fn run_command(&mut self, cmd: ChatCommand) -> Vec<Action> {
        let mut out = Vec::new();
        match cmd {
            ChatCommand::NewChat(_title) => {
                self.sessions_selected = 0;
                self.switch_to(SessionKey::NewChat);
                self.focus = FocusTier::Insert;
            }
            ChatCommand::Fork(title) => {
                if let Some(id) = self.current_session_id().map(str::to_string) {
                    out.push(Action::Ws(WsClientMsg::Fork {
                        session_id: id,
                        at_message_id: None,
                        title,
                    }));
                }
            }
            ChatCommand::Resume => {
                if let Some(id) = self.current_session_id().map(str::to_string) {
                    out.push(Action::Ws(WsClientMsg::Resume { session_id: id }));
                }
            }
            ChatCommand::Rename(_) => { /* TODO PATCH /api/sessions/{id} */ }
            ChatCommand::Delete => { /* TODO DELETE /api/sessions/{id} */ }
        }
        out
    }

    // ----- HTTP results -----------------------------------------------------

    pub fn apply_sessions_loaded(
        &mut self,
        result: std::result::Result<Vec<Session>, String>,
        _ctx: &mut ViewCtx,
    ) {
        match result {
            Ok(list) => {
                // Partition: user-source sessions first, system-source
                // (cron / hook) below. Stable within each group preserves
                // the server's updated_at-DESC order. Mirrors
                // web/src/components/Chat/SessionSidebar.tsx.
                let (user, system): (Vec<_>, Vec<_>) =
                    list.into_iter().partition(|s| !is_system_session(s));
                let mut combined = user;
                combined.extend(system);
                self.sessions = combined;
                self.sessions_loaded = true;
                self.last_error = None;
            }
            Err(e) => {
                self.last_error = Some(format!("sessions: {e}"));
            }
        }
    }

    /// Index of the first system-source session, if any. Used by the
    /// sidebar to insert a section divider.
    pub fn first_system_session_idx(&self) -> Option<usize> {
        self.sessions.iter().position(is_system_session)
    }

    pub fn apply_messages_loaded(
        &mut self,
        session_id: &str,
        limit: u32,
        result: std::result::Result<MessagesPayload, String>,
        _ctx: &mut ViewCtx,
    ) {
        match result {
            Ok(payload) => {
                let MessagesPayload {
                    messages,
                    last_usage,
                } = payload;
                let new_len = messages.len();
                let old_items: usize = self
                    .history
                    .get(session_id)
                    .map(|h| h.iter().map(|m| m.blocks.len()).sum::<usize>())
                    .unwrap_or(0);
                let new_items: usize = messages.iter().map(|m| m.blocks.len()).sum();
                let is_initial_load = old_items == 0;

                self.history.insert(session_id.to_string(), messages);
                let key = SessionKey::Real(session_id.to_string());

                // History-load bookkeeping.
                let entry = self.history_load.entry(session_id.to_string()).or_default();
                entry.limit = limit;
                entry.loading = false;
                // Server returned < limit → we've got everything.
                entry.exhausted = (new_len as u32) < limit;

                // Wire context usage from the messages-endpoint response.
                // last_usage nests max_context_tokens + num_turns inside the
                // Usage object (vs. WS Done where they're top-level).
                let ctx_entry = self.context.entry(session_id.to_string()).or_default();
                if let Some(u) = last_usage {
                    if let Some(m) = u.max_context_tokens {
                        ctx_entry.max_context_tokens = Some(m);
                    }
                    if let Some(n) = u.num_turns {
                        ctx_entry.num_turns = Some(n);
                    }
                    ctx_entry.last = Some(u);
                }
                // Default max_context_tokens for Claude models if we don't
                // know it yet (the WS Done event delivers a precise value).
                if ctx_entry.max_context_tokens.is_none() {
                    ctx_entry.max_context_tokens = Some(200_000);
                }

                let ui = self.ui_mut(&key);
                if is_initial_load {
                    // First load: land on the latest message and follow the tail.
                    if new_items > 0 {
                        ui.selected_block = Some(new_items - 1);
                    }
                    ui.follow_tail = true;
                    ui.viewport_top = 0;
                    ui.seen_block_count = new_items;
                } else {
                    // Pagination: older messages prepended. Shift selection
                    // + viewport_top in *items*, since each message
                    // contributes a variable number of selectable items.
                    let delta = new_items.saturating_sub(old_items);
                    if let Some(s) = ui.selected_block {
                        ui.selected_block = Some(s + delta);
                    }
                    ui.viewport_top += delta;
                }
                self.last_error = None;
            }
            Err(e) => {
                if let Some(s) = self.history_load.get_mut(session_id) {
                    s.loading = false;
                }
                self.last_error = Some(format!("history({session_id}): {e}"));
            }
        }
    }

    /// Returns true if there might be more older messages on the server.
    pub fn can_load_more(&self, session_id: &str) -> bool {
        match self.history_load.get(session_id) {
            Some(s) => !s.exhausted && !s.loading,
            // No load record yet → we haven't loaded anything; assume yes.
            None => true,
        }
    }

    /// True while a history fetch is in flight for the current session.
    pub fn is_loading_history(&self) -> bool {
        match &self.current {
            SessionKey::Real(id) => self
                .history_load
                .get(id)
                .map(|s| s.loading)
                .unwrap_or(false),
            SessionKey::NewChat => false,
        }
    }

    /// Mutable access to the current session's PerSessionUi for in-render
    /// adjustments. Returns None for the new-chat slot.
    pub fn ui_mut_for_current(&mut self) -> Option<&mut PerSessionUi> {
        let key = self.current.clone();
        Some(self.ui.entry(key).or_default())
    }

    /// Make sure the selected item is visible by adjusting `viewport_top`
    /// of the current session.
    pub fn adjust_viewport(&mut self, area: Rect) {
        // Compute the item count + the new viewport_top against borrowed
        // history/streaming, then reborrow `ui` mutably to write the result.
        let key = self.current.clone();
        let new_top = {
            let history = self.current_history();
            let streaming = self.current_streaming();
            let items = super::items::flatten(history, streaming);
            let total = items.len();
            if total == 0 {
                return;
            }
            let ui = self.ui.get(&key).cloned().unwrap_or_default();
            let selected = ui.selected_block.unwrap_or(total - 1).min(total - 1);
            let prev_top = ui.viewport_top.min(total - 1);
            let follow_tail = ui.follow_tail;

            let id_for_hint = match &key {
                SessionKey::Real(id) => Some(id.clone()),
                SessionKey::NewChat => None,
            };
            let can_load = id_for_hint
                .as_deref()
                .map(|id| {
                    self.history_load
                        .get(id)
                        .map(|s| !s.exhausted && !s.loading)
                        .unwrap_or(true)
                })
                .unwrap_or(false);
            let loading = id_for_hint
                .as_deref()
                .map(|id| {
                    self.history_load
                        .get(id)
                        .map(|s| s.loading)
                        .unwrap_or(false)
                })
                .unwrap_or(false);

            if follow_tail {
                compute_top_for_bottom(&items, area, total - 1, can_load, loading)
            } else if selected < prev_top {
                selected
            } else {
                let last_visible = compute_last_visible(&items, area, prev_top, can_load, loading);
                if selected > last_visible {
                    compute_top_for_bottom(&items, area, selected, can_load, loading)
                } else {
                    prev_top
                }
            }
        };

        let ui = self.ui.entry(key).or_default();
        ui.viewport_top = new_top;
    }

    pub fn apply_session_created(
        &mut self,
        pending_content: Option<String>,
        result: std::result::Result<Session, String>,
        ctx: &mut ViewCtx,
    ) {
        match result {
            Ok(session) => {
                let id = session.id.clone();
                // Insert at top of live group.
                self.sessions.insert(0, session);
                // Switch active session.
                self.current = SessionKey::Real(id.clone());
                self.drafts.clear(&SessionKey::NewChat);
                // Sidebar selection now points to the new session (idx 1).
                self.sessions_selected = 1;
                // SwitchSession over WS.
                ctx.push(Action::Ws(WsClientMsg::SwitchSession {
                    session_id: id.clone(),
                }));
                // Send pending content as the first message.
                if let Some(content) = pending_content
                    && !content.is_empty()
                {
                    ctx.push(Action::Ws(WsClientMsg::Message {
                        session_id: id.clone(),
                        content: content.clone(),
                        file_ids: None,
                    }));
                    // Optimistically append the user message to history.
                    self.history
                        .entry(id.clone())
                        .or_default()
                        .push(Message::new_user(id.clone(), content));
                    // Follow-tail engaged.
                    let key = SessionKey::Real(id.clone());
                    let last_idx = self.items_count_for(&id).saturating_sub(1);
                    let ui = self.ui_mut(&key);
                    ui.follow_tail = true;
                    ui.selected_block = Some(last_idx);
                }
                // Clear input, focus → ChatBlocks (follow tail mode).
                self.input = TextArea::default();
                self.input
                    .set_cursor_line_style(ratatui::style::Style::default());
                self.focus = FocusTier::ChatBlocks;
            }
            Err(e) => {
                self.last_error = Some(format!("create session: {e}"));
                // Do NOT consume draft; restore input contents.
                if let Some(content) = pending_content {
                    let lines: Vec<String> = content.split('\n').map(|s| s.to_string()).collect();
                    self.input = TextArea::new(lines);
                    self.input
                        .set_cursor_line_style(ratatui::style::Style::default());
                }
                self.focus = FocusTier::Insert;
            }
        }
    }

    // ----- WS streaming reducer --------------------------------------------

    pub fn apply_wire(&mut self, msg: &WsServerMsg, ctx: &mut ViewCtx) {
        super::reducer::apply(self, msg, ctx);
    }
}

impl ChatView {
    /// Preview the session at sidebar `idx`: switch the displayed session
    /// and lazy-load its history, but keep focus on the sidebar.
    /// Used for arrow-walking the sidebar. No WS `switch_session` —
    /// streaming sessions only resync once the user actually enters.
    fn preview_session(&mut self, idx: usize, ctx: &mut ViewCtx) {
        self.sessions_selected = idx;
        let key = self.key_at(idx);
        self.switch_to(key.clone());
        if let SessionKey::Real(id) = &key {
            self.ensure_history_fetch(id, ctx);
        }
    }

    /// Make the session at sidebar `idx` active: preview + WS switch_session
    /// + focus push.
    fn enter_session(&mut self, idx: usize, ctx: &mut ViewCtx) {
        self.sessions_selected = idx;
        let key = self.key_at(idx);
        self.switch_to(key.clone());

        match &key {
            SessionKey::Real(id) => {
                ctx.push(Action::Ws(WsClientMsg::SwitchSession {
                    session_id: id.clone(),
                }));
                self.ensure_history_fetch(id, ctx);
                // Existing chats always land in ChatBlocks — only the
                // new-chat row jumps to Insert. A session waiting on a poll
                // jumps straight to answering it.
                self.focus = FocusTier::ChatBlocks;
                // Cached session: jump to the latest unless the user is pinned
                // up-thread. (Uncached: handled on history load.)
                self.scroll_to_latest_unless_pinned(id);
                self.focus_poll_if_pending();
            }
            SessionKey::NewChat => {
                self.focus = FocusTier::Insert;
            }
        }
    }

    fn ensure_history_fetch(&mut self, id: &str, ctx: &mut ViewCtx) {
        if self.history.contains_key(id) {
            return;
        }
        ctx.push(Action::Http(HttpReq::GetMessages {
            session_id: id.to_string(),
            limit: INITIAL_HISTORY_LIMIT,
        }));
        let entry = self.history_load.entry(id.to_string()).or_default();
        entry.limit = INITIAL_HISTORY_LIMIT;
        entry.loading = true;
        entry.exhausted = false;
    }

    fn pop_to_sessions(&mut self) {
        // Auto-restore sidebar (per §9.7).
        self.sidebar_visible = true;
        self.focus = FocusTier::Sessions;
    }

    fn jump_to_input(&mut self) {
        self.focus = FocusTier::Input;
    }

    fn enter_insert(&mut self) {
        self.focus = FocusTier::Insert;
    }

    // ---- Pending poll (AskUserQuestion) --------------------------------

    /// The agent's current task list (TaskCreate/Update/List), reconstructed
    /// from the active session's tool-call blocks.
    pub fn current_tasks(&self) -> Vec<super::tasks_panel::CcTask> {
        super::tasks_panel::extract(self.current_history(), self.current_streaming())
    }

    /// run_in_background jobs for the active session.
    pub fn current_background(&self) -> &[serde_json::Value] {
        self.current_session_id()
            .and_then(|id| self.background_tasks.get(id))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The poll the user can answer right now: a pending `question`
    /// interaction for the current session whose id matches `poll_ui`.
    pub fn active_poll(&self) -> Option<&PollUiState> {
        let sid = self.current_session_id()?;
        let pending = self.pending_interaction.get(sid)?;
        let ui = self.poll_ui.as_ref()?;
        (pending.interaction_type == "question" && pending.interaction_id == ui.interaction_id)
            .then_some(ui)
    }

    /// Reconcile `poll_ui` with the current session's pending interaction:
    /// (re)build selection state for a waiting question, tear it down when
    /// none remains. Does NOT change focus — callers decide whether to grab
    /// it (live arrival / explicit enter) or leave it (sidebar preview).
    pub fn sync_poll(&mut self) {
        let pending = self
            .current_session_id()
            .and_then(|sid| self.pending_interaction.get(sid))
            .filter(|p| p.interaction_type == "question")
            .cloned()
            .filter(|p| !self.answered_interactions.contains(&p.interaction_id));
        match pending {
            Some(p) => {
                let stale = self
                    .poll_ui
                    .as_ref()
                    .map(|u| u.interaction_id != p.interaction_id)
                    .unwrap_or(true);
                if stale {
                    self.poll_ui = Poll::from_tool_input(&p.tool_input)
                        .map(|poll| PollUiState::new(p.interaction_id.clone(), poll));
                }
            }
            None => {
                self.poll_ui = None;
                if matches!(self.focus, FocusTier::Poll) {
                    self.focus = FocusTier::Input;
                }
            }
        }
    }

    /// Move focus to the pending poll if one is waiting for this session.
    fn focus_poll_if_pending(&mut self) {
        if self.active_poll().is_some() {
            self.focus = FocusTier::Poll;
        }
    }

    fn clear_poll(&mut self, session_id: &str) {
        self.pending_interaction.remove(session_id);
        self.poll_ui = None;
        self.focus = FocusTier::ChatBlocks;
        self.jump_last();
    }

    fn submit_poll(&mut self, ctx: &mut ViewCtx) {
        let Some(ui) = self.poll_ui.as_ref() else {
            return;
        };
        let Some(sid) = self.current_session_id().map(str::to_string) else {
            return;
        };
        let interaction_id = ui.interaction_id.clone();
        let result = ui.build_answers();
        self.answered_interactions.insert(interaction_id.clone());
        ctx.push(Action::Ws(WsClientMsg::AnswerInteraction {
            session_id: sid.clone(),
            interaction_id,
            result: Some(result),
            denied: false,
            message: None,
        }));
        self.clear_poll(&sid);
    }

    fn deny_poll(&mut self, ctx: &mut ViewCtx) {
        let Some(ui) = self.poll_ui.as_ref() else {
            return;
        };
        let Some(sid) = self.current_session_id().map(str::to_string) else {
            return;
        };
        let interaction_id = ui.interaction_id.clone();
        self.answered_interactions.insert(interaction_id.clone());
        ctx.push(Action::Ws(WsClientMsg::AnswerInteraction {
            session_id: sid.clone(),
            interaction_id,
            result: None,
            denied: true,
            message: Some("Skipped by user.".into()),
        }));
        self.clear_poll(&sid);
    }

    fn keys_poll(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        if self.poll_ui.is_none() {
            self.focus = FocusTier::Input;
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                self.poll_ui.as_mut().unwrap().move_option(-1);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                self.poll_ui.as_mut().unwrap().move_option(1);
            }
            (KeyCode::Right, _) | (KeyCode::Char('l'), _) => {
                self.poll_ui.as_mut().unwrap().move_question(1);
            }
            (KeyCode::Left, _) | (KeyCode::Char('h'), _) => {
                self.poll_ui.as_mut().unwrap().move_question(-1);
            }
            (KeyCode::Char(' '), _) => {
                self.poll_ui.as_mut().unwrap().select_current();
            }
            (KeyCode::Enter, _) => {
                let ui = self.poll_ui.as_mut().unwrap();
                let qi = ui.cursor_q;
                if !ui.poll.questions[qi].multi_select && !ui.question_answered(qi) {
                    ui.select_current();
                }
                if ui.is_single_simple() || ui.all_answered() {
                    self.submit_poll(ctx);
                } else {
                    ui.move_question(1);
                }
            }
            (KeyCode::Esc, _) => self.deny_poll(ctx),
            _ => {}
        }
    }

    fn enter_block(&mut self) {
        // Cursor was already reset when the user changed selection (in
        // `select_block`). Re-entering the same block keeps its position.
        self.focus = FocusTier::BlockInterior;
    }

    fn pop_from_block(&mut self) {
        self.focus = FocusTier::ChatBlocks;
    }

    fn block_count(&self) -> usize {
        super::items::count(self.current_history(), self.current_streaming())
    }

    /// Total chat items (per-block count) for a given session id.
    fn items_count_for(&self, session_id: &str) -> usize {
        let h_count = self
            .history
            .get(session_id)
            .map(|h| h.iter().map(|m| m.blocks.len()).sum::<usize>())
            .unwrap_or(0);
        let s_count = self
            .streaming
            .get(session_id)
            .map(|m| m.blocks.len())
            .unwrap_or(0);
        h_count + s_count
    }

    fn select_block(&mut self, idx: Option<usize>) {
        let key = self.current.clone();
        let ui = self.ui_mut(&key);
        // Reset the block-internal cursor only when the selected block
        // actually changes — re-entering the same block via Esc → Enter
        // restores the user's previous position.
        if ui.selected_block != idx {
            ui.block_cursor = BlockCursor::default();
        }
        ui.selected_block = idx;
        ui.follow_tail = false;
    }

    fn move_selection(&mut self, delta: isize, ctx: &mut ViewCtx) {
        let total = self.block_count();
        if total == 0 {
            return;
        }
        let key = self.current.clone();
        let cur = self.ui.entry(key).or_default().selected_block.unwrap_or(0);
        let next = (cur as isize + delta).clamp(0, total as isize - 1) as usize;
        // If we're trying to go further up but already at index 0, ask for
        // older messages.
        if delta < 0 && cur == 0 {
            self.try_load_older(ctx);
        }
        self.select_block(Some(next));
        // Landing on the last item re-engages tail-follow so new content
        // autoscrolls; moving up off it (via select_block) disengages.
        if next == total - 1 {
            let key = self.current.clone();
            let ui = self.ui_mut(&key);
            ui.follow_tail = true;
            ui.seen_block_count = total;
        }
    }

    /// Trigger a load-older fetch for the current session, if applicable.
    fn try_load_older(&mut self, ctx: &mut ViewCtx) {
        let id = match self.current_session_id() {
            Some(s) => s.to_string(),
            None => return,
        };
        if !self.can_load_more(&id) {
            return;
        }
        let entry = self.history_load.entry(id.clone()).or_default();
        let next_limit = if entry.limit == 0 {
            INITIAL_HISTORY_LIMIT
        } else {
            entry.limit + HISTORY_PAGE_SIZE
        };
        entry.limit = next_limit;
        entry.loading = true;
        ctx.push(Action::Http(HttpReq::GetMessages {
            session_id: id,
            limit: next_limit,
        }));
    }

    fn jump_first(&mut self) {
        self.select_block(Some(0));
    }

    fn jump_last(&mut self) {
        let n = self.block_count();
        if n == 0 {
            return;
        }
        let key = self.current.clone();
        self.select_block(Some(n - 1));
        let ui = self.ui_mut(&key);
        ui.follow_tail = true;
        ui.seen_block_count = n;
    }

    /// On (re)entering a session, jump to the latest message — unless the user
    /// had scrolled up (`!follow_tail`) and no blocks have arrived since
    /// (`total <= seen_block_count`), in which case their position is kept.
    /// A no-op until history loads (`apply_messages_loaded` handles first load).
    fn scroll_to_latest_unless_pinned(&mut self, id: &str) {
        let total = self.items_count_for(id);
        if total == 0 {
            return;
        }
        let key = SessionKey::Real(id.to_string());
        let ui = self.ui_mut(&key);
        let pinned = !ui.follow_tail && total <= ui.seen_block_count;
        if pinned {
            ui.selected_block = ui.selected_block.map(|s| s.min(total - 1));
        } else {
            ui.follow_tail = true;
            ui.selected_block = Some(total - 1);
            ui.viewport_top = 0;
            ui.seen_block_count = total;
        }
    }

    fn send_input(&mut self, ctx: &mut ViewCtx) {
        let content = self.input.lines().join("\n");
        if content.trim().is_empty() {
            return;
        }
        match self.current.clone() {
            SessionKey::Real(id) => {
                ctx.push(Action::Ws(WsClientMsg::Message {
                    session_id: id.clone(),
                    content: content.clone(),
                    file_ids: None,
                }));
                // Optimistic local append.
                self.history
                    .entry(id.clone())
                    .or_default()
                    .push(Message::new_user(id.clone(), content));
                let key = SessionKey::Real(id.clone());
                let last_idx = self.history[&id].len().saturating_sub(1);
                let ui = self.ui_mut(&key);
                ui.follow_tail = true;
                ui.selected_block = Some(last_idx);
                self.drafts.clear(&self.current);
                self.input = TextArea::default();
                self.input
                    .set_cursor_line_style(ratatui::style::Style::default());
                self.focus = FocusTier::ChatBlocks;
            }
            SessionKey::NewChat => {
                // Lazy POST: create the session, carrying the typed message so
                // it's sent as the first turn once the id comes back. Clear the
                // input now; `apply_session_created` restores it if the POST fails.
                ctx.push(Action::Http(HttpReq::CreateSession {
                    title: None,
                    content: Some(content),
                }));
                self.drafts.clear(&SessionKey::NewChat);
                self.input = TextArea::default();
                self.input
                    .set_cursor_line_style(ratatui::style::Style::default());
                self.last_error = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Commands surfaced from the `:` parser
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ChatCommand {
    NewChat(Option<String>),
    Fork(Option<String>),
    Resume,
    Rename(Option<String>),
    Delete,
}

// ---------------------------------------------------------------------------
// View impl
// ---------------------------------------------------------------------------

impl View for ChatView {
    fn id(&self) -> &'static str {
        "chat"
    }
    fn title(&self) -> &str {
        "chat"
    }
    fn consumes_global_shortcuts(&self) -> bool {
        // While typing or answering a poll, the view owns every key —
        // global `q` / `?` / `:` must not fire.
        matches!(self.focus, FocusTier::Insert | FocusTier::Poll)
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        match self.focus {
            FocusTier::Sessions => self.keys_sessions(key, ctx),
            FocusTier::ChatBlocks => self.keys_chatblocks(key, ctx),
            FocusTier::BlockInterior => self.keys_block_interior(key, ctx),
            FocusTier::Input => self.keys_input_focused(key, ctx),
            FocusTier::Insert => self.keys_insert(key, ctx),
            FocusTier::Poll => self.keys_poll(key, ctx),
        }
    }

    fn render(&mut self, area: Rect, frame: &mut Frame, ctx: ViewRenderCtx<'_>) {
        super::render::render(self, area, frame, &ctx);
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

use ratatui::Frame;

// ---------------------------------------------------------------------------
// Per-tier key handlers
// ---------------------------------------------------------------------------

impl ChatView {
    fn keys_sessions(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        if self.sidebar_search_active {
            self.keys_sessions_search(key);
            return;
        }
        match (key.code, key.modifiers) {
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                if self.sessions_selected > 0 {
                    let idx = self.sessions_selected - 1;
                    self.preview_session(idx, ctx);
                }
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                let max = self.session_count_total().saturating_sub(1);
                if self.sessions_selected < max {
                    let idx = self.sessions_selected + 1;
                    self.preview_session(idx, ctx);
                }
            }
            (KeyCode::Enter, _) | (KeyCode::Right, _) | (KeyCode::Char('l'), _) => {
                let idx = self.sessions_selected;
                self.enter_session(idx, ctx);
            }
            (KeyCode::Char('/'), _) => {
                self.sidebar_search_active = true;
                self.sidebar_search.clear();
            }
            _ => {}
        }
    }

    fn keys_sessions_search(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.sidebar_search.clear();
                self.sidebar_search_active = false;
            }
            KeyCode::Enter => {
                self.sidebar_search_active = false;
            }
            KeyCode::Backspace => {
                self.sidebar_search.pop();
            }
            KeyCode::Char(c) => {
                self.sidebar_search.push(c);
            }
            _ => {}
        }
    }

    fn keys_chatblocks(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Left, _) | (KeyCode::Char('h'), _) => {
                self.pop_to_sessions();
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                self.move_selection(-1, ctx);
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                let total = self.block_count();
                let key2 = self.current.clone();
                let cur = self
                    .ui
                    .entry(key2.clone())
                    .or_default()
                    .selected_block
                    .unwrap_or(0);
                if total == 0 || cur + 1 >= total {
                    // The input box is hidden while streaming — don't let the
                    // selection fall through into it.
                    if !self.is_current_streaming() {
                        self.jump_to_input();
                    }
                } else {
                    self.move_selection(1, ctx);
                }
            }
            (KeyCode::PageUp, _) => {
                self.move_selection(-5, ctx);
            }
            (KeyCode::PageDown, _) => {
                self.move_selection(5, ctx);
            }
            (KeyCode::Char('g'), _) => {
                self.jump_first();
            }
            (KeyCode::Char('G'), _) => {
                self.jump_last();
            }
            (KeyCode::Enter, _) | (KeyCode::Right, _) | (KeyCode::Char('l'), _) => {
                self.enter_block();
            }
            (KeyCode::Char('i'), _) | (KeyCode::Char('a'), _) => {
                // No text input while the agent streams (input box hidden).
                if !self.is_current_streaming() {
                    self.enter_insert();
                }
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.sidebar_visible = !self.sidebar_visible;
            }
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                self.side_panel.visible = !self.side_panel.visible;
            }
            _ => {}
        }
    }

    fn keys_block_interior(&mut self, key: KeyEvent, _ctx: &mut ViewCtx) {
        // Keys move a line cursor inside the selected block. The renderer
        // clamps the cursor to [0, total_lines) on each frame; we don't have
        // the block's rendered line count here, so we just nudge by ± and
        // let render normalize.
        const PAGE: usize = 10;
        let key2 = self.current.clone();
        let ui = self.ui_mut(&key2);
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Left, _) => {
                self.pop_from_block();
            }
            (KeyCode::Up, _) | (KeyCode::Char('k'), _) => {
                if ui.block_cursor.line > 0 {
                    ui.block_cursor.line -= 1;
                }
            }
            (KeyCode::Down, _) | (KeyCode::Char('j'), _) => {
                ui.block_cursor.line = ui.block_cursor.line.saturating_add(1);
            }
            (KeyCode::PageUp, _) => {
                ui.block_cursor.line = ui.block_cursor.line.saturating_sub(PAGE);
            }
            (KeyCode::PageDown, _) => {
                ui.block_cursor.line = ui.block_cursor.line.saturating_add(PAGE);
            }
            (KeyCode::Char('g'), _) | (KeyCode::Home, _) => {
                ui.block_cursor.line = 0;
                ui.block_cursor.scroll = 0;
            }
            (KeyCode::Char('G'), _) | (KeyCode::End, _) => {
                // "Big" — renderer clamps to total_lines - 1.
                ui.block_cursor.line = usize::MAX;
            }
            (KeyCode::Char('y'), _) => { /* TODO yank to clipboard */ }
            _ => {}
        }
    }

    fn keys_input_focused(&mut self, key: KeyEvent, _ctx: &mut ViewCtx) {
        match (key.code, key.modifiers) {
            (KeyCode::Enter, _) | (KeyCode::Char('i'), _) | (KeyCode::Char('a'), _) => {
                self.enter_insert();
            }
            (KeyCode::Esc, _) | (KeyCode::Up, _) | (KeyCode::Left, _) => {
                self.focus = FocusTier::ChatBlocks;
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.sidebar_visible = !self.sidebar_visible;
            }
            _ => {}
        }
    }

    fn keys_insert(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        // Send on Enter (no shift); newline on Shift+Enter / Alt+Enter.
        let shift_enter =
            matches!(key.code, KeyCode::Enter) && key.modifiers.contains(KeyModifiers::SHIFT);
        let alt_enter =
            matches!(key.code, KeyCode::Enter) && key.modifiers.contains(KeyModifiers::ALT);
        if matches!(key.code, KeyCode::Enter) && !shift_enter && !alt_enter {
            self.send_input(ctx);
            return;
        }
        if matches!(key.code, KeyCode::Esc) {
            self.save_draft();
            self.focus = FocusTier::Input;
            return;
        }
        // Note: Ctrl+C is intercepted globally in app/update.rs::handle_key
        // (stop running agent, otherwise quit) — never reaches this handler.

        self.input.input(key);
        // Persist draft after every keystroke (cheap, < few KB).
        let v = self.input.lines().join("\n");
        self.drafts.set(self.current.clone(), v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant_with_blocks(session_id: &str, n: usize) -> Message {
        let mut m = Message::new_streaming_assistant(session_id.to_string());
        m.blocks = (0..n)
            .map(|i| Block::Text {
                content: format!("block {i}"),
            })
            .collect();
        m
    }

    /// Scrolled up + nothing new since → keep position (clamped in range).
    #[test]
    fn scroll_keeps_position_when_pinned() {
        let mut v = ChatView::new();
        v.history
            .insert("s1".into(), vec![assistant_with_blocks("s1", 3)]);
        let key = SessionKey::Real("s1".into());
        {
            let ui = v.ui.entry(key.clone()).or_default();
            ui.follow_tail = false;
            ui.seen_block_count = 3;
            ui.selected_block = Some(0);
        }
        v.scroll_to_latest_unless_pinned("s1");
        let ui = &v.ui[&key];
        assert_eq!(ui.selected_block, Some(0));
        assert!(!ui.follow_tail);
    }

    /// Scrolled up but new blocks arrived since → jump to the tail.
    #[test]
    fn scroll_jumps_to_tail_when_blocks_arrived() {
        let mut v = ChatView::new();
        v.history
            .insert("s1".into(), vec![assistant_with_blocks("s1", 4)]);
        let key = SessionKey::Real("s1".into());
        {
            let ui = v.ui.entry(key.clone()).or_default();
            ui.follow_tail = false;
            ui.seen_block_count = 3;
            ui.selected_block = Some(0);
        }
        v.scroll_to_latest_unless_pinned("s1");
        let ui = &v.ui[&key];
        assert_eq!(ui.selected_block, Some(3));
        assert!(ui.follow_tail);
        assert_eq!(ui.seen_block_count, 4);
    }

    /// Already following the tail → stay at the latest.
    #[test]
    fn scroll_stays_at_tail_when_following() {
        let mut v = ChatView::new();
        v.history
            .insert("s1".into(), vec![assistant_with_blocks("s1", 3)]);
        let key = SessionKey::Real("s1".into());
        {
            let ui = v.ui.entry(key.clone()).or_default();
            ui.follow_tail = true;
            ui.seen_block_count = 3;
            ui.selected_block = Some(2);
        }
        v.scroll_to_latest_unless_pinned("s1");
        let ui = &v.ui[&key];
        assert_eq!(ui.selected_block, Some(2));
        assert!(ui.follow_tail);
    }

    fn ctx(actions: &mut Vec<Action>) -> ViewCtx<'_> {
        ViewCtx {
            app_actions: actions,
        }
    }

    /// Navigating down onto the last item re-engages tail-follow (autoscroll).
    #[test]
    fn moving_onto_last_item_engages_follow_tail() {
        let mut v = ChatView::new();
        v.focus = FocusTier::ChatBlocks;
        v.current = SessionKey::Real("s1".into());
        v.history
            .insert("s1".into(), vec![assistant_with_blocks("s1", 3)]);
        {
            let ui = v.ui.entry(v.current.clone()).or_default();
            ui.selected_block = Some(1);
            ui.follow_tail = false;
        }
        let mut actions = Vec::new();
        v.move_selection(1, &mut ctx(&mut actions));
        let ui = &v.ui[&SessionKey::Real("s1".into())];
        assert_eq!(ui.selected_block, Some(2));
        assert!(ui.follow_tail);
    }

    /// Moving up off the last item disengages tail-follow.
    #[test]
    fn moving_up_off_last_item_disengages_follow_tail() {
        let mut v = ChatView::new();
        v.focus = FocusTier::ChatBlocks;
        v.current = SessionKey::Real("s1".into());
        v.history
            .insert("s1".into(), vec![assistant_with_blocks("s1", 3)]);
        {
            let ui = v.ui.entry(v.current.clone()).or_default();
            ui.selected_block = Some(2);
            ui.follow_tail = true;
        }
        let mut actions = Vec::new();
        v.move_selection(-1, &mut ctx(&mut actions));
        let ui = &v.ui[&SessionKey::Real("s1".into())];
        assert_eq!(ui.selected_block, Some(1));
        assert!(!ui.follow_tail);
    }
}
