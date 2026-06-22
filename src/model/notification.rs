//! Notification — `/api/notifications` items, including answerable polls
//! (`type == "question"` and `propose_action`, both carrying `options`).

use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct Notification {
    pub id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub status: String,
    /// Poll choices. The API serializes this as a JSON-encoded string
    /// (`"[\"Red\",\"Green\"]"`) or null; both `question` labels and
    /// `propose_action` values land here as a flat string list.
    #[serde(default, deserialize_with = "options_from_json")]
    pub options: Vec<String>,
    #[serde(default)]
    pub answer: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub session_title: Option<String>,
}

impl Notification {
    pub fn is_pending(&self) -> bool {
        self.status == "pending"
    }

    /// A pending notification with options the user can pick from.
    pub fn answerable(&self) -> bool {
        self.is_pending() && !self.options.is_empty()
    }
}

fn options_from_json<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    let out = match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) if !s.is_empty() => {
            serde_json::from_str::<Vec<String>>(&s).unwrap_or_default()
        }
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect(),
        _ => Vec::new(),
    };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::Notification;
    use serde_json::json;

    fn parse(v: serde_json::Value) -> Notification {
        serde_json::from_value(v).expect("notification parse")
    }

    #[test]
    fn parses_json_string_options_and_is_answerable() {
        let n = parse(json!({
            "id": "ask-1", "type": "question", "status": "pending",
            "options": "[\"Red\", \"Green\", \"Blue\"]"
        }));
        assert_eq!(n.options, vec!["Red", "Green", "Blue"]);
        assert!(n.answerable());
    }

    #[test]
    fn null_options_yield_empty_not_answerable() {
        let n = parse(json!({
            "id": "notif-1", "type": "notify", "status": "pending", "options": null
        }));
        assert!(n.options.is_empty());
        assert!(!n.answerable());
    }

    #[test]
    fn answered_poll_is_not_answerable() {
        let n = parse(json!({
            "id": "a", "type": "question", "status": "answered",
            "options": "[\"x\"]", "answer": "x"
        }));
        assert!(!n.answerable());
    }
}
