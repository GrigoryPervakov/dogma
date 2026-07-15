//! ChatView state — focus tiers, draft registry, sessions list, history,
//! streaming reducer.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::ui::textarea::TextArea;

use crate::api::types::{
    BackendsInfo, HttpReq, MessagesPayload, ModelOption, ModelsPayload, WsClientMsg, WsServerMsg,
};
use crate::app::action::Action;
use crate::instance::InstanceId;
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
    /// Browsing the session's changed-files list / a file diff.
    Files,
    /// New-chat setup form: instance / backend / model columns, shown only
    /// when at least one of them offers a real choice.
    NewChatPicker,
    /// Choosing the composer model (only when >1 model is offered).
    ModelPicker,
}

/// One column of the new-chat setup form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupCol {
    Instance,
    Backend,
    Model,
}

/// Step `cur` by `dir` within `0..len`, skipping items rejected by `ok`.
/// Stays put when nothing acceptable exists in that direction.
fn step_index(len: usize, cur: usize, dir: isize, ok: impl Fn(usize) -> bool) -> usize {
    let mut i = cur as isize;
    loop {
        i += dir;
        if i < 0 || i >= len as isize {
            return cur;
        }
        if ok(i as usize) {
            return i as usize;
        }
    }
}

// ---------------------------------------------------------------------------
// Drafts
// ---------------------------------------------------------------------------

/// A session identity qualified by the instance it lives on — ids are only
/// unique within one Nerve server, so every map key and `current` pointer uses
/// this rather than a bare id string.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SessionRef {
    pub instance: InstanceId,
    pub id: String,
}

impl SessionRef {
    pub fn new(instance: InstanceId, id: impl Into<String>) -> Self {
        Self {
            instance,
            id: id.into(),
        }
    }
}

/// A bare id refers to the primary instance — convenient for single-instance
/// contexts and tests.
impl From<&str> for SessionRef {
    fn from(id: &str) -> Self {
        SessionRef::new(InstanceId::PRIMARY, id)
    }
}

