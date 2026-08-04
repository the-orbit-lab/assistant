//! The Agent Event Stream: a provider-independent, protocol-independent
//! description of what a session is actually doing.
//!
//! Every front end — the interactive CLI renderer, the JSON Lines
//! application bridge, a future SwiftUI client — observes the *same*
//! events. Agent and action logic lives in `orbit-agent`/`orbit-actions`
//! and emits through [`EventSink`]; no renderer or wire protocol ever
//! contains agent logic, and no agent code knows how events are
//! displayed.
//!
//! Two rules constrain what may appear here:
//!
//! 1. **Events describe real work.** Each one corresponds to a request
//!    actually made, an action actually executed, or bytes actually
//!    received from a provider. There are no decorative "thinking"
//!    events.
//! 2. **Events are safe to display and log.** Action arguments are
//!    summarized through [`summarize_arguments`], which redacts
//!    secret-shaped values and shortens absolute paths, so an event
//!    stream never carries credentials or a user's full directory
//!    layout.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::source::SourceReference;

/// Version of the event/JSONL wire contract. Increment on any
/// backwards-incompatible change to event names or payload fields.
pub const EVENT_PROTOCOL_VERSION: u32 = 1;

/// Unix milliseconds. Events are timestamped in wall-clock time because
/// their consumers (a log, a UI transcript) are wall-clock things; the
/// monotonic durations used for `ActionCompleted` come from
/// [`crate::ExecutionRecord`] instead.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

fn next_global() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(1);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Identifies one session for the lifetime of the process that owns it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub String);

impl SessionId {
    /// Unique within a process by construction (a monotonic counter), and
    /// unique in practice across processes on a host (start time + pid).
    pub fn generate() -> Self {
        SessionId(format!(
            "sess-{:x}-{:x}-{:04x}",
            now_ms(),
            std::process::id(),
            next_global()
        ))
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One user request and everything done to answer it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TurnId(pub String);

impl TurnId {
    pub fn new(sequence: u64) -> Self {
        TurnId(format!("turn-{sequence}"))
    }
}

impl std::fmt::Display for TurnId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// One unit of cancellable/observable work inside a turn: a single action
/// execution, or the model generation itself.
///
/// Distinct from [`TurnId`] specifically so that concurrent read-only
/// actions can be correlated later without a protocol change. Orbit does
/// not run actions concurrently today.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExecutionId(pub String);

impl ExecutionId {
    pub fn new(sequence: u64) -> Self {
        ExecutionId(format!("exec-{sequence}"))
    }
}

impl std::fmt::Display for ExecutionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Correlates a [`EventPayload::PermissionRequired`] with the decision
/// that answers it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PermissionRequestId(pub String);

impl PermissionRequestId {
    pub fn generate() -> Self {
        PermissionRequestId(format!("perm-{:x}-{:04x}", now_ms(), next_global()))
    }
}

impl std::fmt::Display for PermissionRequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// How a pending `ask` permission was resolved.
///
/// Deliberately per-request only: this version has no "always allow"
/// decision, because a persistent permission change belongs in
/// `.orbit/project.yaml`, not in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    AllowOnce,
    DenyOnce,
    /// The turn was cancelled while this request was pending.
    Cancelled,
}

impl PermissionDecision {
    pub fn is_allowed(self) -> bool {
        matches!(self, PermissionDecision::AllowOnce)
    }
}

/// Whether a session is scoped to one project or to a workspace of
/// several.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMode {
    SingleProject,
    Workspace,
}

