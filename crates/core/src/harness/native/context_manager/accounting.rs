//! Hybrid token accounting: server-reported usage + 4-bytes/token estimates
//! for locally appended items (spec §6.1).

use serde_json::Value;

pub fn estimate_tokens(v: &Value) -> u64 {
    serde_json::to_string(v)
        .map(|s| s.len() as u64)
        .unwrap_or(0)
        / 4
}

#[derive(Default)]
pub struct TokenState {
    /// input + cache + output of the last committed provider response.
    pub last_server_total: Option<u64>,
    /// Estimate of items appended since the last committed response.
    pub local_appended: u64,
    /// Estimate of (system prompt + tools JSON), for the percent display.
    pub baseline: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub last_output: u64,
    // In-flight (uncommitted) response usage.
    pending_input: Option<u64>,
    pending_cache_read: u64,
    pending_cache_creation: u64,
    pending_output: u64,
}

impl TokenState {
    pub fn observe_start_usage(&mut self, usage: &Value) {
        let get = |k: &str| usage.get(k).and_then(|v| v.as_u64()).unwrap_or(0);
        let input = get("input_tokens");
        if input > 0 {
            self.pending_input = Some(input);
        }
        self.pending_cache_read = get("cache_read_input_tokens");
        self.pending_cache_creation = get("cache_creation_input_tokens");
    }

    pub fn observe_delta_usage(
        &mut self,
        output: i64,
        input: Option<i64>,
        cache_read: Option<i64>,
        cache_creation: Option<i64>,
    ) {
        if output > 0 {
            self.pending_output = output as u64;
        }
        // Terminal-delta input (OpenAI-format translation) is authoritative.
        if let Some(i) = input.filter(|v| *v > 0) {
            self.pending_input = Some(i as u64);
        }
        if let Some(c) = cache_read.filter(|v| *v > 0) {
            self.pending_cache_read = c as u64;
        }
        if let Some(c) = cache_creation.filter(|v| *v > 0) {
            self.pending_cache_creation = c as u64;
        }
    }

    /// Fold the in-flight response into the committed totals; local estimates
    /// restart from zero (the server total now covers everything sent).
    pub fn commit(&mut self) {
        if let Some(input) = self.pending_input.take() {
            self.cache_read = self.pending_cache_read;
            self.cache_creation = self.pending_cache_creation;
            self.last_output = self.pending_output;
            self.last_server_total =
                Some(input + self.cache_read + self.cache_creation + self.pending_output);
            self.local_appended = 0;
        } else if self.pending_output > 0 {
            // Output-only report (no input anywhere): best-effort add.
            self.last_output = self.pending_output;
            self.last_server_total =
                Some(self.last_server_total.unwrap_or(self.baseline) + self.pending_output);
            self.local_appended = 0;
        }
        self.pending_cache_read = 0;
        self.pending_cache_creation = 0;
        self.pending_output = 0;
    }

    /// Active context right now: last server truth + local additions, or the
    /// pure estimate when no response has completed yet.
    pub fn active(&self) -> u64 {
        self.last_server_total.unwrap_or(self.baseline) + self.local_appended
    }
}
