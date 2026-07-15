//! HTTP client — typed wrappers around `/api/*`.
//!
//! The authenticated client carries an [`AuthHandle`] (server + stored
//! password + current token). Every request attaches the current token as a
//! bearer; on a `401` it re-logs-in once with the stored password and retries,
//! so sessions survive the 24h token expiry without a restart.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::json;
use tokio::sync::{Mutex, RwLock};

use crate::api::types::{MessagesPayload, Token};
use crate::model::{Message, Notification, Plan, Session, Skill, Task, Usage};

/// Shared, mutable authentication state. Holds the password in memory so the
/// token can be silently reissued when it expires. Cloned (via `Arc`) into the
/// HTTP worker and the WebSocket task.
pub struct AuthHandle {
    server: String,
    password: Option<String>,
    token: RwLock<Token>,
    reauth_lock: Mutex<()>,
}

impl AuthHandle {
    pub fn new(server: impl Into<String>, password: Option<String>, token: Token) -> Arc<Self> {
        Arc::new(Self {
            server: server.into(),
            password,
            token: RwLock::new(token),
            reauth_lock: Mutex::new(()),
        })
    }

    /// The current token.
    pub async fn token(&self) -> Token {
        self.token.read().await.clone()
    }

    /// Whether a fresh token can be obtained without user interaction.
    pub fn can_reauth(&self) -> bool {
        self.password.is_some()
    }

    /// Re-login with the stored password and store the new token. Single-flight:
    /// concurrent callers (HTTP worker + WS task) serialize on `reauth_lock`, so
    /// a token-expiry storm triggers at most one login at a time.
    pub async fn reauth(&self) -> Result<Token> {
        let _guard = self.reauth_lock.lock().await;
        let Some(pw) = self.password.as_deref() else {
            bail!("no stored password to re-authenticate");
        };
        let token = HttpClient::new_anonymous(&self.server)?.login(pw).await?;
        *self.token.write().await = token.clone();
        Ok(token)
    }
}

#[derive(Clone)]
pub struct HttpClient {
    base: String,
    inner: Client,
    auth: Option<Arc<AuthHandle>>,
}

#[derive(Debug, Deserialize)]
pub struct AuthStatus {
    #[serde(default)]
    pub auth_required: bool,
}

#[derive(Debug, Deserialize)]
struct LoginResp {
    token: String,
}

#[derive(Debug, Deserialize)]
struct MessagesResp {
    // Decoded per-element so one malformed message/block can't fail the whole
    // history (e.g. a different-version instance's schema quirk).
    #[serde(default)]
    messages: Vec<serde_json::Value>,
    #[serde(default)]
    last_usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct SessionsResp {
    sessions: Vec<Session>,
}

#[derive(Debug, Deserialize)]
struct TasksResp {
    tasks: Vec<Task>,
}

#[derive(Debug, Deserialize)]
struct PlansResp {
    plans: Vec<Plan>,
}

#[derive(Debug, Deserialize)]
struct SkillsResp {
    skills: Vec<Skill>,
}

#[derive(Debug, Deserialize)]
struct NotificationsResp {
    notifications: Vec<Notification>,
}

#[derive(Debug, Deserialize)]
struct ModifiedFilesResp {
    #[serde(default)]
    files: Vec<crate::model::ModifiedFile>,
}

fn build_inner() -> Result<Client> {
    Client::builder()
        .user_agent(concat!("dogma/", env!("CARGO_PKG_VERSION")))
        .build()
        .context("build http client")
}

/// Turn a non-2xx response into an error carrying the call name + status.
fn ensure_ok(resp: &Response, what: &str) -> Result<()> {
    if !resp.status().is_success() {
        bail!("{what}: HTTP {}", resp.status());
    }
    Ok(())
}

impl HttpClient {
    /// Build a client *without* auth. Used for the pre-TUI auth phase and for
    /// the re-login call inside [`AuthHandle::reauth`].
    pub fn new_anonymous(base: impl Into<String>) -> Result<Self> {
        Ok(Self {
            base: base.into(),
            inner: build_inner()?,
            auth: None,
        })
    }

    /// Build a client bound to a shared [`AuthHandle`]. The token is attached
    /// per-request (not baked into default headers) so reissued tokens take
    /// effect immediately.
    pub fn with_auth(base: impl Into<String>, auth: Arc<AuthHandle>) -> Result<Self> {
        Ok(Self {
            base: base.into(),
            inner: build_inner()?,
            auth: Some(auth),
        })
    }

    fn url(&self, path: &str) -> String {
        let base = self.base.trim_end_matches('/');
        format!("{base}{path}")
    }

