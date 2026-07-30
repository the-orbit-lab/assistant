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
#[derive(Debug, Clone)]
pub struct ConfirmationRequest {
    pub action: String,
    pub description: String,
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
/// prompt, an automatic denial in non-interactive mode, or a pre-supplied
/// approval list. The Action Runtime never assumes a particular UI.
pub trait ConfirmationProvider: Send + Sync {
    fn confirm(&self, request: &ConfirmationRequest) -> bool;
}

/// Always denies `ask` permissions. Safe default for non-interactive
/// contexts (e.g. the MCP server) that supplied no explicit approval.
pub struct AlwaysDeny;

impl ConfirmationProvider for AlwaysDeny {
    fn confirm(&self, _request: &ConfirmationRequest) -> bool {
        false
    }
}

/// Always approves `ask` permissions. Only meant for explicit,
/// user-supplied non-interactive approval (e.g. a CLI flag), never as a
/// silent default.
pub struct AlwaysAllow;

impl ConfirmationProvider for AlwaysAllow {
    fn confirm(&self, _request: &ConfirmationRequest) -> bool {
        true
    }
}