/// What happened. Serialized with an internal `type` tag and flattened
/// into [`AgentEvent`], so one frame carries both correlation fields and
/// payload fields at the top level.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventPayload {
    SessionStarted {
        protocol_version: u32,
        mode: SessionMode,
        /// Present in workspace mode.
        #[serde(skip_serializing_if = "Option::is_none")]
        workspace: Option<String>,
        projects: Vec<String>,
    },
    SessionEnded {
        reason: String,
    },
    UserMessageReceived {
        text: String,
    },
    ActiveProjectsChanged {
        projects: Vec<String>,
    },
    /// Deterministic pre-model retrieval began for `scope` (empty means
    /// workspace-level, no specific project).
    RetrievalStarted {
        scope: Vec<String>,
    },
    RetrievalCompleted {
        scope: Vec<String>,
        action_count: usize,
        source_count: usize,
    },
    /// The model asked for an action, before any permission check.
    ActionRequested {
        action: String,
        arguments: String,
    },
    /// Execution is paused until a matching `permission.resolve` arrives.
    PermissionRequired {
        request_id: PermissionRequestId,
        action: String,
        description: String,
        arguments: String,
    },
    PermissionResolved {
        request_id: PermissionRequestId,
        decision: PermissionDecision,
    },
    /// The permission check passed and the action is now running.
    ActionStarted {
        action: String,
    },
    /// Incremental progress from a long-running action.
    ///
    /// Part of the wire contract for future consumers; no action emits it
    /// today, because no current action produces intermediate progress.
    /// It is never synthesized to make the stream look busier.
    ActionProgress {
        action: String,
        message: String,
    },
    ActionCompleted {
        action: String,
        duration_ms: u64,
        source_count: usize,
    },
    ActionFailed {
        action: String,
        error: String,
    },
    /// A source an executed action actually returned. Never derived from
    /// model output.
    SourceFound {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        line_start: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        line_end: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        section: Option<String>,
    },
    ModelResponseStarted {
        model: String,
        streaming: bool,
    },
    /// Text as it arrived from the provider. Concatenating every delta of
    /// one model response yields exactly the
    /// [`EventPayload::ModelResponseCompleted`] text.
    ResponseDelta {
        text: String,
    },
    ModelResponseCompleted {
        text: String,
    },
    ExecutionCancelled {
        reason: String,
    },
    Warning {
        message: String,
    },
    /// The turn failed and produced no answer.
    Failure {
        message: String,
    },
    TurnCompleted {
        source_count: usize,
        action_count: usize,
    },
}

impl EventPayload {
    /// The `type` tag as it appears on the wire. Useful for asserting
    /// event ordering without matching every payload field.
    pub fn type_name(&self) -> &'static str {
        match self {
            EventPayload::SessionStarted { .. } => "session_started",
            EventPayload::SessionEnded { .. } => "session_ended",
            EventPayload::UserMessageReceived { .. } => "user_message_received",
            EventPayload::ActiveProjectsChanged { .. } => "active_projects_changed",
            EventPayload::RetrievalStarted { .. } => "retrieval_started",
            EventPayload::RetrievalCompleted { .. } => "retrieval_completed",
            EventPayload::ActionRequested { .. } => "action_requested",
            EventPayload::PermissionRequired { .. } => "permission_required",
            EventPayload::PermissionResolved { .. } => "permission_resolved",
            EventPayload::ActionStarted { .. } => "action_started",
            EventPayload::ActionProgress { .. } => "action_progress",
            EventPayload::ActionCompleted { .. } => "action_completed",
            EventPayload::ActionFailed { .. } => "action_failed",
            EventPayload::SourceFound { .. } => "source_found",
            EventPayload::ModelResponseStarted { .. } => "model_response_started",
            EventPayload::ResponseDelta { .. } => "response_delta",
            EventPayload::ModelResponseCompleted { .. } => "model_response_completed",
            EventPayload::ExecutionCancelled { .. } => "execution_cancelled",
            EventPayload::Warning { .. } => "warning",
            EventPayload::Failure { .. } => "failure",
            EventPayload::TurnCompleted { .. } => "turn_completed",
        }
    }
}

/// One event, with everything needed to correlate it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEvent {
    pub session_id: SessionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<ExecutionId>,
    pub timestamp_ms: u64,
    /// The registered project this event concerns, when it concerns one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(flatten)]
    pub payload: EventPayload,
}

impl AgentEvent {
    pub fn type_name(&self) -> &'static str {
        self.payload.type_name()
    }
}

/// Receives events as they happen. Implementations must be cheap and must
/// not block: the CLI renderer prints a line, the JSONL bridge writes a
/// frame, a test sink pushes onto a vector.
pub trait EventSink: Send + Sync {
    fn emit(&self, event: AgentEvent);

    /// `false` lets emitters skip building payloads nobody will read.
    fn is_enabled(&self) -> bool {
        true
    }
}

/// Discards every event. The default for callers that do not observe a
/// session (`orbit ask`, one-shot commands, most tests).
pub struct NullSink;

impl EventSink for NullSink {
    fn emit(&self, _event: AgentEvent) {}
    fn is_enabled(&self) -> bool {
        false
    }
}

/// Records events in order. Used by tests to assert ordering guarantees,
/// and by the CLI to replay a turn's sources after the fact.
#[derive(Default)]
pub struct CollectingSink {
    events: std::sync::Mutex<Vec<AgentEvent>>,
}

