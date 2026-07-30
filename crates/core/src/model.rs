use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::message::Message;

/// A tool the model is allowed to call, described in provider-independent
/// terms (JSON Schema input).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// A request to a model provider. Providers translate this into their own
/// wire format; nothing provider-specific should ever appear here.
#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDefinition>,
    pub timeout: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
}

/// A response from a model provider.
#[derive(Debug, Clone)]
pub struct ModelResponse {
    pub message: Message,
    pub finish_reason: FinishReason,
}
