//! Wire types — exact 1:1 with Nerve's WS + REST schemas.
//!
//! Nerve's web-frontend WS handlers are the source of truth for these shapes.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{Message, Notification, Plan, Session, Skill, Task, Usage};

// ---------------------------------------------------------------------------
// Auth token (NewType so Debug can redact it)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Token(pub String);

impl std::fmt::Debug for Token {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Token([REDACTED])")
    }
}

impl Token {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn empty() -> Self {
        Self(String::new())
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

// ---------------------------------------------------------------------------
// WebSocket — server → client
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsServerMsg {
    SessionSwitched {
        session_id: String,
    },
    SessionStatus {
        session_id: String,
        #[serde(default)]
        is_running: bool,
        #[serde(default)]
        status: Option<String>,
        #[serde(default)]
        buffered_events: Vec<WsServerMsg>,
    },
    Token {
        session_id: String,
        content: String,
        #[serde(default)]
        parent_tool_use_id: Option<String>,
    },
    Thinking {
        session_id: String,
        content: String,
        #[serde(default)]
        parent_tool_use_id: Option<String>,
    },
    ToolUse {
        session_id: String,
        tool: String,
        #[serde(default)]
        input: Value,
        #[serde(default)]
        tool_use_id: Option<String>,
        #[serde(default)]
        parent_tool_use_id: Option<String>,
    },
    ToolResult {
        session_id: String,
        #[serde(default)]
        tool_use_id: Option<String>,
        #[serde(default)]
        result: String,
        #[serde(default)]
        is_error: Option<bool>,
        #[serde(default)]
        parent_tool_use_id: Option<String>,
    },
    Done {
        session_id: String,
        #[serde(default)]
        usage: Option<Usage>,
        #[serde(default)]
        max_context_tokens: Option<u64>,
        #[serde(default)]
        num_turns: Option<u32>,
    },
    Stopped {
        session_id: String,
    },
    Error {
        session_id: String,
        error: String,
    },
    SessionUpdated {
        session_id: String,
        #[serde(default)]
        title: Option<String>,
    },
    SessionForked {
        source_id: String,
        fork_id: String,
        #[serde(default)]
        title: Option<String>,
    },
    SessionResumed {
        session_id: String,
    },
    SessionArchived {
        session_id: String,
    },
    SessionRunning {
        session_id: String,
        is_running: bool,
    },
    AnswerInjected {
        session_id: String,
        notification_id: String,
        title: String,
        answer: String,
        answered_by: String,
        content: String,
    },
    SubagentStart {
        session_id: String,
        tool_use_id: String,
        subagent_type: String,
        description: String,
        #[serde(default)]
        model: Option<String>,
    },
    SubagentComplete {
        session_id: String,
        tool_use_id: String,
        duration_ms: u64,
        #[serde(default)]
        is_error: Option<bool>,
    },
    PlanUpdate {
        session_id: String,
        content: String,
    },
    HoaProgress {
        session_id: String,
        event: Value,
    },
    Interaction {
        session_id: String,
        interaction_id: String,
        interaction_type: String,
        tool_name: String,
        tool_input: Value,
    },
    FileChanged {
        session_id: String,
        path: String,
        operation: String,
        tool_use_id: String,
    },
    BackgroundTasksUpdate {
        session_id: String,
        #[serde(default)]
        tasks: Vec<Value>,
    },
    Notification(Value),
    NotificationAnswered(Value),
    Pong,
    /// Unknown variant — caught here so the WS reader doesn't crash.
    /// Logged by the reducer but otherwise ignored.
    #[serde(other)]
    Unknown,
}

impl WsServerMsg {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            WsServerMsg::SessionSwitched { session_id }
            | WsServerMsg::SessionStatus { session_id, .. }
            | WsServerMsg::Token { session_id, .. }
            | WsServerMsg::Thinking { session_id, .. }
            | WsServerMsg::ToolUse { session_id, .. }
            | WsServerMsg::ToolResult { session_id, .. }
            | WsServerMsg::Done { session_id, .. }
            | WsServerMsg::Stopped { session_id }
            | WsServerMsg::Error { session_id, .. }
            | WsServerMsg::SessionUpdated { session_id, .. }
            | WsServerMsg::SessionResumed { session_id }
            | WsServerMsg::SessionArchived { session_id }
            | WsServerMsg::SessionRunning { session_id, .. }
            | WsServerMsg::AnswerInjected { session_id, .. }
            | WsServerMsg::SubagentStart { session_id, .. }
            | WsServerMsg::SubagentComplete { session_id, .. }
            | WsServerMsg::PlanUpdate { session_id, .. }
            | WsServerMsg::HoaProgress { session_id, .. }
            | WsServerMsg::Interaction { session_id, .. }
            | WsServerMsg::FileChanged { session_id, .. }
            | WsServerMsg::BackgroundTasksUpdate { session_id, .. } => Some(session_id),
            WsServerMsg::SessionForked { fork_id, .. } => Some(fork_id),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// WebSocket — client → server
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WsClientMsg {
    Message {
        session_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_ids: Option<Vec<String>>,
    },
    Stop {
        session_id: String,
    },
    SwitchSession {
        session_id: String,
    },
    Fork {
        session_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        at_message_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
    },
    Resume {
        session_id: String,
    },
    AnswerInteraction {
        session_id: String,
        interaction_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        denied: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Ping,
}

// ---------------------------------------------------------------------------
// WebSocket connection-state events (driven by the WS task itself, not the
// server). Routed to the App as `AppEvent::WireConn`.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum WsConnEvent {
    Connecting,
    Connected,
    Disconnected {
        reason: String,
        retry_in_ms: Option<u64>,
    },
    AuthRejected,
}

// ---------------------------------------------------------------------------
// HTTP requests / results— async via the http worker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum HttpReq {
    ListSessions,
    GetMessages {
        session_id: String,
        limit: u32,
    },
    CreateSession {
        title: Option<String>,
        /// First message to send once the session exists (lazy new-chat flow).
        content: Option<String>,
    },

