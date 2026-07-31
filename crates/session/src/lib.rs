//! Stateful Orbit sessions.
//!
//! ```text
//! User input
//!     ↓
//! Session Runtime            (this crate)
//!     ↓
//! Agent + Workspace + Actions + Provider
//!     ↓
//! Agent Event Bus            (orbit_core::EventSink)
//!     ├── CLI renderer
//!     ├── JSONL application bridge
//!     └── future SwiftUI client
//! ```
//!
//! Every front end observes the same [`orbit_core::AgentEvent`] stream, so
//! none of them contains agent logic and none of them has to reconstruct
//! what happened from printed text.
//!
//! Session state is in-process only and is never written to disk.

pub mod command;
pub mod permission;
pub mod session;
pub mod topic;

pub use command::{COMMAND_HELP, ParsedInput, SessionCommand, parse};
pub use permission::{ConfirmationMode, SessionConfirmation};
pub use session::{
    CommandRun, ExecutionState, SessionRuntime, SessionState, SessionStatus, TurnOutcome,
};
pub use topic::TopicState;
