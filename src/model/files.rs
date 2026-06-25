//! Session file changes — `/api/sessions/{id}/modified-files` and
//! `/api/sessions/{id}/file-diff`. Wire-faithful with `nerve/gateway/diff.py`.

use serde::Deserialize;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
pub struct DiffStats {
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
}

/// One entry in the modified-files list (no diff body).
#[derive(Debug, Clone, Deserialize)]
pub struct ModifiedFile {
    pub path: String,
    #[serde(default)]
    pub short_path: String,
    /// "created" | "modified" | "deleted".
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub stats: DiffStats,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiffLine {
    /// "addition" | "deletion" | "context" | "info".
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub old_line: Option<u32>,
    #[serde(default)]
    pub new_line: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DiffHunk {
    #[serde(default)]
    pub header: String,
    #[serde(default)]
    pub lines: Vec<DiffLine>,
}

/// Full diff for one file (`GET …/file-diff?path=`).
#[derive(Debug, Clone, Deserialize)]
pub struct FileDiff {
    pub path: String,
    #[serde(default)]
    pub short_path: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub stats: DiffStats,
    #[serde(default)]
    pub hunks: Vec<DiffHunk>,
    #[serde(default)]
    pub truncated: bool,
}
