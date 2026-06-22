//! Pre-TUI auth flow: probe `/api/auth/status`, prompt for password if needed,
//! call `/api/auth/login`, and return a shared [`AuthHandle`] that keeps the
//! password in memory so the token can be reissued automatically when it
//! expires.

use std::sync::Arc;

use crate::api::types::Token;
use crate::api::{AuthHandle, HttpClient};

/// Number of password prompts before we give up.
const MAX_ATTEMPTS: usize = 3;

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("connect failed: {0}")]
    Connect(String),
    #[error("authentication failed after {0} attempts")]
    BadPassword(usize),
    #[error("{0}")]
    Other(String),
}

pub async fn authenticate(server_url: &str) -> std::result::Result<Arc<AuthHandle>, AuthError> {
    println!("dogma — connecting to {server_url}");

    let probe = HttpClient::new_anonymous(server_url)
        .map_err(|e| AuthError::Other(format!("build http client: {e:#}")))?;

    let status = match probe.auth_status().await {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("{e:#}");
            return Err(AuthError::Connect(msg));
        }
    };

    if !status.auth_required {
        return Ok(AuthHandle::new(server_url, None, Token::empty()));
    }

    for attempt in 1..=MAX_ATTEMPTS {
        let pw = rpassword::prompt_password("password: ")
            .map_err(|e| AuthError::Other(format!("read password: {e}")))?;
        match probe.login(&pw).await {
            Ok(token) => {
                println!("authenticating... ok");
                // Keep the password so the token can be reissued on expiry.
                return Ok(AuthHandle::new(server_url, Some(pw), token));
            }
            Err(e) => {
                let msg = format!("{e:#}");
                if msg.contains("incorrect password") {
                    eprintln!("error: incorrect password ({attempt}/{MAX_ATTEMPTS})");
                    continue;
                }
                return Err(AuthError::Other(format!("login: {msg}")));
            }
        }
    }
    Err(AuthError::BadPassword(MAX_ATTEMPTS))
}
