use serde::{Deserialize, Serialize};

/// The effective permission for a native or MCP-exported action.
///
/// The model can never change this value: it is read from
/// `.orbit/project.yaml` by application code and enforced by the Action
/// Runtime before an action ever executes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// Execute without additional confirmation.
    Allow,
    /// Require explicit user confirmation before executing.
    Ask,
    /// Never execute.
    Deny,
}

impl std::fmt::Display for Permission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Permission::Allow => "allow",
            Permission::Ask => "ask",
            Permission::Deny => "deny",
        };
        write!(f, "{s}")
    }
}

/// A request to confirm a single `ask`-permission action, handed to whatever
/// front end (CLI prompt, MCP host, future GUI) is driving the session.
///
/// `project` and `arguments_summary` exist so a front end can show *what*
/// is about to happen and *where*, not just which action was named.
/// `arguments_summary` is produced by
/// [`crate::event::summarize_arguments`]: secret-shaped values are
/// redacted and absolute paths shortened, so it is safe to display and
/// log. The action itself always receives the original, unmodified input.
#[derive(Debug, Clone)]
pub struct ConfirmationRequest {
    /// Correlates this request with the
    /// [`crate::event::EventPayload::PermissionRequired`] event the Action
    /// Runtime emits for it, and with the decision that answers it. A
    /// front end that resolves permissions asynchronously (the JSONL
    /// bridge, a future GUI) keys its pending-request table on this.
    pub request_id: crate::event::PermissionRequestId,
    pub action: String,
    pub description: String,
    /// The project the action will run against, when one is known.
    pub project: Option<String>,
    pub arguments_summary: String,
}

/// How permission was resolved for a single execution attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionOutcome {
    Allowed,
    ConfirmedByUser,
    DeniedByConfig,
    DeniedByUser,
    /// The request never reached a permission check (unknown action or
    /// invalid input).
    NotApplicable,
}

/// Resolves `ask` permissions at execution time.
///
/// Implementations decide how confirmation is obtained: an interactive CLI
/// prompt, an automatic denial in non-interactive mode, a pre-supplied
/// approval flag, or — for a session driven by an external UI — waiting
/// for a structured decision to arrive over a protocol. The Action Runtime
/// never assumes a particular front end.
///
/// This is `async` because obtaining a decision genuinely is: a session
/// must be able to *pause* an action, emit
/// [`crate::event::EventPayload::PermissionRequired`], and resume only
/// once a real approval or denial arrives, without blocking the runtime
/// thread that the rest of the session is running on.
#[async_trait::async_trait]
pub trait ConfirmationProvider: Send + Sync {
    async fn confirm(&self, request: &ConfirmationRequest) -> bool;
}

/// Always denies `ask` permissions. Safe default for non-interactive
/// contexts (e.g. the MCP server) that supplied no explicit approval.
pub struct AlwaysDeny;

#[async_trait::async_trait]
impl ConfirmationProvider for AlwaysDeny {
    async fn confirm(&self, _request: &ConfirmationRequest) -> bool {
        false
    }
}

/// Always approves `ask` permissions. Only meant for explicit,
/// user-supplied non-interactive approval (e.g. a CLI flag), never as a
/// silent default.
pub struct AlwaysAllow;

#[async_trait::async_trait]
impl ConfirmationProvider for AlwaysAllow {
    async fn confirm(&self, _request: &ConfirmationRequest) -> bool {
        true
    }
}