    ListTasks,
    GetTask {
        task_id: String,
    },

    ListPlans,
    GetPlan {
        plan_id: String,
    },

    ListSkills,
    GetSkill {
        skill_id: String,
    },

    ListNotifications,
    AnswerNotification {
        id: String,
        answer: String,
    },
    DismissNotification {
        id: String,
    },

    GetModifiedFiles {
        session_id: String,
    },
    GetFileDiff {
        session_id: String,
        path: String,
    },
}

/// Body returned from `GET /api/sessions/{id}/messages` after decoding.
#[derive(Debug, Clone)]
pub struct MessagesPayload {
    pub messages: Vec<Message>,
    pub last_usage: Option<Usage>,
}

#[derive(Debug, Clone)]
pub struct HttpResult {
    pub kind: HttpResultKind,
}

#[derive(Debug, Clone)]
pub enum HttpResultKind {
    Sessions(Result<Vec<Session>, String>),
    Messages {
        session_id: String,
        limit: u32,
        result: Result<MessagesPayload, String>,
    },
    SessionCreated {
        pending_content: Option<String>,
        result: Result<Session, String>,
    },
    Tasks(Result<Vec<Task>, String>),
    TaskDetail {
        task_id: String,
        result: Result<Task, String>,
    },
    Plans(Result<Vec<Plan>, String>),
    PlanDetail {
        plan_id: String,
        result: Result<Plan, String>,
    },
    Skills(Result<Vec<Skill>, String>),
    SkillDetail {
        skill_id: String,
        result: Result<Skill, String>,
    },
    Notifications(Result<Vec<Notification>, String>),
    NotificationAnswered {
        id: String,
        answer: String,
        result: Result<(), String>,
    },
    NotificationDismissed {
        id: String,
        result: Result<(), String>,
    },
    ModifiedFiles {
        session_id: String,
        result: Result<Vec<crate::model::ModifiedFile>, String>,
    },
    FileDiff {
        session_id: String,
        path: String,
        result: Result<crate::model::FileDiff, String>,
    },
}
