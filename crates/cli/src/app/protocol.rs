//! The `orbit app serve --jsonl` wire contract.
//!
//! One JSON object per line in each direction: commands on stdin,
//! [`orbit_core::AgentEvent`] frames and the notices below on stdout.
//! Diagnostics never appear on stdout — they go to stderr — so a client
//! can parse every stdout line as JSON without filtering.
//!
//! The protocol is versioned by [`orbit_core::EVENT_PROTOCOL_VERSION`],
//! reported in the `ready` frame at startup and in `session_started`.

use serde::{Deserialize, Serialize};

/// How a session should answer `ask` permissions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Emit `permission_required` and wait for `permission.resolve`.
    #[default]
    External,
    /// Approve automatically. The client is asserting it has already
    /// obtained the user's consent.
    AllowAll,
    /// Refuse automatically, for a client that cannot prompt.
    DenyAll,
}

/// A command from the client.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum BridgeRequest {
    /// Open a session. Exactly one of `workspace` or `project` may be
    /// given; with neither, Orbit discovers one from the current
    /// directory using the ordinary precedence rules.
    #[serde(rename = "session.start")]
    SessionStart {
        #[serde(default)]
        workspace: Option<String>,
        #[serde(default)]
        project: Option<String>,
        #[serde(default)]
        streaming: Option<bool>,
        #[serde(default)]
        permissions: Option<PermissionMode>,
    },
    #[serde(rename = "message.send")]
    MessageSend { session_id: String, text: String },
    #[serde(rename = "permission.resolve")]
    PermissionResolve {
        request_id: String,
        decision: PermissionDecisionInput,
    },
    /// Cancel the turn currently running in a session. `execution_id` is
    /// accepted for symmetry with the events but is not required: a
    /// session runs one turn at a time, so the session alone identifies
    /// what to cancel.
    #[serde(rename = "execution.cancel")]
    ExecutionCancel {
        session_id: String,
        #[serde(default)]
        execution_id: Option<String>,
    },
    /// Replace the active project set (workspace sessions only).
    #[serde(rename = "projects.set")]
    ProjectsSet {
        session_id: String,
        projects: Vec<String>,
    },
    #[serde(rename = "projects.list")]
    ProjectsList { session_id: String },
    #[serde(rename = "session.status")]
    SessionStatus { session_id: String },
    #[serde(rename = "session.end")]
    SessionEnd { session_id: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecisionInput {
    AllowOnce,
    DenyOnce,
}

impl From<PermissionDecisionInput> for orbit_core::PermissionDecision {
    fn from(value: PermissionDecisionInput) -> Self {
        match value {
            PermissionDecisionInput::AllowOnce => orbit_core::PermissionDecision::AllowOnce,
            PermissionDecisionInput::DenyOnce => orbit_core::PermissionDecision::DenyOnce,
        }
    }
}

/// Machine-readable error categories, so a client can branch without
/// matching on prose.
pub mod error_code {
    /// The line was not valid JSON.
    pub const MALFORMED_JSON: &str = "malformed_json";
    /// Valid JSON, but not a known request shape.
    pub const UNKNOWN_REQUEST: &str = "unknown_request";
    /// `session_id` does not name a live session.
    pub const UNKNOWN_SESSION: &str = "unknown_session";
    /// The request was valid but could not be carried out.
    pub const REQUEST_FAILED: &str = "request_failed";
    /// A session could not be opened (no project/workspace, bad config).
    pub const SESSION_START_FAILED: &str = "session_start_failed";
    /// Nothing was running to cancel.
    pub const NOTHING_TO_CANCEL: &str = "nothing_to_cancel";
    /// The permission request id is not pending.
    pub const UNKNOWN_PERMISSION_REQUEST: &str = "unknown_permission_request";
}

/// A non-event frame written to stdout.
///
/// Shares the `type` namespace with [`orbit_core::AgentEvent`]; these
/// names are deliberately distinct from every event name so one stream
/// can carry both.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BridgeFrame {
    /// Sent once at startup, before any command is read.
    Ready {
        protocol_version: u32,
    },
    Error {
        code: &'static str,
        message: String,
    },
    /// A request succeeded but produced no events of its own.
    Ack {
        request: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    Status {
        session_id: String,
        mode: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        active_projects: Vec<String>,
        turns: u64,
        message_count: usize,
        source_count: usize,
        action_count: usize,
        command_run_count: usize,
        running: bool,
        pending_permissions: Vec<String>,
        streaming: bool,
    },
    Projects {
        session_id: String,
        projects: Vec<ProjectSummary>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ProjectSummary {
    pub name: String,
    pub available: bool,
    pub aliases: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Parse one line of client input.
///
/// A malformed line is an error frame, never a reason to exit: one bad
/// message from a client must not take down a session that is working.
/// The error is boxed because [`BridgeFrame`] is much larger than a
/// request: the common `Ok` path should not pay for the size of a frame
/// that is only built when something is wrong.
pub fn parse_request(line: &str) -> Result<BridgeRequest, Box<BridgeFrame>> {
    let trimmed = line.trim();
    let value: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
        Box::new(BridgeFrame::Error {
            code: error_code::MALFORMED_JSON,
            message: format!("could not parse JSON: {e}"),
        })
    })?;

    serde_json::from_value(value).map_err(|e| {
        Box::new(BridgeFrame::Error {
            code: error_code::UNKNOWN_REQUEST,
            message: e.to_string(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_requests() {
        assert_eq!(
            parse_request(r#"{"type":"session.start","workspace":"/path/to/orbit-lab"}"#).unwrap(),
            BridgeRequest::SessionStart {
                workspace: Some("/path/to/orbit-lab".to_string()),
                project: None,
                streaming: None,
                permissions: None,
            }
        );
        assert_eq!(
            parse_request(r#"{"type":"message.send","session_id":"s1","text":"Why STM32?"}"#)
                .unwrap(),
            BridgeRequest::MessageSend {
                session_id: "s1".to_string(),
                text: "Why STM32?".to_string(),
            }
        );
        assert_eq!(
            parse_request(
                r#"{"type":"permission.resolve","request_id":"p1","decision":"allow_once"}"#
            )
            .unwrap(),
            BridgeRequest::PermissionResolve {
                request_id: "p1".to_string(),
                decision: PermissionDecisionInput::AllowOnce,
            }
        );
        assert_eq!(
            parse_request(r#"{"type":"execution.cancel","session_id":"s1"}"#).unwrap(),
            BridgeRequest::ExecutionCancel {
                session_id: "s1".to_string(),
                execution_id: None,
            }
        );
        assert_eq!(
            parse_request(r#"{"type":"session.end","session_id":"s1"}"#).unwrap(),
            BridgeRequest::SessionEnd {
                session_id: "s1".to_string(),
            }
        );
    }

    #[test]
    fn execution_cancel_accepts_an_execution_id() {
        assert_eq!(
            parse_request(
                r#"{"type":"execution.cancel","session_id":"s1","execution_id":"exec-3"}"#
            )
            .unwrap(),
            BridgeRequest::ExecutionCancel {
                session_id: "s1".to_string(),
                execution_id: Some("exec-3".to_string()),
            }
        );
    }

    #[test]
    fn malformed_json_is_a_structured_error_not_a_panic() {
        let frame = *parse_request("{not json").unwrap_err();
        match frame {
            BridgeFrame::Error { code, .. } => assert_eq!(code, error_code::MALFORMED_JSON),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn an_unknown_request_type_is_reported() {
        let frame = *parse_request(r#"{"type":"does.not.exist"}"#).unwrap_err();
        match frame {
            BridgeFrame::Error { code, .. } => assert_eq!(code, error_code::UNKNOWN_REQUEST),
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A missing required field must be reported, not defaulted -- sending
    /// a message to an unnamed session should never silently pick one.
    #[test]
    fn a_missing_required_field_is_reported() {
        let frame = *parse_request(r#"{"type":"message.send","text":"hi"}"#).unwrap_err();
        match frame {
            BridgeFrame::Error { code, message } => {
                assert_eq!(code, error_code::UNKNOWN_REQUEST);
                assert!(message.contains("session_id"), "{message}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// Typos in field names must not be silently ignored.
    #[test]
    fn unknown_fields_are_rejected() {
        let frame =
            parse_request(r#"{"type":"message.send","session_id":"s","text":"x","txet":"y"}"#)
                .unwrap_err();
        assert!(matches!(*frame, BridgeFrame::Error { .. }));
    }

    #[test]
    fn bridge_frames_serialize_with_distinct_type_names() {
        let ready = serde_json::to_value(BridgeFrame::Ready {
            protocol_version: 1,
        })
        .unwrap();
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["protocol_version"], 1);

        let error = serde_json::to_value(BridgeFrame::Error {
            code: error_code::UNKNOWN_SESSION,
            message: "no such session".to_string(),
        })
        .unwrap();
        assert_eq!(error["type"], "error");
        assert_eq!(error["code"], "unknown_session");
    }
}
