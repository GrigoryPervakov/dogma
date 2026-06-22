//! Usage stats — fed to the context-usage widget.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_creation_input_tokens: u64,
    #[serde(default)]
    pub cache_read_input_tokens: u64,
    /// `/api/sessions/{id}/messages.last_usage` nests these inside `usage`,
    /// while `WsServerMsg::Done` carries them top-level — capture both.
    #[serde(default)]
    pub max_context_tokens: Option<u64>,
    #[serde(default)]
    pub num_turns: Option<u32>,
}

impl Usage {
    /// Sum of all input-side token counts. Note: this aggregates across
    /// every API sub-call the SDK made for this conversational turn, so
    /// for context-window occupancy you want `ContextUsage::estimated_context_tokens`.
    pub fn total_input(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.cache_creation_input_tokens)
            .saturating_add(self.cache_read_input_tokens)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ContextUsage {
    pub last: Option<Usage>,
    pub max_context_tokens: Option<u64>,
    pub num_turns: Option<u32>,
    pub session_cost_usd: f64,
}

impl ContextUsage {
    /// Estimate the context-window occupancy of the most recent API sub-call.
    ///
    /// The SDK's `ResultMessage.usage` aggregates tokens across ALL API
    /// sub-calls in a turn (each tool use triggers a new call). So
    /// `cache_read` can be N× the context window — it's summed across
    /// calls, not one call's context. Divide total input by `num_turns`
    /// (the call count) to get per-call occupancy. Mirrors
    /// `web/src/components/Chat/ContextBar.tsx`.
    pub fn estimated_context_tokens(&self) -> Option<u64> {
        let usage = self.last.as_ref()?;
        let total_input = usage.total_input();
        if total_input == 0 {
            return None;
        }
        let max = self.max_context_tokens.unwrap_or(200_000);
        let n = self.num_turns.unwrap_or(0) as u64;
        let num_calls = if n > 0 {
            n
        } else if max > 0 {
            // Fallback heuristic: ceil(total / max).
            let ceil = total_input.div_ceil(max);
            ceil.max(1)
        } else {
            1
        };
        Some(total_input / num_calls)
    }
}