impl CollectingSink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn events(&self) -> Vec<AgentEvent> {
        self.events
            .lock()
            .expect("event sink mutex poisoned")
            .clone()
    }

    /// The `type` tags in emission order — the readable form for ordering
    /// assertions.
    pub fn type_names(&self) -> Vec<&'static str> {
        self.events()
            .iter()
            .map(|e| e.payload.type_name())
            .collect()
    }

    pub fn clear(&self) {
        self.events
            .lock()
            .expect("event sink mutex poisoned")
            .clear();
    }
}

impl EventSink for CollectingSink {
    fn emit(&self, event: AgentEvent) {
        self.events
            .lock()
            .expect("event sink mutex poisoned")
            .push(event);
    }
}

/// Stamps events with session/turn/execution identity and hands them to a
/// sink.
///
/// Passed by value (it is a cheap `Arc` bundle) rather than as an
/// `Option`, so emitting code never branches on whether anyone is
/// listening — [`EventEmitter::null`] is a working emitter that drops
/// everything.
#[derive(Clone)]
pub struct EventEmitter {
    sink: Arc<dyn EventSink>,
    session_id: SessionId,
    turn_id: Option<TurnId>,
    project: Option<String>,
    next_execution: Arc<AtomicU64>,
}

impl EventEmitter {
    pub fn new(sink: Arc<dyn EventSink>, session_id: SessionId) -> Self {
        Self {
            sink,
            session_id,
            turn_id: None,
            project: None,
            next_execution: Arc::new(AtomicU64::new(1)),
        }
    }

    /// An emitter that discards everything, for callers with no observer.
    pub fn null() -> Self {
        Self::new(Arc::new(NullSink), SessionId("sess-none".to_string()))
    }

    pub fn is_enabled(&self) -> bool {
        self.sink.is_enabled()
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn turn_id(&self) -> Option<&TurnId> {
        self.turn_id.as_ref()
    }

    pub fn with_turn(&self, turn_id: TurnId) -> Self {
        Self {
            turn_id: Some(turn_id),
            ..self.clone()
        }
    }

    pub fn with_project(&self, project: Option<String>) -> Self {
        Self {
            project,
            ..self.clone()
        }
    }

    /// Execution IDs are allocated from a per-session counter shared by
    /// every clone of this emitter, so they stay unique across a turn's
    /// actions and model calls.
    pub fn next_execution_id(&self) -> ExecutionId {
        ExecutionId::new(self.next_execution.fetch_add(1, Ordering::Relaxed))
    }

    pub fn emit(&self, payload: EventPayload) {
        self.dispatch(None, self.project.clone(), payload);
    }

    pub fn emit_execution(&self, execution_id: &ExecutionId, payload: EventPayload) {
        self.dispatch(Some(execution_id.clone()), self.project.clone(), payload);
    }

    pub fn emit_execution_project(
        &self,
        execution_id: &ExecutionId,
        project: Option<&str>,
        payload: EventPayload,
    ) {
        self.dispatch(
            Some(execution_id.clone()),
            project.map(str::to_string).or_else(|| self.project.clone()),
            payload,
        );
    }

    /// Emit [`EventPayload::SourceFound`] for a source an action returned.
    ///
    /// Project identity is resolved in this order: the `<project>:<path>`
    /// prefix workspace actions encode into the path (which is
    /// authoritative — in workspace mode a single action returns sources
    /// from several different projects), then `fallback` (the project the
    /// action ran against, which is what a single-project source belongs
    /// to), then the emitter's own project.
    pub fn emit_source(
        &self,
        execution_id: &ExecutionId,
        fallback_project: Option<&str>,
        source: &SourceReference,
    ) {
        let (encoded_project, path) = source.split_project_prefix();
        let project = encoded_project
            .or_else(|| fallback_project.map(str::to_string))
            .or_else(|| self.project.clone());
        self.dispatch(
            Some(execution_id.clone()),
            project,
            EventPayload::SourceFound {
                path: path.display().to_string(),
                line_start: source.line_start,
                line_end: source.line_end,
                section: source.section.clone(),
            },
        );
    }

    fn dispatch(
        &self,
        execution_id: Option<ExecutionId>,
        project: Option<String>,
        payload: EventPayload,
    ) {
        if !self.sink.is_enabled() {
            return;
        }
        self.sink.emit(AgentEvent {
            session_id: self.session_id.clone(),
            turn_id: self.turn_id.clone(),
            execution_id,
            timestamp_ms: now_ms(),
            project,
            payload,
        });
    }
}

/// Cooperative cancellation for one turn.
///
/// Checked between agent iterations, between tool calls, and between
/// streaming chunks. Cancellation stops *future* work; it never claims
/// that an already-completed filesystem write or command execution was
/// undone.
#[derive(Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

const MAX_ARGUMENT_VALUE_CHARS: usize = 80;
const MAX_ARGUMENT_SUMMARY_CHARS: usize = 240;

/// Keys whose values are never shown, matched case-insensitively as
/// substrings so `ANTHROPIC_API_KEY`, `authToken`, and `db_password` are
/// all caught.
const SENSITIVE_KEY_FRAGMENTS: &[&str] = &[
    "secret",
    "token",
    "password",
    "passwd",
    "credential",
    "api_key",
    "apikey",
    "auth",
    "private",
];

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    SENSITIVE_KEY_FRAGMENTS
        .iter()
        .any(|fragment| lower.contains(fragment))
}

