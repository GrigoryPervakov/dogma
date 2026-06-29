//! Instance identity — which Nerve server a connection, list row, or routed
//! action belongs to. dogma can drive several Nerve servers at once and merges
//! their resources into single lists, so every id is only unique *within* one
//! instance; `InstanceId` qualifies it.

/// Index into `App::instances`. `Copy` and cheap; default is the primary (0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstanceId(pub usize);

impl InstanceId {
    pub const PRIMARY: InstanceId = InstanceId(0);

    pub fn index(self) -> usize {
        self.0
    }
}

impl std::fmt::Display for InstanceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
