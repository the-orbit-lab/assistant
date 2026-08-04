//! Shared, provider-independent and protocol-independent types used across
//! every Orbit crate: project identity, messages, model requests/responses,
//! actions, permissions, sources, execution records, and errors.

pub mod action;
pub mod error;
pub mod event;
pub mod execution;
pub mod message;
pub mod model;
pub mod permission;
pub mod project;
pub mod retrieval;
pub mod source;

pub use action::{ActionDescriptor, ActionInput, ActionOutput};
pub use error::{OrbitError, ProviderError, Result};
pub use event::{
    AgentEvent, CancellationToken, CollectingSink, EVENT_PROTOCOL_VERSION, EventEmitter,
    EventPayload, EventSink, ExecutionId, NullSink, PermissionDecision, PermissionRequestId,
    SessionId, SessionMode, TurnId, summarize_arguments,
};
pub use execution::ExecutionRecord;
pub use message::{Message, Role, ToolCall};
pub use model::{FinishReason, ModelRequest, ModelResponse, ToolDefinition};
pub use permission::{
    AlwaysAllow, AlwaysDeny, ConfirmationProvider, ConfirmationRequest, Permission,
    PermissionOutcome,
};
pub use project::{ProjectId, ProjectPaths};
pub use retrieval::{CONFIDENT_SOURCE_FILES, RetrievalConfidence};
pub use source::SourceReference;
