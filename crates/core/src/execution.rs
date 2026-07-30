use std::time::{Duration, SystemTime};

use crate::permission::PermissionOutcome;

/// A record of a single action execution, kept for session history and
/// `orbit --json` output. Never holds secrets or full sensitive payloads —
/// only the action name, timing, and outcome.
#[derive(Debug, Clone)]
pub struct ExecutionRecord {
    pub action: String,
    pub permission_outcome: PermissionOutcome,
    pub started_at: SystemTime,
    pub finished_at: SystemTime,
    pub success: bool,
    pub error_summary: Option<String>,
}

impl ExecutionRecord {
    pub fn duration(&self) -> Duration {
        self.finished_at
            .duration_since(self.started_at)
            .unwrap_or_default()
    }
}