impl From<String> for SessionRef {
    fn from(id: String) -> Self {
        SessionRef::new(InstanceId::PRIMARY, id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionKey {
    Real(SessionRef),
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

#[derive(Debug, Clone)]
pub struct SubAgentPanel {
    pub tool_use_id: String,
    pub kind: String, // "Plan" | "Agent" | "Explore" | ...
    pub description: String,
    pub blocks: Vec<Block>,
    pub running: bool,
    /// Session the sub-agent belongs to — the strip shows only the current
    /// session's panels, and a finished turn prunes its session's entries.
    pub session: SessionRef,
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
    /// Paused on an `EnterPlanMode`/`ExitPlanMode` approval.
    WaitingPlan,
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
    /// Which instance a brand-new chat is created on (the `+ new chat` row),
    /// chosen via the new-chat setup form when several instances are connected.
    pub new_chat_target: InstanceId,
    /// Focused column of the new-chat setup form.
    pub setup_col: SetupCol,

    // Backend + model pickers (from `GET /api/models`)
    /// Selectable chat models per instance, tagged with their backend.
    pub models: HashMap<InstanceId, Vec<ModelOption>>,
    /// Per-instance default model per backend (`defaults` in the payload).
    pub model_defaults: HashMap<InstanceId, HashMap<String, String>>,
    /// Per-instance agent backends (`backends` block; absent on old servers).
    pub backends: HashMap<InstanceId, BackendsInfo>,
    /// Composer model override per backend, sent on the next message.
    /// Absent = backend default. Mirrors the web's `selectedModels`.
    pub selected_models: HashMap<String, String>,
    /// Backend for the next new chat (`None` = server default). Consumed and
    /// reset when the session materializes, mirroring the web's
    /// `newChatBackend`.
    pub new_chat_backend: Option<String>,
    /// Cursor in the standalone `:model` picker.
    pub model_pick: usize,
    /// Which backend the open model picker edits.
    pub model_picker_backend: String,

    // Chat state per session (history + streaming buffer + UI bits)
    pub history: HashMap<SessionRef, Vec<Message>>,
    pub streaming: HashMap<SessionRef, Message>,
    pub agent_status: HashMap<SessionRef, AgentStatus>,
    pub ui: HashMap<SessionKey, PerSessionUi>,

    // Drafts per session
    pub drafts: DraftRegistry,
    pub input: TextArea,

    // Side panel
    pub side_panel: SidePanelState,

    // Pending interaction (one at a time, per session)
    pub pending_interaction: HashMap<SessionRef, PendingInteraction>,

    // Selection state for the current session's focused poll, if any.
    pub poll_ui: Option<PollUiState>,

    // Interaction ids already answered/denied — guards against a resolved
    // poll being resurrected when buffered events replay on session re-entry.
    pub answered_interactions: std::collections::HashSet<String>,

    // Changed files per session (from `/modified-files`, refreshed on the
    // FileChanged WS event). Browsed in the Files tier.
    pub modified_files: HashMap<SessionRef, Vec<crate::model::ModifiedFile>>,
    pub files_selected: usize,
    pub files_loading: bool,
    /// The diff currently open in the Files tier, plus its scroll offset.
    pub open_diff: Option<crate::model::FileDiff>,
    pub diff_scroll: usize,

    // Wall-clock of the last WS event per session — drives the stall watchdog
    // that resyncs a session gone quiet mid-stream.
    pub last_activity: HashMap<SessionRef, std::time::Instant>,

    // run_in_background jobs per session (from BackgroundTasksUpdate WS events).
    pub background_tasks: HashMap<SessionRef, Vec<serde_json::Value>>,

    // Context usage per session
    pub context: HashMap<SessionRef, ContextUsage>,

    // Sessions request flight
    pub sessions_loaded: bool,
    pub last_error: Option<String>,

    // History pagination state per session.
    pub history_load: HashMap<SessionRef, HistoryLoadState>,
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
            new_chat_target: InstanceId::PRIMARY,
            setup_col: SetupCol::Instance,
            models: HashMap::new(),
            model_defaults: HashMap::new(),
            backends: HashMap::new(),
            selected_models: HashMap::new(),
            new_chat_backend: None,
            model_pick: 0,
            model_picker_backend: String::new(),
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
            modified_files: HashMap::new(),
            files_selected: 0,
            files_loading: false,
            open_diff: None,
            diff_scroll: 0,
            last_activity: HashMap::new(),
            background_tasks: HashMap::new(),
            context: HashMap::new(),
            sessions_loaded: false,
            last_error: None,
            history_load: HashMap::new(),
        }
    }

    pub fn current_session_id(&self) -> Option<&str> {
        match &self.current {
            SessionKey::Real(s) => Some(s.id.as_str()),
            SessionKey::NewChat => None,
        }
    }

    /// The full instance-qualified ref of the current session, if any.
    pub fn current_session_ref(&self) -> Option<&SessionRef> {
        match &self.current {
            SessionKey::Real(s) => Some(s),
            SessionKey::NewChat => None,
        }
    }

    /// The instance an action on the current view targets: the current
    /// session's instance, or the new-chat target while composing.
    pub fn current_instance(&self) -> InstanceId {
        match &self.current {
            SessionKey::Real(s) => s.instance,
            SessionKey::NewChat => self.new_chat_target,
        }
    }

    /// Sub-agent panels belonging to the session on screen.
    pub fn current_panels(&self) -> Vec<&SubAgentPanel> {
        match self.current_session_ref() {
            Some(sref) => self
                .side_panel
                .panels
                .iter()
                .filter(|p| p.session == *sref)
                .collect(),
            None => Vec::new(),
        }
    }

    /// Coarse runtime state of a session: waiting on a poll, actively
    /// streaming, or idle.
    pub fn session_runtime(&self, sref: &SessionRef) -> SessionRuntime {
        match self
            .pending_interaction
            .get(sref)
            .map(|p| p.interaction_type.as_str())
        {
            Some("question") => return SessionRuntime::WaitingPoll,
            Some("plan_enter") | Some("plan_exit") => return SessionRuntime::WaitingPlan,
            _ => {}
        }
        let running = self
            .sessions
            .iter()
            .find(|s| s.instance == sref.instance && s.id == sref.id)
            .map(|s| s.is_running)
            .unwrap_or(false);
        let streaming = running
            || self.streaming.contains_key(sref)
            || !matches!(self.agent_status.get(sref), None | Some(AgentStatus::Idle));
        if streaming {
            SessionRuntime::Streaming
        } else {
            SessionRuntime::Idle
        }
    }

    /// Whether the currently-viewed session is actively streaming — gates the
    /// compact (collapsed + typed-header) view off and the green frame on.
    pub fn is_current_streaming(&self) -> bool {
        self.current_session_ref()
            .map(|r| matches!(self.session_runtime(r), SessionRuntime::Streaming))
            .unwrap_or(false)
    }

    /// Called on every UI tick: if the current session looks like it's
    /// streaming but no WS event has arrived in a while, resync it with the
    /// server. Non-destructive — the `SessionStatus` reply rebuilds a running
    /// turn or heals a finished one (a lost Done leaves a chat stuck).
    /// `connected[i]` is whether instance `i`'s WS is up.
    pub fn tick_watchdog(&mut self, connected: &[bool], ctx: &mut ViewCtx) {
        const STALL: std::time::Duration = std::time::Duration::from_secs(60);
        let Some(sref) = self.current_session_ref().cloned() else {
            return;
        };
        let online = connected
            .get(sref.instance.index())
            .copied()
            .unwrap_or(false);
        if !online || !self.is_current_streaming() {
            return;
        }
        let stale = self
            .last_activity
            .get(&sref)
            .map(|t| t.elapsed() >= STALL)
            .unwrap_or(true);
        if !stale {
            return;
        }
        // Debounce — resync at most once per stall window.
        self.last_activity
            .insert(sref.clone(), std::time::Instant::now());
        ctx.ws(
            sref.instance,
            WsClientMsg::SwitchSession {
                session_id: sref.id.clone(),
            },
        );
        ctx.http(
            sref.instance,
            HttpReq::GetMessages {
                session_id: sref.id,
                limit: INITIAL_HISTORY_LIMIT,
            },
        );
    }

    pub fn current_history(&self) -> &[Message] {
        match &self.current {
            SessionKey::Real(r) => self.history.get(r).map(|v| v.as_slice()).unwrap_or(&[]),
            SessionKey::NewChat => &[],
        }
    }

    pub fn current_streaming(&self) -> Option<&Message> {
        match &self.current {
            SessionKey::Real(r) => self.streaming.get(r),
            SessionKey::NewChat => None,
        }
    }

    pub fn current_agent_status(&self) -> &AgentStatus {
        static IDLE: AgentStatus = AgentStatus::Idle;
        match &self.current {
            SessionKey::Real(r) => self.agent_status.get(r).unwrap_or(&IDLE),
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
            SessionKey::Real(r) => self.pending_interaction.get(r),
            SessionKey::NewChat => None,
        }
    }

    pub fn current_context(&self) -> Option<&ContextUsage> {
        match &self.current {
            SessionKey::Real(r) => self.context.get(r),
            SessionKey::NewChat => None,
        }
    }

    /// Cumulative cost in USD for the current session (from the sessions
    /// list). Returns 0.0 if unknown.
    pub fn current_cost_usd(&self) -> f64 {
        match &self.current {
            SessionKey::Real(r) => self
                .sessions
                .iter()
                .find(|s| s.instance == r.instance && s.id == r.id)
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
            Some(s) => SessionKey::Real(SessionRef::new(s.instance, s.id.clone())),
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
                // Start the stall-watchdog clock when a session first appears.
                self.last_activity
                    .entry(id.clone())
                    .or_insert_with(std::time::Instant::now);
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
                if let Some(sref) = self.current_session_ref().cloned() {
                    out.push(Action::Ws {
                        instance: sref.instance,
                        msg: WsClientMsg::Fork {
                            session_id: sref.id,
                            at_message_id: None,
                            title,
                        },
                    });
                }
            }
            ChatCommand::Resume => {
                if let Some(sref) = self.current_session_ref().cloned() {
                    out.push(Action::Ws {
                        instance: sref.instance,
                        msg: WsClientMsg::Resume {
                            session_id: sref.id,
                        },
                    });
                }
            }
            ChatCommand::Rename(_) => { /* TODO PATCH /api/sessions/{id} */ }
            ChatCommand::Delete => { /* TODO DELETE /api/sessions/{id} */ }
            ChatCommand::Reload => {
                if let Some(sref) = self.current_session_ref().cloned() {
                    // Drop every local cache for this session — an orphaned
                    // streaming buffer or a non-Idle agent_status (from a lost
                    // Done/Stopped) is exactly what gets a chat "stuck".
                    self.streaming.remove(&sref);
                    self.agent_status.insert(sref.clone(), AgentStatus::Idle);
                    self.history.remove(&sref);
                    self.history_load.remove(&sref);
                    self.pending_interaction.remove(&sref);
                    self.modified_files.remove(&sref);
                    self.open_diff = None;
                    if let Some(s) = self
                        .sessions
                        .iter_mut()
                        .find(|s| s.instance == sref.instance && s.id == sref.id)
                    {
                        s.is_running = false;
                    }
                    self.poll_ui = None;
                    if matches!(self.focus, FocusTier::Poll | FocusTier::Files) {
                        self.focus = FocusTier::ChatBlocks;
                    }
                    // Re-route events and refetch canonical history.
                    out.push(Action::Ws {
                        instance: sref.instance,
                        msg: WsClientMsg::SwitchSession {
                            session_id: sref.id.clone(),
                        },
                    });
                    out.push(Action::Http {
                        instance: sref.instance,
                        req: HttpReq::GetMessages {
                            session_id: sref.id.clone(),
                            limit: INITIAL_HISTORY_LIMIT,
                        },
                    });
                    let entry = self.history_load.entry(sref).or_default();
                    entry.limit = INITIAL_HISTORY_LIMIT;
                    entry.loading = true;
                    entry.exhausted = false;
                }
            }
        }
        out
    }

    // ----- HTTP results -----------------------------------------------------

    pub fn apply_sessions_loaded(
        &mut self,
        instance: InstanceId,
        result: std::result::Result<Vec<Session>, String>,
        _ctx: &mut ViewCtx,
    ) {
        match result {
            Ok(mut list) => {
                // Stamp the origin instance, then merge: replace this instance's
                // rows, keep the others. User-source sessions sort above
                // system-source (cron / hook); each group most-recent-first
                // across all instances. Mirrors
                // web/src/components/Chat/SessionSidebar.tsx.
                for s in &mut list {
                    s.instance = instance;
                }
                self.sessions.retain(|s| s.instance != instance);
                self.sessions.append(&mut list);
                self.sessions.sort_by(|a, b| {
                    is_system_session(a)
                        .cmp(&is_system_session(b))
                        .then_with(|| {
                            b.updated_at
                                .as_deref()
                                .unwrap_or("")
                                .cmp(a.updated_at.as_deref().unwrap_or(""))
                        })
                });
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

    /// Float a user session to the top of the list — most-recent-first, like
    /// the web. Called when a message is sent, so the chat you just wrote to
    /// jumps to the top (the next `ListSessions` agrees, since the server bumps
    /// `updated_at`). System/cron sessions keep their own group ordering.
    fn bump_session_to_front(&mut self, sref: &SessionRef) {
        let Some(pos) = self
            .sessions
            .iter()
            .position(|s| s.instance == sref.instance && s.id == sref.id)
        else {
            return;
        };
        if is_system_session(&self.sessions[pos]) {
            return;
        }
        if pos != 0 {
            let session = self.sessions.remove(pos);
            self.sessions.insert(0, session);
        }
        // Sidebar index 0 is "+ new chat", so the front session is at 1.
        self.sessions_selected = 1;
    }

    pub fn apply_messages_loaded(
        &mut self,
        instance: InstanceId,
        session_id: &str,
        limit: u32,
        result: std::result::Result<MessagesPayload, String>,
        _ctx: &mut ViewCtx,
    ) {
        let sref = SessionRef::new(instance, session_id);
        match result {
            Ok(payload) => {
                let MessagesPayload {
                    messages,
                    last_usage,
                } = payload;
                let new_len = messages.len();
                let old_items: usize = self
                    .history
                    .get(&sref)
                    .map(|h| h.iter().map(|m| m.blocks.len()).sum::<usize>())
                    .unwrap_or(0);
                let new_items: usize = messages.iter().map(|m| m.blocks.len()).sum();
                let is_initial_load = old_items == 0;

                self.history.insert(sref.clone(), messages);
                let key = SessionKey::Real(sref.clone());

                // History-load bookkeeping.
                let entry = self.history_load.entry(sref.clone()).or_default();
                entry.limit = limit;
                entry.loading = false;
                // Server returned < limit → we've got everything.
                entry.exhausted = (new_len as u32) < limit;

                // Wire context usage from the messages-endpoint response.
                // last_usage nests max_context_tokens + num_turns inside the
                // Usage object (vs. WS Done where they're top-level).
                let ctx_entry = self.context.entry(sref.clone()).or_default();
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
                if let Some(s) = self.history_load.get_mut(&sref) {
                    s.loading = false;
                }
                self.last_error = Some(format!("history({session_id}): {e}"));
            }
        }
    }

    /// Returns true if there might be more older messages on the server.
    pub fn can_load_more(&self, sref: &SessionRef) -> bool {
        match self.history_load.get(sref) {
            Some(s) => !s.exhausted && !s.loading,
            // No load record yet → we haven't loaded anything; assume yes.
            None => true,
        }
    }

    /// True while a history fetch is in flight for the current session.
    pub fn is_loading_history(&self) -> bool {
        match &self.current {
            SessionKey::Real(r) => self.history_load.get(r).map(|s| s.loading).unwrap_or(false),
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

            let ref_for_hint = match &key {
                SessionKey::Real(r) => Some(r.clone()),
                SessionKey::NewChat => None,
            };
            let can_load = ref_for_hint
                .as_ref()
                .map(|r| {
                    self.history_load
                        .get(r)
                        .map(|s| !s.exhausted && !s.loading)
                        .unwrap_or(true)
                })
                .unwrap_or(false);
            let loading = ref_for_hint
                .as_ref()
                .map(|r| self.history_load.get(r).map(|s| s.loading).unwrap_or(false))
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
        instance: InstanceId,
        pending_content: Option<String>,
        result: std::result::Result<Session, String>,
        ctx: &mut ViewCtx,
    ) {
        match result {
            Ok(mut session) => {
                session.instance = instance;
                // POST returns a partial row — stamp the backend it was created
                // with so the badge and model resolution are right before the
                // next sessions refresh.
                if session.backend.is_none() {
                    session.backend = self
                        .new_chat_backend
                        .clone()
                        .or_else(|| self.backends.get(&instance).map(|b| b.default.clone()));
                }
                self.new_chat_backend = None;
                let backend = session
                    .backend
                    .clone()
                    .unwrap_or_else(|| "claude".to_string());
                let first_msg_model = self.selected_models.get(&backend).cloned();
                let sref = SessionRef::new(instance, session.id.clone());
                // Insert at top of live group.
                self.sessions.insert(0, session);
                // Switch active session.
                self.current = SessionKey::Real(sref.clone());
                self.drafts.clear(&SessionKey::NewChat);
                // Sidebar selection now points to the new session (idx 1).
                self.sessions_selected = 1;
                // SwitchSession over WS.
                ctx.ws(
                    instance,
                    WsClientMsg::SwitchSession {
                        session_id: sref.id.clone(),
                    },
                );
                // Send pending content as the first message.
                if let Some(content) = pending_content
                    && !content.is_empty()
                {
                    ctx.ws(
                        instance,
                        WsClientMsg::Message {
                            session_id: sref.id.clone(),
                            content: content.clone(),
                            file_ids: None,
                            model: first_msg_model,
                        },
                    );
                    // Optimistically append the user message to history.
                    self.history
                        .entry(sref.clone())
                        .or_default()
                        .push(Message::new_user(sref.id.clone(), content));
                    // Follow-tail engaged.
                    let key = SessionKey::Real(sref.clone());
                    let last_idx = self.items_count_for(&sref).saturating_sub(1);
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

    pub fn apply_wire(&mut self, instance: InstanceId, msg: &WsServerMsg, ctx: &mut ViewCtx) {
        super::reducer::apply(self, instance, msg, ctx);
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
        if let SessionKey::Real(sref) = &key {
            self.ensure_history_fetch(sref, ctx);
        }
    }

    /// Make the session at sidebar `idx` active: preview + WS switch_session
    /// + focus push.
    fn enter_session(&mut self, idx: usize, ctx: &mut ViewCtx) {
        self.sessions_selected = idx;
        let key = self.key_at(idx);
        self.switch_to(key.clone());

        match &key {
            SessionKey::Real(sref) => {
                ctx.ws(
                    sref.instance,
                    WsClientMsg::SwitchSession {
                        session_id: sref.id.clone(),
                    },
                );
                self.ensure_history_fetch(sref, ctx);
                // Land on the input box, ready to type — unless the agent is
                // mid-stream (input is hidden) in which case browse the blocks.
                // A pending poll/plan overrides below.
                self.focus = if self.is_current_streaming() {
                    FocusTier::ChatBlocks
                } else {
                    FocusTier::Input
                };
                // Cached session: jump to the latest unless the user is pinned
                // up-thread. (Uncached: handled on history load.)
                self.scroll_to_latest_unless_pinned(sref);
                self.focus_interaction_if_pending();
            }
            SessionKey::NewChat => {
                self.show_new_chat_setup(ctx.instances);
            }
        }
    }

    /// Open the new-chat setup form (instance / backend / model columns).
    /// Goes straight to Insert when no column offers a real choice.
    pub fn show_new_chat_setup(&mut self, instances: &[InstanceId]) {
        match self.setup_cols(instances.len()).first() {
            Some(&first) => {
                self.setup_col = first;
                self.focus = FocusTier::NewChatPicker;
            }
            None => self.focus = FocusTier::Insert,
        }
    }

    /// Form columns that currently offer more than one choice. Later columns
    /// re-derive from earlier answers (instance → backends → models).
    pub fn setup_cols(&self, n_instances: usize) -> Vec<SetupCol> {
        let mut cols = Vec::new();
        if n_instances > 1 {
            cols.push(SetupCol::Instance);
        }
        let backends = self
            .backends
            .get(&self.current_instance())
            .map(|b| b.options.len())
            .unwrap_or(0);
        if backends > 1 {
            cols.push(SetupCol::Backend);
        }
        if self.backend_models(&self.current_backend()).len() > 1 {
            cols.push(SetupCol::Model);
        }
        cols
    }

    pub fn setup_instance_index(&self, instances: &[InstanceId]) -> usize {
        instances
            .iter()
            .position(|&i| i == self.new_chat_target)
            .unwrap_or(0)
    }

    pub fn setup_backend_index(&self) -> usize {
        let Some(info) = self.backends.get(&self.current_instance()) else {
            return 0;
        };
        let active = self.new_chat_backend.as_deref().unwrap_or(&info.default);
        info.options
            .iter()
            .position(|o| o.id == active)
            .unwrap_or(0)
    }

    pub fn setup_model_index(&self) -> usize {
        let backend = self.current_backend();
        let active = self
            .selected_models
            .get(&backend)
            .cloned()
            .or_else(|| self.default_model_for(&backend));
        active
            .and_then(|id| {
                self.backend_models(&backend)
                    .iter()
                    .position(|m| m.id == id)
            })
            .unwrap_or(0)
    }

    fn keys_new_chat_setup(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        let cols = self.setup_cols(ctx.instances.len());
        if cols.is_empty() {
            self.focus = FocusTier::Insert;
            return;
        }
        // An earlier answer may have removed the focused column.
        let col_idx = match cols.iter().position(|&c| c == self.setup_col) {
            Some(i) => i,
            None => {
                self.setup_col = cols[cols.len() - 1];
                cols.len() - 1
            }
        };
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => {
                if col_idx == 0 {
                    self.focus = FocusTier::Sessions;
                } else {
                    self.setup_col = cols[col_idx - 1];
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                if col_idx + 1 < cols.len() {
                    self.setup_col = cols[col_idx + 1];
                }
            }
            KeyCode::Tab => {
                self.setup_col = cols[(col_idx + 1) % cols.len()];
            }
            KeyCode::Up | KeyCode::Char('k') => self.setup_move(-1, ctx),
            KeyCode::Down | KeyCode::Char('j') => self.setup_move(1, ctx),
            KeyCode::Enter => {
                self.focus = FocusTier::Insert;
            }
            KeyCode::Esc => {
                self.focus = FocusTier::Sessions;
            }
            _ => {}
        }
    }

    /// Move the focused column's answer by `dir`. The answer commits in place —
    /// the cursor *is* the selection — so later columns re-derive immediately.
    fn setup_move(&mut self, dir: isize, ctx: &ViewCtx) {
        match self.setup_col {
            SetupCol::Instance => {
                let cur = self.setup_instance_index(ctx.instances);
                let next = step_index(ctx.instances.len(), cur, dir, |_| true);
                if let Some(&id) = ctx.instances.get(next) {
                    self.new_chat_target = id;
                }
            }
            SetupCol::Backend => {
                let inst = self.current_instance();
                let picked = {
                    let Some(info) = self.backends.get(&inst) else {
                        return;
                    };
                    let cur = self.setup_backend_index();
                    let next =
                        step_index(info.options.len(), cur, dir, |i| info.options[i].available);
                    info.options
                        .get(next)
                        .map(|o| (o.id.clone(), info.default.clone()))
                };
                if let Some((id, default)) = picked {
                    self.new_chat_backend = if id == default { None } else { Some(id) };
                }
            }
            SetupCol::Model => {
                let backend = self.current_backend();
                let models: Vec<String> = self
                    .backend_models(&backend)
                    .iter()
                    .map(|m| m.id.clone())
                    .collect();
                let cur = self.setup_model_index();
                let next = step_index(models.len(), cur, dir, |_| true);
                if let Some(id) = models.get(next).cloned() {
                    if self.default_model_for(&backend).as_deref() == Some(id.as_str()) {
                        self.selected_models.remove(&backend);
                    } else {
                        self.selected_models.insert(backend, id);
                    }
                }
            }
        }
    }

    /// Backend of the composer's target: the session's sticky backend, or the
    /// new-chat choice (falling back to the instance default) while composing.
    pub fn current_backend(&self) -> String {
        let inst_default = || {
            self.backends
                .get(&self.current_instance())
                .map(|b| b.default.clone())
                .unwrap_or_else(|| "claude".to_string())
        };
        match &self.current {
            SessionKey::Real(sref) => self
                .sessions
                .iter()
                .find(|s| s.instance == sref.instance && s.id == sref.id)
                .and_then(|s| s.backend.clone())
                .unwrap_or_else(inst_default),
            SessionKey::NewChat => self.new_chat_backend.clone().unwrap_or_else(inst_default),
        }
    }

    /// Current instance's models served by `backend`. Untagged models (from a
    /// pre-backend server) count for every backend.
    pub fn backend_models(&self, backend: &str) -> Vec<&ModelOption> {
        self.models
            .get(&self.current_instance())
            .into_iter()
            .flatten()
            .filter(|m| m.backend == backend || m.backend.is_empty())
            .collect()
    }

    fn default_model_for(&self, backend: &str) -> Option<String> {
        self.model_defaults
            .get(&self.current_instance())
            .and_then(|d| d.get(backend))
            .cloned()
    }

    /// Model override the next message should carry (`None` = server default).
    pub fn pending_model_override(&self) -> Option<String> {
        self.selected_models.get(&self.current_backend()).cloned()
    }

    /// Open the model picker for the composer's current backend (`:model`).
    /// No-op (with a hint) when that backend serves a single model.
    pub fn show_model_picker(&mut self) {
        let backend = self.current_backend();
        if self.backend_models(&backend).len() <= 1 {
            self.last_error = Some(format!("only one model available on {backend}"));
            return;
        }
        self.show_model_picker_for(backend);
    }

    fn show_model_picker_for(&mut self, backend: String) {
        let active = self
            .selected_models
            .get(&backend)
            .cloned()
            .or_else(|| self.default_model_for(&backend));
        self.model_pick = active
            .and_then(|id| {
                self.backend_models(&backend)
                    .iter()
                    .position(|m| m.id == id)
            })
            .unwrap_or(0);
        self.model_picker_backend = backend;
        self.focus = FocusTier::ModelPicker;
    }

    fn keys_model_picker(&mut self, key: KeyEvent) {
        let backend = self.model_picker_backend.clone();
        let n = self.backend_models(&backend).len();
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.model_pick = self.model_pick.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.model_pick + 1 < n {
                    self.model_pick += 1;
                }
            }
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(id) = self
                    .backend_models(&backend)
                    .get(self.model_pick)
                    .map(|m| m.id.clone())
                {
                    if self.default_model_for(&backend).as_deref() == Some(id.as_str()) {
                        self.selected_models.remove(&backend);
                    } else {
                        self.selected_models.insert(backend, id);
                    }
                }
                self.focus = FocusTier::Insert;
            }
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                // Mid-chain (new chat): accept the default and start typing.
                self.focus = match self.current {
                    SessionKey::NewChat => FocusTier::Insert,
                    SessionKey::Real(_) => FocusTier::Input,
                };
            }
            _ => {}
        }
    }

    pub fn apply_models_loaded(
        &mut self,
        instance: InstanceId,
        result: std::result::Result<ModelsPayload, String>,
    ) {
        match result {
            Ok(payload) => {
                let mut defaults = payload.defaults;
                if defaults.is_empty() && !payload.default.is_empty() {
                    // Pre-backend server: a single default under "claude".
                    defaults.insert("claude".to_string(), payload.default);
                }
                self.model_defaults.insert(instance, defaults);
                if let Some(backends) = payload.backends {
                    self.backends.insert(instance, backends);
                }
                self.models.insert(instance, payload.models);
            }
            Err(e) => self.last_error = Some(format!("list models: {e}")),
        }
    }

    fn ensure_history_fetch(&mut self, sref: &SessionRef, ctx: &mut ViewCtx) {
        if self.history.contains_key(sref) {
            return;
        }
        ctx.http(
            sref.instance,
            HttpReq::GetMessages {
                session_id: sref.id.clone(),
                limit: INITIAL_HISTORY_LIMIT,
            },
        );
        let entry = self.history_load.entry(sref.clone()).or_default();
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

    /// Leave the editor for the message list, landing on the block directly
    /// above the input (the latest one). The draft is preserved.
    fn exit_insert_to_blocks(&mut self) {
        self.save_draft();
        self.focus = FocusTier::ChatBlocks;
        self.jump_last();
    }

    // ---- Pending poll (AskUserQuestion) --------------------------------

    /// The agent's current task list (TaskCreate/Update/List), reconstructed
    /// from the active session's tool-call blocks.
    pub fn current_tasks(&self) -> Vec<super::tasks_panel::CcTask> {
        super::tasks_panel::extract(self.current_history(), self.current_streaming())
    }

    /// run_in_background jobs for the active session.
    pub fn current_background(&self) -> &[serde_json::Value] {
        self.current_session_ref()
            .and_then(|r| self.background_tasks.get(r))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The poll the user can answer right now: a pending `question`
    /// interaction for the current session whose id matches `poll_ui`.
    pub fn active_poll(&self) -> Option<&PollUiState> {
        let sref = self.current_session_ref()?;
        let pending = self.pending_interaction.get(sref)?;
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
            .current_session_ref()
            .and_then(|sref| self.pending_interaction.get(sref))
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

    /// Move focus to the poll waiting for this session, if any. A pending plan
    /// does NOT grab focus — it lives as the latest block and is answered with
    /// `a`/`d` from the message list, so navigation is never trapped.
    pub fn focus_interaction_if_pending(&mut self) {
        if self.active_poll().is_some() {
            self.focus = FocusTier::Poll;
        }
    }

    /// The pending `EnterPlanMode`/`ExitPlanMode` interaction the user can
    /// approve right now for the current session, if any.
    pub fn active_plan(&self) -> Option<&PendingInteraction> {
        let sref = self.current_session_ref()?;
        let p = self.pending_interaction.get(sref)?;
        (matches!(p.interaction_type.as_str(), "plan_enter" | "plan_exit")
            && !self.answered_interactions.contains(&p.interaction_id))
        .then_some(p)
    }

    /// Answer the pending plan (`a` approve / `d` decline). No-op without one.
    fn answer_plan(&mut self, approved: bool, ctx: &mut ViewCtx) {
        let Some(p) = self.active_plan() else {
            return;
        };
        let Some(sref) = self.current_session_ref().cloned() else {
            return;
        };
        let interaction_id = p.interaction_id.clone();
        self.answered_interactions.insert(interaction_id.clone());
        ctx.ws(
            sref.instance,
            WsClientMsg::AnswerInteraction {
                session_id: sref.id.clone(),
                interaction_id,
                result: None,
                denied: !approved,
                message: (!approved).then(|| "User declined.".to_string()),
            },
        );
        self.pending_interaction.remove(&sref);
        self.jump_last();
    }

    // ----- Changed files / diff viewer ----------------------------------

    /// The current session's changed-files list.
    pub fn current_modified_files(&self) -> &[crate::model::ModifiedFile] {
        self.current_session_ref()
            .and_then(|r| self.modified_files.get(r))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Open the Files tier and (re)fetch the changed-files list.
    fn open_files_panel(&mut self, ctx: &mut ViewCtx) {
        let Some(sref) = self.current_session_ref().cloned() else {
            return;
        };
        self.focus = FocusTier::Files;
        self.open_diff = None;
        self.diff_scroll = 0;
        self.files_selected = self
            .files_selected
            .min(self.current_modified_files().len().saturating_sub(1));
        self.files_loading = true;
        ctx.http(
            sref.instance,
            HttpReq::GetModifiedFiles {
                session_id: sref.id,
            },
        );
    }

    /// Refresh the changed-files list for a session if it's already tracked —
    /// used to react to FileChanged events without grabbing focus.
    pub fn refresh_modified_files(&mut self, sref: &SessionRef, ctx: &mut ViewCtx) {
        if self.modified_files.contains_key(sref) {
            ctx.http(
                sref.instance,
                HttpReq::GetModifiedFiles {
                    session_id: sref.id.clone(),
                },
            );
        }
    }

    pub fn apply_modified_files(
        &mut self,
        instance: InstanceId,
        session_id: &str,
        result: std::result::Result<Vec<crate::model::ModifiedFile>, String>,
    ) {
        self.files_loading = false;
        match result {
            Ok(files) => {
                self.files_selected = self.files_selected.min(files.len().saturating_sub(1));
                self.modified_files
                    .insert(SessionRef::new(instance, session_id), files);
            }
            Err(e) => self.last_error = Some(format!("modified-files: {e}")),
        }
    }

    pub fn apply_file_diff(
        &mut self,
        _instance: InstanceId,
        _session_id: &str,
        _path: &str,
        result: std::result::Result<crate::model::FileDiff, String>,
    ) {
        match result {
            Ok(diff) => {
                self.open_diff = Some(diff);
                self.diff_scroll = 0;
            }
            Err(e) => self.last_error = Some(format!("file-diff: {e}")),
        }
    }

    fn keys_files(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        // A diff is open → scroll it; Esc/← drops back to the file list.
        if self.open_diff.is_some() {
            match key.code {
                KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                    self.open_diff = None;
                    self.diff_scroll = 0;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.diff_scroll = self.diff_scroll.saturating_add(1);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.diff_scroll = self.diff_scroll.saturating_sub(1);
                }
                KeyCode::PageDown => self.diff_scroll = self.diff_scroll.saturating_add(10),
                KeyCode::PageUp => self.diff_scroll = self.diff_scroll.saturating_sub(10),
                KeyCode::Char('g') | KeyCode::Home => self.diff_scroll = 0,
                _ => {}
            }
            return;
        }
        let n = self.current_modified_files().len();
        match key.code {
            KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('q') => {
                self.focus = FocusTier::ChatBlocks;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.files_selected + 1 < n {
                    self.files_selected += 1;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.files_selected = self.files_selected.saturating_sub(1);
            }
            KeyCode::Char('r') => self.open_files_panel(ctx),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let target = self
                    .current_modified_files()
                    .get(self.files_selected)
                    .map(|f| f.path.clone());
                if let (Some(path), Some(sref)) = (target, self.current_session_ref().cloned()) {
                    ctx.http(
                        sref.instance,
                        HttpReq::GetFileDiff {
                            session_id: sref.id,
                            path,
                        },
                    );
                }
            }
            _ => {}
        }
    }

    fn clear_poll(&mut self, sref: &SessionRef) {
        self.pending_interaction.remove(sref);
        self.poll_ui = None;
        self.focus = FocusTier::ChatBlocks;
        self.jump_last();
    }

    fn submit_poll(&mut self, ctx: &mut ViewCtx) {
        let Some(ui) = self.poll_ui.as_ref() else {
            return;
        };
        let Some(sref) = self.current_session_ref().cloned() else {
            return;
        };
        let interaction_id = ui.interaction_id.clone();
        let result = ui.build_answers();
        self.answered_interactions.insert(interaction_id.clone());
        ctx.ws(
            sref.instance,
            WsClientMsg::AnswerInteraction {
                session_id: sref.id.clone(),
                interaction_id,
                result: Some(result),
                denied: false,
                message: None,
            },
        );
        self.clear_poll(&sref);
    }

    fn deny_poll(&mut self, ctx: &mut ViewCtx) {
        let Some(ui) = self.poll_ui.as_ref() else {
            return;
        };
        let Some(sref) = self.current_session_ref().cloned() else {
            return;
        };
        let interaction_id = ui.interaction_id.clone();
        self.answered_interactions.insert(interaction_id.clone());
        ctx.ws(
            sref.instance,
            WsClientMsg::AnswerInteraction {
                session_id: sref.id.clone(),
                interaction_id,
                result: None,
                denied: true,
                message: Some("Skipped by user.".into()),
            },
        );
        self.clear_poll(&sref);
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

    /// Total chat items (per-block count) for a given session.
    fn items_count_for(&self, sref: &SessionRef) -> usize {
        let h_count = self
            .history
            .get(sref)
            .map(|h| h.iter().map(|m| m.blocks.len()).sum::<usize>())
            .unwrap_or(0);
        let s_count = self
            .streaming
            .get(sref)
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
        let sref = match self.current_session_ref() {
            Some(s) => s.clone(),
            None => return,
        };
        if !self.can_load_more(&sref) {
            return;
        }
        let entry = self.history_load.entry(sref.clone()).or_default();
        let next_limit = if entry.limit == 0 {
            INITIAL_HISTORY_LIMIT
        } else {
            entry.limit + HISTORY_PAGE_SIZE
        };
        entry.limit = next_limit;
        entry.loading = true;
        ctx.http(
            sref.instance,
            HttpReq::GetMessages {
                session_id: sref.id,
                limit: next_limit,
            },
        );
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
    fn scroll_to_latest_unless_pinned(&mut self, sref: &SessionRef) {
        let total = self.items_count_for(sref);
        if total == 0 {
            return;
        }
        let key = SessionKey::Real(sref.clone());
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

    /// Insert pasted text into the composer, entering Insert mode. Ignored
    /// unless the input is already focused so a stray paste elsewhere is inert.
    pub fn paste_into_input(&mut self, text: &str) {
        if matches!(self.focus, FocusTier::Input | FocusTier::Insert) {
            self.input.insert_str(text);
            self.focus = FocusTier::Insert;
        }
    }

    fn send_input(&mut self, ctx: &mut ViewCtx) {
        let content = self.input.lines().join("\n");
        if content.trim().is_empty() {
            return;
        }
        match self.current.clone() {
            SessionKey::Real(sref) => {
                ctx.ws(
                    sref.instance,
                    WsClientMsg::Message {
                        session_id: sref.id.clone(),
                        content: content.clone(),
                        file_ids: None,
                        model: self.pending_model_override(),
                    },
                );
                // Optimistic local append.
                self.history
                    .entry(sref.clone())
                    .or_default()
                    .push(Message::new_user(sref.id.clone(), content));
                let key = SessionKey::Real(sref.clone());
                let last_idx = self.history[&sref].len().saturating_sub(1);
                let ui = self.ui_mut(&key);
                ui.follow_tail = true;
                ui.selected_block = Some(last_idx);
                // Float this chat to the top of the sidebar, as in the web.
                self.bump_session_to_front(&sref);
                self.drafts.clear(&self.current);
                self.input = TextArea::default();
                self.input
                    .set_cursor_line_style(ratatui::style::Style::default());
                self.focus = FocusTier::ChatBlocks;
            }
            SessionKey::NewChat => {
                // Lazy POST: create the session on the new-chat target instance,
                // carrying the typed message so it's sent as the first turn once
                // the id comes back. Clear the input now; `apply_session_created`
                // restores it if the POST fails.
                ctx.http(
                    self.new_chat_target,
                    HttpReq::CreateSession {
                        title: None,
                        content: Some(content),
                        backend: self.new_chat_backend.clone(),
                    },
                );
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
    /// Rebuild the current session's state from scratch (drops local caches,
    /// re-switches, refetches) — clears a stuck stream.
    Reload,
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
        // While actively typing or answering a poll, the view owns every key.
        // NOT on the focused-but-idle input: there `q`/`?`/`:` stay global
        // commands, and only the chars they don't claim fall through to start
        // typing (see `keys_input_focused`).
        matches!(
            self.focus,
            FocusTier::Insert
                | FocusTier::Poll
                | FocusTier::Files
                | FocusTier::NewChatPicker
                | FocusTier::ModelPicker
        )
    }

    fn handle_key(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        match self.focus {
            FocusTier::Sessions => self.keys_sessions(key, ctx),
            FocusTier::ChatBlocks => self.keys_chatblocks(key, ctx),
            FocusTier::BlockInterior => self.keys_block_interior(key, ctx),
            FocusTier::Input => self.keys_input_focused(key, ctx),
            FocusTier::Insert => self.keys_insert(key, ctx),
            FocusTier::Poll => self.keys_poll(key, ctx),
            FocusTier::Files => self.keys_files(key, ctx),
            FocusTier::NewChatPicker => self.keys_new_chat_setup(key, ctx),
            FocusTier::ModelPicker => self.keys_model_picker(key),
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
            // While a plan awaits approval, `a`/`d` answer it from the list.
            (KeyCode::Char('a'), _) if self.active_plan().is_some() => {
                self.answer_plan(true, ctx);
            }
            (KeyCode::Char('d'), _) if self.active_plan().is_some() => {
                self.answer_plan(false, ctx);
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
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.open_files_panel(ctx);
            }
            _ => {}
        }
    }

    fn keys_block_interior(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        // Keys move a line cursor inside the selected block. The renderer
        // clamps the cursor to [0, total_lines) on each frame; we don't have
        // the block's rendered line count here, so we just nudge by ± and
        // let render normalize.
        const PAGE: usize = 10;
        // Approve/decline a pending plan while reading it zoomed in.
        let plan_pending = self.active_plan().is_some();
        let key2 = self.current.clone();
        let ui = self.ui_mut(&key2);
        match (key.code, key.modifiers) {
            (KeyCode::Char('a'), _) if plan_pending => self.answer_plan(true, ctx),
            (KeyCode::Char('d'), _) if plan_pending => self.answer_plan(false, ctx),
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

    fn keys_input_focused(&mut self, key: KeyEvent, ctx: &mut ViewCtx) {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) | (KeyCode::Up, _) | (KeyCode::Left, _) => {
                self.focus = FocusTier::ChatBlocks;
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.sidebar_visible = !self.sidebar_visible;
            }
            (KeyCode::Enter, _) => self.enter_insert(),
            // Any printable key the global shortcuts don't claim (`q`/`?`/`:`
            // are intercepted before this) starts editing with that character.
            (KeyCode::Char(_), m) if !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) => {
                self.enter_insert();
                self.keys_insert(key, ctx);
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
        // Leaving the editor by walking off its top/left edge — Up on the first
        // line, or plain Left at the very start — drops to the block above the
        // input. Alt+Left is word-navigation, so it's excluded here.
        let at_top = matches!(key.code, KeyCode::Up) && self.input.at_top_line();
        let at_start = matches!(key.code, KeyCode::Left)
            && !key.modifiers.contains(KeyModifiers::ALT)
            && self.input.at_start();
        if at_top || at_start {
            self.exit_insert_to_blocks();
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

    const TEST_IDS: [InstanceId; 1] = [InstanceId::PRIMARY];

    /// Primary-instance session ref from a bare id.
    fn sref(id: &str) -> SessionRef {
        SessionRef::from(id)
    }

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
        v.scroll_to_latest_unless_pinned(&sref("s1"));
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
        v.scroll_to_latest_unless_pinned(&sref("s1"));
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
        v.scroll_to_latest_unless_pinned(&sref("s1"));
        let ui = &v.ui[&key];
        assert_eq!(ui.selected_block, Some(2));
        assert!(ui.follow_tail);
    }

    fn ctx(actions: &mut Vec<Action>) -> ViewCtx<'_> {
        ViewCtx {
            app_actions: actions,
            instances: &TEST_IDS,
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

    fn plan_pending(v: &mut ChatView, kind: &str) {
        v.current = SessionKey::Real("s1".into());
        // The plan no longer steals focus — it's a block in the list answered
        // from ChatBlocks, so navigation is never trapped.
        v.focus = FocusTier::ChatBlocks;
        v.history
            .insert("s1".into(), vec![assistant_with_blocks("s1", 1)]);
        v.pending_interaction.insert(
            "s1".into(),
            PendingInteraction {
                session_id: "s1".into(),
                interaction_id: "pi1".into(),
                interaction_type: kind.into(),
                tool_name: "ExitPlanMode".into(),
                tool_input: serde_json::json!({ "plan": "# Plan\n- step" }),
            },
        );
    }

    /// `a` in ChatBlocks approves a pending plan and clears the interaction.
    #[test]
    fn plan_approval_answers_and_clears() {
        let mut v = ChatView::new();
        plan_pending(&mut v, "plan_exit");
        assert!(v.active_plan().is_some());

        let mut actions = Vec::new();
        v.keys_chatblocks(
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            &mut ctx(&mut actions),
        );
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Ws {
                msg: WsClientMsg::AnswerInteraction { denied: false, .. },
                ..
            }
        )));
        assert!(v.active_plan().is_none());
    }

    /// `d` declines with a message; navigation keys never trigger approval.
    #[test]
    fn plan_decline_sends_denied() {
        let mut v = ChatView::new();
        plan_pending(&mut v, "plan_enter");
        let mut actions = Vec::new();
        v.keys_chatblocks(
            KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE),
            &mut ctx(&mut actions),
        );
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Ws {
                msg: WsClientMsg::AnswerInteraction {
                    denied: true,
                    message: Some(_),
                    ..
                },
                ..
            }
        )));
        assert!(v.active_plan().is_none());
    }

    /// A session paused on a plan shows the distinct WaitingPlan runtime.
    #[test]
    fn pending_plan_marks_session_waiting_plan() {
        let mut v = ChatView::new();
        plan_pending(&mut v, "plan_exit");
        assert_eq!(v.session_runtime(&sref("s1")), SessionRuntime::WaitingPlan);
    }

    /// Navigating (j/k) while a plan is pending does NOT answer it — the user
    /// can traverse history/other chats without responding.
    #[test]
    fn plan_pending_does_not_trap_navigation() {
        let mut v = ChatView::new();
        plan_pending(&mut v, "plan_exit");
        let mut actions = Vec::new();
        v.keys_chatblocks(
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
            &mut ctx(&mut actions),
        );
        assert!(
            !actions.iter().any(|a| matches!(
                a,
                Action::Ws {
                    msg: WsClientMsg::AnswerInteraction { .. },
                    ..
                }
            )),
            "navigation must not answer the plan"
        );
        assert!(v.active_plan().is_some(), "plan still pending");
    }

    /// A printable key on the focused input starts editing and types the char.
    #[test]
    fn input_tier_printable_starts_editing_with_char() {
        let mut v = ChatView::new();
        v.current = SessionKey::Real("s1".into());
        v.focus = FocusTier::Input;
        let mut actions = Vec::new();
        v.keys_input_focused(
            KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
            &mut ctx(&mut actions),
        );
        assert_eq!(v.focus, FocusTier::Insert);
        assert_eq!(v.input.lines().join("\n"), "h");
    }

    /// Esc on the focused input backs out to the message list.
    #[test]
    fn input_tier_esc_returns_to_blocks() {
        let mut v = ChatView::new();
        v.current = SessionKey::Real("s1".into());
        v.focus = FocusTier::Input;
        let mut actions = Vec::new();
        v.keys_input_focused(
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &mut ctx(&mut actions),
        );
        assert_eq!(v.focus, FocusTier::ChatBlocks);
    }

    /// Up on the editor's top line exits to the latest block.
    #[test]
    fn up_on_top_line_exits_editor_to_blocks() {
        let mut v = ChatView::new();
        v.current = SessionKey::Real("s1".into());
        v.history
            .insert("s1".into(), vec![assistant_with_blocks("s1", 3)]);
        v.focus = FocusTier::Insert;
        v.input = TextArea::new(vec!["hello".into()]);
        let mut actions = Vec::new();
        v.keys_insert(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut ctx(&mut actions),
        );
        assert_eq!(v.focus, FocusTier::ChatBlocks);
        assert_eq!(v.ui[&SessionKey::Real("s1".into())].selected_block, Some(2));
    }

    /// Left at the very start of the editor exits the same way.
    #[test]
    fn left_at_start_exits_editor_to_blocks() {
        let mut v = ChatView::new();
        v.current = SessionKey::Real("s1".into());
        v.history
            .insert("s1".into(), vec![assistant_with_blocks("s1", 2)]);
        v.focus = FocusTier::Insert;
        v.input = TextArea::default();
        let mut actions = Vec::new();
        v.keys_insert(
            KeyEvent::new(KeyCode::Left, KeyModifiers::NONE),
            &mut ctx(&mut actions),
        );
        assert_eq!(v.focus, FocusTier::ChatBlocks);
    }

    /// Up from a lower line keeps editing (cursor just moves up).
    #[test]
    fn up_below_top_line_keeps_editing() {
        let mut v = ChatView::new();
        v.current = SessionKey::Real("s1".into());
        v.focus = FocusTier::Insert;
        v.input = TextArea::new(vec!["one".into(), "two".into()]);
        let mut actions = Vec::new();
        v.keys_insert(
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
            &mut ctx(&mut actions),
        );
        assert_eq!(v.focus, FocusTier::Insert);
    }

    /// `:reload` drops the stuck stream's local state and refetches.
    #[test]
    fn reload_rebuilds_current_session_from_scratch() {
        let mut v = ChatView::new();
        v.current = SessionKey::Real("s1".into());
        v.sessions = vec![
            serde_json::from_value(serde_json::json!({ "id": "s1", "is_running": true })).unwrap(),
        ];
        v.streaming
            .insert("s1".into(), Message::new_streaming_assistant("s1".into()));
        v.agent_status.insert("s1".into(), AgentStatus::Writing);
        v.history
            .insert("s1".into(), vec![assistant_with_blocks("s1", 2)]);

        let actions = v.run_command(ChatCommand::Reload);

        assert!(
            !v.streaming.contains_key(&sref("s1")),
            "streaming buffer cleared"
        );
        assert!(matches!(
            v.agent_status.get(&sref("s1")),
            Some(AgentStatus::Idle)
        ));
        assert!(
            !v.history.contains_key(&sref("s1")),
            "history cleared for refetch"
        );
        assert!(!v.sessions[0].is_running, "is_running reset");
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Ws {
                msg: WsClientMsg::SwitchSession { .. },
                ..
            }
        )));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Http {
                req: HttpReq::GetMessages { .. },
                ..
            }
        )));
    }

    /// Sending a message floats that chat to the top of the sidebar list.
    #[test]
    fn sending_floats_session_to_top() {
        let mut v = ChatView::new();
        v.sessions = ["a", "b", "c"]
            .iter()
            .map(|id| serde_json::from_value(serde_json::json!({ "id": id })).unwrap())
            .collect();
        v.current = SessionKey::Real("c".into());
        v.focus = FocusTier::Insert;
        v.input = TextArea::new(vec!["hi".into()]);

        let mut actions = Vec::new();
        v.send_input(&mut ctx(&mut actions));

        assert_eq!(v.sessions[0].id, "c", "messaged chat floats to the top");
        assert_eq!(v.sessions_selected, 1);
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Ws {
                msg: WsClientMsg::Message { .. },
                ..
            }
        )));
    }

