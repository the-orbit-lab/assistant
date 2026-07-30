use orbit_core::{
    FinishReason, Message, ModelRequest, ModelResponse, OrbitError, ProviderError, Role, ToolCall,
    ToolDefinition,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::ModelProvider;

/// The default local Ollama endpoint. No API key is required or supported.
pub const DEFAULT_ENDPOINT: &str = "http://localhost:11434";

pub struct OllamaProvider {
    client: reqwest::Client,
    endpoint: String,
    model: String,
}

impl OllamaProvider {
    pub fn new(endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            endpoint: endpoint.into(),
            model: model.into(),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// GET `/api/tags`. Used by `orbit doctor` and CLI model-availability
    /// checks; also the connectivity check itself.
    pub async fn list_models(&self) -> Result<Vec<String>, ProviderError> {
        let url = format!("{}/api/tags", self.endpoint.trim_end_matches('/'));
        let response = self
            .client
            .get(&url)
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .map_err(|e| connection_error(&self.endpoint, &e))?;

        if !response.status().is_success() {
            return Err(ProviderError::Other {
                reason: format!("GET /api/tags returned {}", response.status()),
            });
        }

        let body: TagsResponse =
            response
                .json()
                .await
                .map_err(|e| ProviderError::InvalidResponse {
                    reason: e.to_string(),
                })?;
        Ok(body.models.into_iter().map(|m| m.name).collect())
    }

    pub async fn check_connectivity(&self) -> Result<(), ProviderError> {
        self.list_models().await.map(|_| ())
    }

    pub async fn model_is_available(&self) -> Result<bool, ProviderError> {
        let models = self.list_models().await?;
        Ok(models
            .iter()
            .any(|m| m == &self.model || tag_matches(m, &self.model)))
    }

    /// The exact command to run when the configured model is missing,
    /// shown by `orbit doctor` instead of Orbit ever downloading it itself.
    pub fn pull_command(&self) -> String {
        format!("ollama pull {}", self.model)
    }
}

/// `qwen2.5` should match an installed `qwen2.5:latest`, and vice versa.
fn tag_matches(installed: &str, requested: &str) -> bool {
    let strip = |s: &str| s.split(':').next().unwrap_or(s).to_string();
    strip(installed) == strip(requested)
}

fn connection_error(endpoint: &str, err: &reqwest::Error) -> ProviderError {
    if err.is_timeout() {
        ProviderError::Timeout { timeout_secs: 5 }
    } else {
        ProviderError::ConnectionFailed {
            endpoint: endpoint.to_string(),
            reason: err.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl ModelProvider for OllamaProvider {
    fn describe(&self) -> String {
        format!("ollama ({} @ {})", self.model, self.endpoint)
    }

    async fn chat(&self, request: &ModelRequest) -> Result<ModelResponse, OrbitError> {
        let wire = ChatRequest {
            model: request.model.clone(),
            messages: request.messages.iter().map(to_wire_message).collect(),
            tools: request.tools.iter().map(to_wire_tool).collect(),
            stream: false,
        };

        let url = format!("{}/api/chat", self.endpoint.trim_end_matches('/'));
        let response = self
            .client
            .post(&url)
            .timeout(request.timeout)
            .json(&wire)
            .send()
            .await
            .map_err(|e| connection_error(&self.endpoint, &e))?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            let body: ErrorResponse = response.json().await.unwrap_or_default();
            return Err(OrbitError::Provider(ProviderError::ModelUnavailable {
                model: request.model.clone(),
                hint: format!(
                    "{}. Run `{}` to download it.",
                    body.error.unwrap_or_else(|| "model not found".to_string()),
                    self.pull_command()
                ),
            }));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(OrbitError::Provider(ProviderError::RateLimited {
                reason: format!("HTTP {status}"),
            }));
        }
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(OrbitError::Provider(ProviderError::Unauthorized {
                reason: format!("HTTP {status}"),
            }));
        }
        if !status.is_success() {
            let body: ErrorResponse = response.json().await.unwrap_or_default();
            return Err(OrbitError::Provider(ProviderError::Other {
                reason: body
                    .error
                    .unwrap_or_else(|| format!("Ollama returned HTTP {status}")),
            }));
        }

        let body: ChatResponse = response.json().await.map_err(|e| {
            OrbitError::Provider(ProviderError::InvalidResponse {
                reason: e.to_string(),
            })
        })?;

        from_wire_response(body)
    }
}

fn to_wire_message(message: &Message) -> WireMessage {
    let tool_calls = if message.tool_calls.is_empty() {
        None
    } else {
        Some(
            message
                .tool_calls
                .iter()
                .map(|tc| WireToolCall {
                    function: WireToolCallFunction {
                        name: tc.name.clone(),
                        arguments: tc.arguments.clone(),
                    },
                })
                .collect(),
        )
    };
    WireMessage {
        role: role_to_wire(message.role).to_string(),
        content: message.content.clone(),
        tool_calls,
    }
}

fn role_to_wire(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn to_wire_tool(tool: &ToolDefinition) -> WireTool {
    WireTool {
        r#type: "function".to_string(),
        function: WireToolFunction {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: tool.input_schema.clone(),
        },
    }
}

fn from_wire_response(response: ChatResponse) -> Result<ModelResponse, OrbitError> {
    let content = response.message.content.unwrap_or_default();
    let message = match response.message.tool_calls {
        Some(calls) if !calls.is_empty() => {
            let tool_calls = calls
                .into_iter()
                .enumerate()
                .map(|(idx, call)| ToolCall {
                    id: format!("call_{idx}"),
                    name: call.function.name,
                    arguments: call.function.arguments,
                })
                .collect();
            let mut msg = Message::assistant_tool_calls(tool_calls);
            msg.content = content;
            (msg, FinishReason::ToolCalls)
        }
        _ => (
            Message::assistant(content),
            finish_reason(response.done_reason.as_deref()),
        ),
    };

    Ok(ModelResponse {
        message: message.0,
        finish_reason: message.1,
    })
}

fn finish_reason(done_reason: Option<&str>) -> FinishReason {
    match done_reason {
        Some("length") => FinishReason::Length,
        _ => FinishReason::Stop,
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<WireMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<WireTool>,
    stream: bool,
}

#[derive(Debug, Serialize)]
struct WireMessage {
    role: String,
    content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<WireToolCall>>,
}

#[derive(Debug, Serialize)]
struct WireToolCall {
    function: WireToolCallFunction,
}

#[derive(Debug, Serialize)]
struct WireToolCallFunction {
    name: String,
    arguments: Value,
}

#[derive(Debug, Serialize)]
struct WireTool {
    r#type: String,
    function: WireToolFunction,
}

#[derive(Debug, Serialize)]
struct WireToolFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Debug, Deserialize)]
struct ChatResponse {
    message: WireResponseMessage,
    #[serde(default)]
    done_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WireResponseMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<WireResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
struct WireResponseToolCall {
    function: WireResponseToolCallFunction,
}

#[derive(Debug, Deserialize)]
struct WireResponseToolCallFunction {
    name: String,
    #[serde(default)]
    arguments: Value,
}

#[derive(Debug, Deserialize, Default)]
struct ErrorResponse {
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TagsResponse {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Debug, Deserialize)]
struct TagModel {
    name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_matching_ignores_default_suffix() {
        assert!(tag_matches("qwen2.5:latest", "qwen2.5"));
        assert!(tag_matches("qwen2.5:latest", "qwen2.5:latest"));
        assert!(!tag_matches("qwen2.5:latest", "llama3.1:8b"));
    }

    #[test]
    fn pull_command_names_the_configured_model() {
        let provider = OllamaProvider::new(DEFAULT_ENDPOINT, "qwen2.5:latest");
        assert_eq!(provider.pull_command(), "ollama pull qwen2.5:latest");
    }

    #[test]
    fn wire_round_trip_maps_tool_call_response() {
        let response = ChatResponse {
            message: WireResponseMessage {
                content: Some(String::new()),
                tool_calls: Some(vec![WireResponseToolCall {
                    function: WireResponseToolCallFunction {
                        name: "project.search".to_string(),
                        arguments: serde_json::json!({"query": "watchdog"}),
                    },
                }]),
            },
            done_reason: None,
        };
        let parsed = from_wire_response(response).unwrap();
        assert_eq!(parsed.finish_reason, FinishReason::ToolCalls);
        assert_eq!(parsed.message.tool_calls[0].name, "project.search");
    }

    /// Opt-in: exercises a real Ollama server. Run with
    /// `cargo test -p orbit-providers -- --ignored live_ollama`.
    #[tokio::test]
    #[ignore]
    async fn live_ollama_reports_connectivity_and_chats() {
        let provider = OllamaProvider::new(DEFAULT_ENDPOINT, "qwen2.5:0.5b");
        provider
            .check_connectivity()
            .await
            .expect("Ollama must be running locally for this test");
        let request = ModelRequest {
            model: provider.model().to_string(),
            messages: vec![Message::user("Reply with exactly: OK")],
            tools: vec![],
            timeout: std::time::Duration::from_secs(30),
        };
        let response = provider.chat(&request).await.unwrap();
        assert!(!response.message.content.is_empty());
    }

    #[test]
    fn wire_round_trip_maps_plain_text_response() {
        let response = ChatResponse {
            message: WireResponseMessage {
                content: Some("hello".to_string()),
                tool_calls: None,
            },
            done_reason: Some("stop".to_string()),
        };
        let parsed = from_wire_response(response).unwrap();
        assert_eq!(parsed.finish_reason, FinishReason::Stop);
        assert_eq!(parsed.message.content, "hello");
    }
}