    /// Send a request with the current bearer token. On `401`, re-authenticate
    /// once (with the stored password) and retry. `build` must produce a fresh
    /// `RequestBuilder` each call so the retry can re-issue the request.
    async fn send(&self, build: impl Fn(&Client) -> RequestBuilder) -> reqwest::Result<Response> {
        let token = match &self.auth {
            Some(a) => a.token().await,
            None => Token::empty(),
        };
        let req = build(&self.inner);
        let req = if token.is_empty() {
            req
        } else {
            req.bearer_auth(token.as_str())
        };
        let resp = req.send().await?;

        if resp.status() == StatusCode::UNAUTHORIZED
            && let Some(auth) = &self.auth
            && auth.can_reauth()
            && let Ok(fresh) = auth.reauth().await
        {
            return build(&self.inner).bearer_auth(fresh.as_str()).send().await;
        }
        Ok(resp)
    }

    /// Authenticated `GET path` decoded as `T`. `what` names the call for error
    /// context. Covers every plain read; callers needing query params or body
    /// mapping (messages) build their own request.
    async fn get_json<T: DeserializeOwned>(&self, path: &str, what: &str) -> Result<T> {
        let url = self.url(path);
        let resp = self
            .send(move |c| c.get(url.as_str()))
            .await
            .with_context(|| format!("GET {what}"))?;
        ensure_ok(&resp, what)?;
        resp.json::<T>()
            .await
            .with_context(|| format!("parse {what}"))
    }

    pub async fn auth_status(&self) -> Result<AuthStatus> {
        let resp = self
            .inner
            .get(self.url("/api/auth/status"))
            .send()
            .await
            .context("GET /api/auth/status")?;
        let status = resp.status();
        if !status.is_success() {
            bail!("auth/status: HTTP {status}");
        }
        resp.json::<AuthStatus>()
            .await
            .context("parse auth/status response")
    }

    /// Returns Ok(token) on success, Err on incorrect password.
    pub async fn login(&self, password: &str) -> Result<Token> {
        let resp = self
            .inner
            .post(self.url("/api/auth/login"))
            .json(&json!({ "password": password }))
            .send()
            .await
            .context("POST /api/auth/login")?;
        let status = resp.status();
        if status == StatusCode::UNAUTHORIZED {
            bail!("incorrect password");
        }
        if !status.is_success() {
            bail!("login: HTTP {status}");
        }
        let body: LoginResp = resp.json().await.context("parse login response")?;
        Ok(Token(body.token))
    }

    pub async fn list_sessions(&self) -> Result<Vec<Session>> {
        Ok(self
            .get_json::<SessionsResp>("/api/sessions", "list sessions")
            .await?
            .sessions)
    }

    pub async fn get_messages(&self, session_id: &str, limit: u32) -> Result<MessagesPayload> {
        let url = self.url(&format!("/api/sessions/{session_id}/messages"));
        let resp = self
            .send(move |c| c.get(url.as_str()).query(&[("limit", limit)]))
            .await
            .context("GET /api/sessions/{id}/messages")?;
        ensure_ok(&resp, "get messages")?;
        // Read the body once so a decode failure can surface what actually came
        // back (a 200 with an unexpected shape — e.g. a different Nerve version)
        // rather than an opaque "error decoding response body".
        let bytes = resp.bytes().await.context("read messages body")?;
        let body: MessagesResp = serde_json::from_slice(&bytes).map_err(|e| {
            let snippet: String = String::from_utf8_lossy(&bytes).chars().take(300).collect();
            anyhow::anyhow!("parse messages envelope: {e}; body starts: {snippet}")
        })?;
        // Decode each message on its own; skip (and log) any that don't fit the
        // model so a single bad row/block can't blank the whole conversation.
        let mut messages = Vec::with_capacity(body.messages.len());
        for raw in body.messages {
            match serde_json::from_value::<Message>(raw.clone()) {
                Ok(m) => messages.push(m.hydrate()),
                Err(e) => tracing::warn!(error = %e, raw = %raw, "skipping unparseable message"),
            }
        }
        Ok(MessagesPayload {
            messages,
            last_usage: body.last_usage,
        })
    }

    pub async fn create_session(
        &self,
        title: Option<&str>,
        backend: Option<&str>,
    ) -> Result<Session> {
        // "external" is the upstream bucket for satellite API clients (dogma,
        // Codex, etc.); upstream has no "tui" source, so reuse "external".
        let mut body = json!({ "source": "external" });
        if let Some(t) = title {
            body["title"] = json!(t);
        }
        if let Some(b) = backend {
            body["backend"] = json!(b);
        }
        let url = self.url("/api/sessions");
        let resp = self
            .send(move |c| c.post(url.as_str()).json(&body))
            .await
            .context("POST /api/sessions")?;
        ensure_ok(&resp, "create session")?;
        resp.json::<Session>()
            .await
            .context("parse created session")
    }

    // -------------------------------------------------------------------
    // Tasks / Plans / Skills — read-only for v1.
    // -------------------------------------------------------------------

