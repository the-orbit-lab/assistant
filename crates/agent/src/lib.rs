//! Orbit's own agent loop.
//!
//! `Agent` calls the Action Runtime ([`orbit_actions::ActionRegistry`])
//! directly — never through Orbit's own MCP server, which exists purely as
//! an adapter for *external* hosts. [`session::build_registry`] is the one
//! place native actions and externally-consumed MCP tools are merged into
//! a single registry for a session.

pub mod agent;
pub mod prompt;
pub mod retrieval;
pub mod session;

pub use agent::dedupe_sources;
pub use agent::{Agent, AgentOutcome, DEFAULT_MAX_ITERATIONS, DEFAULT_REQUEST_TIMEOUT};
pub use retrieval::is_broad_overview_question;
pub use session::build_registry;

/// Re-exported so callers that receive a manager from [`build_registry`]
/// can hold and shut it down without depending on `orbit-mcp-client`
/// directly -- to them it is an opaque session handle, not an MCP API.
pub use orbit_mcp_client::{McpClientManager, McpConnectionWarning};