    /// Two instances may share a session id; their state stays separate.
    #[test]
    fn same_id_on_two_instances_is_isolated() {
        let mut v = ChatView::new();
        let a = SessionRef::new(InstanceId(0), "s1");
        let b = SessionRef::new(InstanceId(1), "s1");
        v.history
            .insert(a.clone(), vec![assistant_with_blocks("s1", 1)]);
        v.history
            .insert(b.clone(), vec![assistant_with_blocks("s1", 3)]);
        assert_eq!(v.history[&a][0].blocks.len(), 1);
        assert_eq!(v.history[&b][0].blocks.len(), 3);
    }

    /// The setup form: ↑↓ re-picks the instance in place, Enter drops into
    /// Insert; Esc returns to the sidebar.
    #[test]
    fn new_chat_picker_enter_commits_target_esc_cancels() {
        let ids = [InstanceId(0), InstanceId(1)];
        let mut v = ChatView::new();
        v.show_new_chat_setup(&ids);
        assert_eq!(v.focus, FocusTier::NewChatPicker);
        {
            let mut a = Vec::new();
            let mut c = ViewCtx {
                app_actions: &mut a,
                instances: &ids,
            };
            // ↓ re-picks in place — the answer commits without Enter.
            v.keys_new_chat_setup(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &mut c);
            assert_eq!(v.new_chat_target, InstanceId(1));
            v.keys_new_chat_setup(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &mut c);
        }
        assert_eq!(v.focus, FocusTier::Insert);

        v.show_new_chat_setup(&ids);
        {
            let mut a = Vec::new();
            let mut c = ViewCtx {
                app_actions: &mut a,
                instances: &ids,
            };
            v.keys_new_chat_setup(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &mut c);
        }
        assert_eq!(v.focus, FocusTier::Sessions);
    }