    pub async fn list_tasks(&self) -> Result<Vec<Task>> {
        Ok(self
            .get_json::<TasksResp>("/api/tasks", "list tasks")
            .await?
            .tasks)
    }

    pub async fn get_task(&self, task_id: &str) -> Result<Task> {
        self.get_json(&format!("/api/tasks/{task_id}"), "get task")
            .await
    }

    pub async fn list_plans(&self) -> Result<Vec<Plan>> {
        Ok(self
            .get_json::<PlansResp>("/api/plans", "list plans")
            .await?
            .plans)
    }

    pub async fn get_plan(&self, plan_id: &str) -> Result<Plan> {
        self.get_json(&format!("/api/plans/{plan_id}"), "get plan")
            .await
    }

    /// Approve a pending plan (spawns an implementation session server-side).
    pub async fn approve_plan(&self, plan_id: &str) -> Result<()> {
        let url = self.url(&format!("/api/plans/{plan_id}/approve"));
        let resp = self
            .send(move |c| c.post(url.as_str()).json(&json!({})))
            .await
            .context("POST /api/plans/{id}/approve")?;
        ensure_ok(&resp, "approve plan")
    }

    /// Decline a plan via `PATCH` with `status=declined`.
    pub async fn decline_plan(&self, plan_id: &str) -> Result<()> {
        let url = self.url(&format!("/api/plans/{plan_id}"));
        let resp = self
            .send(move |c| c.patch(url.as_str()).json(&json!({ "status": "declined" })))
            .await
            .context("PATCH /api/plans/{id}")?;
        ensure_ok(&resp, "decline plan")
    }

    pub async fn list_skills(&self) -> Result<Vec<Skill>> {
        Ok(self
            .get_json::<SkillsResp>("/api/skills", "list skills")
            .await?
            .skills)
    }

    pub async fn get_skill(&self, skill_id: &str) -> Result<Skill> {
        self.get_json(&format!("/api/skills/{skill_id}"), "get skill")
            .await
    }

    pub async fn list_models(&self) -> Result<crate::api::types::ModelsPayload> {
        self.get_json("/api/models", "list models").await
    }

    // -------------------------------------------------------------------
    // Notifications — list + answer/dismiss polls.
    // -------------------------------------------------------------------

    pub async fn list_notifications(&self) -> Result<Vec<Notification>> {
        let url = self.url("/api/notifications");
        let resp = self
            .send(move |c| c.get(url.as_str()).query(&[("limit", 100)]))
            .await
            .context("GET /api/notifications")?;
        ensure_ok(&resp, "list notifications")?;
        let body: NotificationsResp = resp.json().await.context("parse notifications")?;
        Ok(body.notifications)
    }

    pub async fn answer_notification(&self, id: &str, answer: &str) -> Result<()> {
        let url = self.url(&format!("/api/notifications/{id}/answer"));
        let body = json!({ "answer": answer });
        let resp = self
            .send(move |c| c.post(url.as_str()).json(&body))
            .await
            .context("POST /api/notifications/{id}/answer")?;
        ensure_ok(&resp, "answer notification")
    }

    pub async fn dismiss_notification(&self, id: &str) -> Result<()> {
        let url = self.url(&format!("/api/notifications/{id}/dismiss"));
        let resp = self
            .send(move |c| c.post(url.as_str()))
            .await
            .context("POST /api/notifications/{id}/dismiss")?;
        ensure_ok(&resp, "dismiss notification")
    }

    // -------------------------------------------------------------------
    // Session file changes — modified-files list + per-file diff.
    // -------------------------------------------------------------------

    pub async fn modified_files(
        &self,
        session_id: &str,
    ) -> Result<Vec<crate::model::ModifiedFile>> {
        Ok(self
            .get_json::<ModifiedFilesResp>(
                &format!("/api/sessions/{session_id}/modified-files"),
                "modified files",
            )
            .await?
            .files)
    }

    pub async fn file_diff(&self, session_id: &str, path: &str) -> Result<crate::model::FileDiff> {
        let url = self.url(&format!("/api/sessions/{session_id}/file-diff"));
        let path = path.to_string();
        let resp = self
            .send(move |c| c.get(url.as_str()).query(&[("path", path.as_str())]))
            .await
            .context("GET /api/sessions/{id}/file-diff")?;
        ensure_ok(&resp, "file diff")?;
        resp.json::<crate::model::FileDiff>()
            .await
            .context("parse file diff")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn auth_handle_retains_password_for_reauth() {
        // With a password stored, the token can be reissued without prompting.
        let authed = AuthHandle::new("http://x", Some("secret".into()), Token("t1".into()));
        assert!(authed.can_reauth());
        assert_eq!(authed.token().await.as_str(), "t1");

        // No password (auth not required) — nothing to reissue.
        let anon = AuthHandle::new("http://x", None, Token::empty());
        assert!(!anon.can_reauth());
        assert!(anon.token().await.is_empty());
    }
}