fn looks_absolute(value: &str) -> bool {
    if value.starts_with('/') {
        return true;
    }
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Absolute paths in an event would leak a user's directory layout into
/// logs and UI transcripts, so only the last two components are kept.
///
/// Splits on both separators rather than using [`std::path::Path`]: a
/// Windows-shaped path must be shortened even when Orbit is running on a
/// Unix host (where `\` is an ordinary filename character), otherwise the
/// redaction silently does nothing on exactly the input it was written
/// for.
fn shorten_absolute(value: &str) -> String {
    let tail: Vec<&str> = value
        .split(['/', '\\'])
        .filter(|part| !part.is_empty())
        .collect();
    let kept: Vec<&str> = tail.iter().rev().take(2).rev().copied().collect();
    if kept.is_empty() {
        "…".to_string()
    } else {
        format!("…/{}", kept.join("/"))
    }
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        return value.to_string();
    }
    let kept: String = value.chars().take(max).collect();
    format!("{kept}…")
}

/// Render action arguments as a short, safe, human-readable summary for
/// [`EventPayload::ActionRequested`] and
/// [`EventPayload::PermissionRequired`].
///
/// Never emits raw argument JSON: secret-shaped values are redacted,
/// absolute paths are shortened, long strings and containers are
/// collapsed. The result is for display and audit only — it is never
/// parsed back into arguments, and the action itself always receives the
/// original, unmodified input.
pub fn summarize_arguments(arguments: &Value) -> String {
    let summary = match arguments {
        Value::Object(map) if map.is_empty() => "(no arguments)".to_string(),
        Value::Object(map) => map
            .iter()
            .map(|(key, value)| {
                if is_sensitive_key(key) {
                    return format!("{key}=<redacted>");
                }
                let rendered = match value {
                    Value::String(s) if looks_absolute(s) => shorten_absolute(s),
                    Value::String(s) => truncate_chars(s, MAX_ARGUMENT_VALUE_CHARS),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    Value::Null => "null".to_string(),
                    Value::Array(items) => format!("[{} item(s)]", items.len()),
                    Value::Object(_) => "{…}".to_string(),
                };
                format!("{key}={rendered}")
            })
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => "(no arguments)".to_string(),
        other => truncate_chars(&other.to_string(), MAX_ARGUMENT_VALUE_CHARS),
    };
    truncate_chars(&summary, MAX_ARGUMENT_SUMMARY_CHARS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;

    #[test]
    fn session_ids_are_unique() {
        let a = SessionId::generate();
        let b = SessionId::generate();
        assert_ne!(a, b);
    }

    #[test]
    fn event_round_trips_through_json_with_a_flattened_payload() {
        let event = AgentEvent {
            session_id: SessionId("sess-1".to_string()),
            turn_id: Some(TurnId::new(2)),
            execution_id: Some(ExecutionId::new(3)),
            timestamp_ms: 1234,
            project: Some("docs".to_string()),
            payload: EventPayload::SourceFound {
                path: "obc/architecture.md".to_string(),
                line_start: Some(18),
                line_end: Some(41),
                section: None,
            },
        };

        let json = serde_json::to_value(&event).unwrap();
        // The tag and the payload fields must sit at the top level
        // alongside correlation fields -- that is the documented frame
        // shape the JSONL bridge and a future SwiftUI client parse.
        assert_eq!(json["type"], "source_found");
        assert_eq!(json["session_id"], "sess-1");
        assert_eq!(json["turn_id"], "turn-2");
        assert_eq!(json["execution_id"], "exec-3");
        assert_eq!(json["project"], "docs");
        assert_eq!(json["path"], "obc/architecture.md");
        assert_eq!(json["line_start"], 18);
        assert!(json.get("section").is_none(), "None fields are omitted");

        let parsed: AgentEvent = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, event);
    }

    #[test]
    fn every_payload_variant_round_trips() {
        let payloads = vec![
            EventPayload::SessionStarted {
                protocol_version: EVENT_PROTOCOL_VERSION,
                mode: SessionMode::Workspace,
                workspace: Some("Orbit Lab".to_string()),
                projects: vec!["obc".to_string()],
            },
            EventPayload::SessionEnded {
                reason: "client requested".to_string(),
            },
            EventPayload::UserMessageReceived {
                text: "why STM32?".to_string(),
            },
            EventPayload::ActiveProjectsChanged {
                projects: vec!["docs".to_string(), "obc".to_string()],
            },
            EventPayload::RetrievalStarted { scope: vec![] },
            EventPayload::RetrievalCompleted {
                scope: vec!["obc".to_string()],
                action_count: 2,
                source_count: 3,
            },
            EventPayload::ActionRequested {
                action: "workspace.search".to_string(),
                arguments: "query=STM32".to_string(),
            },
            EventPayload::PermissionRequired {
                request_id: PermissionRequestId("perm-1".to_string()),
                action: "command.run_configured".to_string(),
                description: "run a configured command".to_string(),
                arguments: "name=test".to_string(),
            },
            EventPayload::PermissionResolved {
                request_id: PermissionRequestId("perm-1".to_string()),
                decision: PermissionDecision::AllowOnce,
            },
            EventPayload::ActionStarted {
                action: "project.search".to_string(),
            },
            EventPayload::ActionProgress {
                action: "project.search".to_string(),
                message: "half done".to_string(),
            },
            EventPayload::ActionCompleted {
                action: "project.search".to_string(),
                duration_ms: 12,
                source_count: 1,
            },
            EventPayload::ActionFailed {
                action: "project.read_file".to_string(),
                error: "excluded".to_string(),
            },
            EventPayload::SourceFound {
                path: "README.md".to_string(),
                line_start: None,
                line_end: None,
                section: Some("Overview".to_string()),
            },
            EventPayload::ModelResponseStarted {
                model: "qwen3:latest".to_string(),
                streaming: true,
            },
            EventPayload::ResponseDelta {
                text: "The ".to_string(),
            },
            EventPayload::ModelResponseCompleted {
                text: "The answer.".to_string(),
            },
            EventPayload::ExecutionCancelled {
                reason: "user requested".to_string(),
            },
            EventPayload::Warning {
                message: "mcp server unreachable".to_string(),
            },
            EventPayload::Failure {
                message: "provider unreachable".to_string(),
            },
            EventPayload::TurnCompleted {
                source_count: 3,
                action_count: 2,
            },
        ];

        for payload in payloads {
            let event = AgentEvent {
                session_id: SessionId("sess-1".to_string()),
                turn_id: None,
                execution_id: None,
                timestamp_ms: 1,
                project: None,
                payload: payload.clone(),
            };
            let text = serde_json::to_string(&event).unwrap();
            let parsed: AgentEvent = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{} failed to round-trip: {e}", payload.type_name()));
            assert_eq!(parsed.payload, payload);
            // The tag in the JSON must match the reported name, or
            // ordering assertions elsewhere would be silently wrong.
            let value: Value = serde_json::from_str(&text).unwrap();
            assert_eq!(value["type"], payload.type_name());
        }
    }

    #[test]
    fn null_emitter_is_disabled_and_drops_events() {
        let emitter = EventEmitter::null();
        assert!(!emitter.is_enabled());
        emitter.emit(EventPayload::Warning {
            message: "ignored".to_string(),
        });
    }

    #[test]
    fn emitter_stamps_session_turn_and_execution_identity() {
        let sink = Arc::new(CollectingSink::new());
        let emitter = EventEmitter::new(sink.clone(), SessionId("sess-x".to_string()))
            .with_turn(TurnId::new(1));
        let execution = emitter.next_execution_id();
        emitter.emit_execution(
            &execution,
            EventPayload::ActionStarted {
                action: "project.search".to_string(),
            },
        );

        let events = sink.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].session_id, SessionId("sess-x".to_string()));
        assert_eq!(events[0].turn_id, Some(TurnId::new(1)));
        assert_eq!(events[0].execution_id, Some(ExecutionId::new(1)));
    }

    #[test]
    fn execution_ids_are_unique_across_emitter_clones() {
        let sink = Arc::new(CollectingSink::new());
        let emitter = EventEmitter::new(sink, SessionId("sess-x".to_string()));
        let clone = emitter.with_turn(TurnId::new(1));
        let ids = vec![
            emitter.next_execution_id(),
            clone.next_execution_id(),
            emitter.next_execution_id(),
        ];
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            3,
            "counter must be shared, not cloned: {ids:?}"
        );
    }

    #[test]
    fn emit_source_splits_the_workspace_project_prefix() {
        let sink = Arc::new(CollectingSink::new());
        let emitter = EventEmitter::new(sink.clone(), SessionId("sess-x".to_string()));
        let execution = emitter.next_execution_id();
        emitter.emit_source(
            &execution,
            Some("workspace-fallback"),
            &SourceReference::lines(PathBuf::from("docs:obc/architecture.md"), 3, 4),
        );

        let events = sink.events();
        assert_eq!(events[0].project.as_deref(), Some("docs"));
        match &events[0].payload {
            EventPayload::SourceFound { path, .. } => assert_eq!(path, "obc/architecture.md"),
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    /// A plain single-project source has no encoded prefix, so it must
    /// inherit the project the action actually ran against.
    #[test]
    fn emit_source_falls_back_to_the_executing_project() {
        let sink = Arc::new(CollectingSink::new());
        let emitter = EventEmitter::new(sink.clone(), SessionId("sess-x".to_string()));
        let execution = emitter.next_execution_id();
        emitter.emit_source(
            &execution,
            Some("obc"),
            &SourceReference::lines(PathBuf::from("src/watchdog.rs"), 1, 2),
        );

        let events = sink.events();
        assert_eq!(events[0].project.as_deref(), Some("obc"));
        match &events[0].payload {
            EventPayload::SourceFound { path, .. } => assert_eq!(path, "src/watchdog.rs"),
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn cancellation_token_is_shared_across_clones() {
        let token = CancellationToken::new();
        let clone = token.clone();
        assert!(!token.is_cancelled());
        clone.cancel();
        assert!(token.is_cancelled());
    }

    #[test]
    fn summarize_arguments_redacts_secret_shaped_keys() {
        let summary = summarize_arguments(&json!({
            "api_key": "sk-should-never-appear",
            "authToken": "also-secret",
            "db_password": "hunter2",
        }));
        assert!(!summary.contains("sk-should-never-appear"), "{summary}");
        assert!(!summary.contains("also-secret"), "{summary}");
        assert!(!summary.contains("hunter2"), "{summary}");
        assert_eq!(summary.matches("<redacted>").count(), 3, "{summary}");
    }

    #[test]
    fn summarize_arguments_shortens_absolute_paths() {
        let summary =
            summarize_arguments(&json!({"path": "/Users/someone/secret-lab/obc/src/main.rs"}));
        assert!(!summary.contains("/Users/someone"), "{summary}");
        assert_eq!(summary, "path=…/src/main.rs");

        // Must hold on a Unix host too, where `\` is not a path separator.
        let windows = summarize_arguments(&json!({"path": "C:\\Users\\someone\\obc\\main.rs"}));
        assert_eq!(windows, "path=…/obc/main.rs");
    }

    #[test]
    fn summarize_arguments_keeps_relative_paths_readable() {
        assert_eq!(
            summarize_arguments(&json!({"path": "obc/src/watchdog.rs"})),
            "path=obc/src/watchdog.rs"
        );
    }

    #[test]
    fn summarize_arguments_collapses_containers_and_empty_input() {
        assert_eq!(summarize_arguments(&json!({})), "(no arguments)");
        assert_eq!(
            summarize_arguments(&json!({"projects": ["docs", "obc"]})),
            "projects=[2 item(s)]"
        );
        assert_eq!(
            summarize_arguments(&json!({"nested": {"a": 1}})),
            "nested={…}"
        );
    }

    #[test]
    fn summarize_arguments_is_bounded() {
        let long = "x".repeat(5_000);
        let summary = summarize_arguments(&json!({"query": long}));
        assert!(
            summary.chars().count() <= MAX_ARGUMENT_SUMMARY_CHARS + 1,
            "summary was {} chars",
            summary.chars().count()
        );
    }
}