    /// Paste inserts multi-line text only when the composer is focused, and
    /// drops into Insert.
    #[test]
    fn paste_into_input_only_when_input_focused() {
        let mut v = ChatView::new();
        v.current = SessionKey::Real("s1".into());
        v.focus = FocusTier::ChatBlocks;
        v.paste_into_input("ignored");
        assert!(v.input.lines().iter().all(|l| l.is_empty()));

        v.focus = FocusTier::Input;
        v.paste_into_input("a\nb");
        assert_eq!(v.input.lines(), &["a".to_string(), "b".to_string()]);
        assert_eq!(v.focus, FocusTier::Insert);
    }

    fn model(id: &str, provider: &str, backend: &str) -> ModelOption {
        ModelOption {
            id: id.to_string(),
            provider: provider.to_string(),
            backend: backend.to_string(),
        }
    }

    /// Two backends (claude default), each serving two models.
    fn models_payload() -> ModelsPayload {
        use crate::api::types::BackendOption;
        let opt = |id: &str| BackendOption {
            id: id.to_string(),
            label: String::new(),
            available: true,
            reason: None,
        };
        ModelsPayload {
            default: "claude-sonnet-4-5".to_string(),
            defaults: [
                ("claude".to_string(), "claude-sonnet-4-5".to_string()),
                ("codex".to_string(), "gpt-5-codex".to_string()),
            ]
            .into_iter()
            .collect(),
            backends: Some(BackendsInfo {
                default: "claude".to_string(),
                options: vec![opt("claude"), opt("codex")],
            }),
            models: vec![
                model("claude-sonnet-4-5", "anthropic", "claude"),
                model("claude-opus-4-8", "anthropic", "claude"),
                model("gpt-5-codex", "openai", "codex"),
                model("gpt-5", "openai", "codex"),
            ],
        }
    }

