//! Skill — `/api/skills` items.

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    /// SQLite stores booleans as integers, so the API serializes `enabled`
    /// as `0`/`1`. Accept both integer and bool forms.
    #[serde(default = "default_true", deserialize_with = "de_loose_bool")]
    pub enabled: bool,
    /// SKILL.md body. Populated only by `GET /api/skills/{id}`.
    #[serde(default)]
    pub content: Option<String>,
    /// `total_invocations` on the list endpoint (nested under `stats` on detail).
    #[serde(default, alias = "total_invocations")]
    pub usage_count: Option<u64>,
    #[serde(default, alias = "last_used")]
    pub last_used_at: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Deserialize a bool from either a JSON bool or an integer (`0`/`1`), since
/// SQLite-backed rows surface booleans as integers. Null maps to `true`.
fn de_loose_bool<'de, D: Deserializer<'de>>(d: D) -> Result<bool, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolOrInt {
        Bool(bool),
        Int(i64),
    }
    Ok(match Option::<BoolOrInt>::deserialize(d)? {
        Some(BoolOrInt::Bool(b)) => b,
        Some(BoolOrInt::Int(n)) => n != 0,
        None => true,
    })
}

#[cfg(test)]
mod tests {
    use super::Skill;

    #[test]
    fn list_row_with_integer_enabled_and_stat_aliases() {
        // Shape from `GET /api/skills` (raw SQLite rows: enabled is 0/1).
        let json = r#"{
            "id": "nerve-dev", "name": "Nerve Development",
            "description": "dev skill", "version": "1.0.1",
            "enabled": 1, "total_invocations": 10, "last_used": "2026-06-22 11:56:03"
        }"#;
        let s: Skill = serde_json::from_str(json).expect("integer enabled must parse");
        assert!(s.enabled);
        assert_eq!(s.usage_count, Some(10));
        assert_eq!(s.last_used_at.as_deref(), Some("2026-06-22 11:56:03"));
    }

    #[test]
    fn disabled_integer_and_bool_forms() {
        let off: Skill = serde_json::from_str(r#"{"id":"a","enabled":0}"#).unwrap();
        assert!(!off.enabled);
        let on: Skill = serde_json::from_str(r#"{"id":"b","enabled":true}"#).unwrap();
        assert!(on.enabled);
        // absent → default true
        let def: Skill = serde_json::from_str(r#"{"id":"c"}"#).unwrap();
        assert!(def.enabled);
    }
}