    /// Picking a non-default model sets the backend's override and it rides
    /// the next message frame.
    #[test]
    fn model_picker_selects_override_and_send_includes_it() {
        let mut v = ChatView::new();
        v.apply_models_loaded(InstanceId::PRIMARY, Ok(models_payload()));
        v.current = SessionKey::Real("s1".into());
        v.focus = FocusTier::Input;

        v.show_model_picker();
        assert_eq!(v.focus, FocusTier::ModelPicker);
        assert_eq!(v.model_picker_backend, "claude");
        // Cursor starts on the default (idx 0); move down to opus, select.
        v.keys_model_picker(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        v.keys_model_picker(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(
            v.selected_models.get("claude").map(String::as_str),
            Some("claude-opus-4-8")
        );
        assert_eq!(v.focus, FocusTier::Insert);

        v.input = TextArea::new(vec!["hi".into()]);
        let mut actions = Vec::new();
        v.send_input(&mut ctx(&mut actions));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Ws { msg: WsClientMsg::Message { model: Some(m), .. }, .. }
                if m == "claude-opus-4-8"
        )));
    }

    /// Selecting the default model clears the override (no `model` sent).
    #[test]
    fn model_picker_default_selection_clears_override() {
        let mut v = ChatView::new();
        v.apply_models_loaded(InstanceId::PRIMARY, Ok(models_payload()));
        v.selected_models
            .insert("claude".to_string(), "claude-opus-4-8".to_string());
        v.current = SessionKey::Real("s1".into());
        v.focus = FocusTier::Input;

        v.show_model_picker();
        // Cursor starts on the current override (opus, idx 1); move up to the
        // default and select → override cleared.
        v.keys_model_picker(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        v.keys_model_picker(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert!(!v.selected_models.contains_key("claude"));
    }

    /// A single-model backend has nothing to pick — the picker never opens.
    #[test]
    fn model_picker_single_model_is_noop() {
        let mut v = ChatView::new();
        v.apply_models_loaded(
            InstanceId::PRIMARY,
            Ok(ModelsPayload {
                default: "claude-sonnet-4-5".to_string(),
                defaults: std::collections::HashMap::new(),
                backends: None,
                models: vec![model("claude-sonnet-4-5", "anthropic", "claude")],
            }),
        );
        v.current = SessionKey::Real("s1".into());
        v.focus = FocusTier::Input;
        v.show_model_picker();
        assert_eq!(v.focus, FocusTier::Input);
    }

    fn setup_key(v: &mut ChatView, code: KeyCode) {
        let mut a = Vec::new();
        let mut c = ViewCtx {
            app_actions: &mut a,
            instances: &TEST_IDS,
        };
        v.keys_new_chat_setup(KeyEvent::new(code, KeyModifiers::NONE), &mut c);
    }

    /// Setup form: answers commit in place, ← traverses back so earlier
    /// questions can be re-answered (re-deriving later columns), the create
    /// carries the backend, and the first message carries that backend's model.
    #[test]
    fn new_chat_setup_form_reanswers_and_creates() {
        let mut v = ChatView::new();
        v.apply_models_loaded(InstanceId::PRIMARY, Ok(models_payload()));
        v.current = SessionKey::NewChat;

        // Single instance → the form opens with backend + model columns.
        v.show_new_chat_setup(&TEST_IDS);
        assert_eq!(v.focus, FocusTier::NewChatPicker);
        assert_eq!(
            v.setup_cols(TEST_IDS.len()),
            vec![SetupCol::Backend, SetupCol::Model]
        );
        assert_eq!(v.setup_col, SetupCol::Backend);

        // Backend ↓ → codex; the model column re-derives to codex models.
        setup_key(&mut v, KeyCode::Down);
        assert_eq!(v.new_chat_backend.as_deref(), Some("codex"));
        assert_eq!(v.setup_model_index(), 0, "codex default selected");

        // → into the model column, ↓ → non-default codex model.
        setup_key(&mut v, KeyCode::Right);
        assert_eq!(v.setup_col, SetupCol::Model);
        setup_key(&mut v, KeyCode::Down);
        assert_eq!(
            v.selected_models.get("codex").map(String::as_str),
            Some("gpt-5")
        );

        // ← back to the backend column and re-answer: ↑ → claude (default);
        // the model column now shows claude models with its default selected.
        setup_key(&mut v, KeyCode::Left);
        assert_eq!(v.setup_col, SetupCol::Backend);
        setup_key(&mut v, KeyCode::Up);
        assert_eq!(v.new_chat_backend, None, "default backend = no override");
        assert_eq!(v.current_backend(), "claude");
        assert_eq!(v.setup_model_index(), 0, "claude default selected");

        // Re-answer once more back to codex, then start the chat.
        setup_key(&mut v, KeyCode::Down);
        assert_eq!(v.new_chat_backend.as_deref(), Some("codex"));
        setup_key(&mut v, KeyCode::Enter);
        assert_eq!(v.focus, FocusTier::Insert);

        // Send: the lazy create carries the backend.
        v.input = TextArea::new(vec!["hi".into()]);
        let mut actions = Vec::new();
        v.send_input(&mut ctx(&mut actions));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Http { req: HttpReq::CreateSession { backend: Some(b), .. }, .. }
                if b == "codex"
        )));

        // Materialization stamps the backend, the first message carries the
        // codex model override, and the pending choice resets.
        let created: Session = serde_json::from_value(serde_json::json!({ "id": "n1" })).unwrap();
        let mut actions = Vec::new();
        v.apply_session_created(
            InstanceId::PRIMARY,
            Some("hi".into()),
            Ok(created),
            &mut ctx(&mut actions),
        );
        assert_eq!(v.sessions[0].backend.as_deref(), Some("codex"));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Ws { msg: WsClientMsg::Message { model: Some(m), .. }, .. } if m == "gpt-5"
        )));
        assert_eq!(v.new_chat_backend, None);
    }

    /// An unavailable backend is shown but the cursor skips over it.
    #[test]
    fn setup_form_skips_unavailable_backend() {
        let mut payload = models_payload();
        if let Some(b) = payload.backends.as_mut() {
            b.options[1].available = false;
            b.options[1].reason = Some("codex binary not found".to_string());
        }
        let mut v = ChatView::new();
        v.apply_models_loaded(InstanceId::PRIMARY, Ok(payload));
        v.current = SessionKey::NewChat;

        v.show_new_chat_setup(&TEST_IDS);
        assert_eq!(v.setup_col, SetupCol::Backend);
        setup_key(&mut v, KeyCode::Down);
        assert_eq!(v.new_chat_backend, None, "unavailable codex skipped");
        setup_key(&mut v, KeyCode::Enter);
        assert_eq!(v.focus, FocusTier::Insert, "claude still selectable");
    }

    /// Sending routes the Message over the current session's instance, not
    /// always the primary.
    #[test]
    fn send_input_routes_to_current_instance() {
        let mut v = ChatView::new();
        v.current = SessionKey::Real(SessionRef::new(InstanceId(1), "x"));
        v.focus = FocusTier::Insert;
        v.input = TextArea::new(vec!["hi".into()]);
        let mut actions = Vec::new();
        v.send_input(&mut ctx(&mut actions));
        assert!(actions.iter().any(|a| matches!(
            a,
            Action::Ws { instance, msg: WsClientMsg::Message { .. } } if *instance == InstanceId(1)
        )));
    }
}
