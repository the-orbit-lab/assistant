use orbit_core::{
    FinishReason, Message, ModelRequest, ModelResponse, OrbitError, ProviderError, Role, ToolCall,
    ToolDefinition,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::provider::{DeltaHandler, ModelProvider};

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

impl OllamaProvider {
    /// POST `/api/chat`, mapping transport and HTTP failures onto
    /// [`ProviderError`]. Shared by the buffered and streaming paths so
    /// both report an unreachable server, a missing model, or a rate limit
    /// identically.
    async fn post_chat(
        &self,
        request: &ModelRequest,
        stream: bool,
    ) -> Result<reqwest::Response, OrbitError> {
        let wire = ChatRequest {
            model: request.model.clone(),
            messages: request.messages.iter().map(to_wire_message).collect(),
            tools: request.tools.iter().map(to_wire_tool).collect(),
            stream,
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

        Ok(response)
    }
}

#[async_trait::async_trait]
impl ModelProvider for OllamaProvider {
    fn describe(&self) -> String {
        format!("ollama ({} @ {})", self.model, self.endpoint)
    }

    fn supports_streaming(&self) -> bool {
        true
    }

    async fn chat(&self, request: &ModelRequest) -> Result<ModelResponse, OrbitError> {
        let response = self.post_chat(request, false).await?;
        let body: ChatResponse = response.json().await.map_err(|e| {
            OrbitError::Provider(ProviderError::InvalidResponse {
                reason: e.to_string(),
            })
        })?;
        from_wire_response(body)
    }

    /// Ollama streams `/api/chat` as newline-delimited JSON: one object per
    /// chunk, each carrying an incremental `message.content`, with a final
    /// object marked `done`.
    ///
    /// Content is accumulated exactly as it is handed to `handler`, so the
    /// returned message's content is always the concatenation of the
    /// emitted deltas. Tool calls are collected from whichever chunk
    /// carries them, so tool calling behaves the same as the buffered path.
    async fn chat_streaming(
        &self,
        request: &ModelRequest,
        handler: &dyn DeltaHandler,
    ) -> Result<ModelResponse, OrbitError> {
        let mut response = self.post_chat(request, true).await?;

        // Bytes, not a String: a multi-byte UTF-8 character can straddle
        // two HTTP chunks, and converting each chunk independently would
        // corrupt it. Only complete lines are decoded.
        let mut buffer: Vec<u8> = Vec::new();
        let mut content = String::new();
        let mut tool_calls: Vec<WireResponseToolCall> = Vec::new();
        let mut done_reason: Option<String> = None;
        let mut stopped_early = false;

        'read: while let Some(chunk) = response.chunk().await.map_err(|e| {
            OrbitError::Provider(ProviderError::InvalidResponse {
                reason: format!("failed while reading the response stream: {e}"),
            })
        })? {
            buffer.extend_from_slice(&chunk);

            while let Some(newline) = buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<u8> = buffer.drain(..=newline).collect();
                if !consume_stream_line(
                    &line,
                    handler,
                    &mut content,
                    &mut tool_calls,
                    &mut done_reason,
                )? {
                    stopped_early = true;
                    break 'read;
                }
            }
        }

        // A final chunk may arrive without a trailing newline.
        if !stopped_early && !buffer.is_empty() {
            let line = std::mem::take(&mut buffer);
            consume_stream_line(
                &line,
                handler,
                &mut content,
                &mut tool_calls,
                &mut done_reason,
            )?;
        }

        from_wire_response(ChatResponse {
            message: WireResponseMessage {
                content: Some(content),
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
            },
            done_reason,
        })
    }
}

/// Parse and apply one NDJSON line from a streamed response. Returns
/// `false` when `handler` asked to stop consuming the stream.
///
/// A blank line is skipped, and a line that is not valid JSON is a
/// protocol error rather than something to silently ignore — silently
/// dropping it would turn a malformed stream into a plausible-looking
/// short answer.
fn consume_stream_line(
    line: &[u8],
    handler: &dyn DeltaHandler,
    content: &mut String,
    tool_calls: &mut Vec<WireResponseToolCall>,
    done_reason: &mut Option<String>,
) -> Result<bool, OrbitError> {
    let text = std::str::from_utf8(line)
        .map_err(|e| {
            OrbitError::Provider(ProviderError::InvalidResponse {
                reason: format!("response stream was not valid UTF-8: {e}"),
            })
        })?
        .trim();
    if text.is_empty() {
        return Ok(true);
    }

    let chunk: StreamChunk = serde_json::from_str(text).map_err(|e| {
        OrbitError::Provider(ProviderError::InvalidResponse {
            reason: format!("malformed streaming chunk: {e}"),
        })
    })?;

    if let Some(reason) = chunk.done_reason {
        *done_reason = Some(reason);
    }
    if let Some(message) = chunk.message {
        if let Some(calls) = message.tool_calls {
            tool_calls.extend(calls);
        }
        if let Some(delta) = message.content.filter(|d| !d.is_empty()) {
            content.push_str(&delta);
            if !handler.on_delta(&delta) {
                return Ok(false);
            }
        }
    }
    Ok(true)
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

/// One newline-delimited object from a streamed `/api/chat` response.
/// `message` is absent on some keep-alive/final frames, so it is optional.
#[derive(Debug, Deserialize)]
struct StreamChunk {
    #[serde(default)]
    message: Option<WireResponseMessage>,
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

/// Exercises the real NDJSON line parser used by `chat_streaming`, without
/// needing a live Ollama server: the same function that consumes bytes off
/// the socket consumes these lines.
#[cfg(test)]
mod streaming_wire_tests {
    use super::*;
    use crate::provider::IgnoreDeltas;
    use std::sync::Mutex;

    struct Collector(Mutex<Vec<String>>);

    impl DeltaHandler for Collector {
        fn on_delta(&self, text: &str) -> bool {
            self.0.lock().unwrap().push(text.to_string());
            true
        }
    }

    fn feed(lines: &[&str], handler: &dyn DeltaHandler) -> (String, Vec<WireResponseToolCall>) {
        let mut content = String::new();
        let mut calls = Vec::new();
        let mut done = None;
        for line in lines {
            consume_stream_line(
                line.as_bytes(),
                handler,
                &mut content,
                &mut calls,
                &mut done,
            )
            .expect("line should parse");
        }
        (content, calls)
    }

    #[test]
    fn incremental_content_chunks_accumulate_in_order() {
        let collector = Collector(Mutex::new(Vec::new()));
        let (content, _) = feed(
            &[
                r#"{"message":{"content":"The "},"done":false}"#,
                r#"{"message":{"content":"watchdog "},"done":false}"#,
                r#"{"message":{"content":"resets."},"done":false}"#,
                r#"{"message":{"content":""},"done":true,"done_reason":"stop"}"#,
            ],
            &collector,
        );

        assert_eq!(content, "The watchdog resets.");
        assert_eq!(
            collector.0.lock().unwrap().concat(),
            content,
            "emitted deltas must reconstruct the accumulated content exactly"
        );
    }

    #[test]
    fn tool_calls_are_collected_from_whichever_chunk_carries_them() {
        let (_, calls) = feed(
            &[
                r#"{"message":{"content":""},"done":false}"#,
                r#"{"message":{"tool_calls":[{"function":{"name":"project.search","arguments":{"query":"brownout"}}}]},"done":false}"#,
                r#"{"done":true,"done_reason":"stop"}"#,
            ],
            &IgnoreDeltas,
        );

        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "project.search");
    }

    #[test]
    fn blank_lines_are_skipped() {
        let (content, _) = feed(
            &["", "   ", r#"{"message":{"content":"ok"},"done":true}"#],
            &IgnoreDeltas,
        );
        assert_eq!(content, "ok");
    }

    /// A corrupt frame must surface as a provider error, not be silently
    /// dropped -- dropping it would turn a broken stream into a
    /// plausible-looking short answer.
    #[test]
    fn a_malformed_chunk_is_a_provider_error() {
        let mut content = String::new();
        let mut calls = Vec::new();
        let mut done = None;
        let err = consume_stream_line(
            b"{not json}",
            &IgnoreDeltas,
            &mut content,
            &mut calls,
            &mut done,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            OrbitError::Provider(ProviderError::InvalidResponse { .. })
        ));
    }

    #[test]
    fn a_handler_that_stops_reports_false() {
        let mut content = String::new();
        let mut calls = Vec::new();
        let mut done = None;
        let keep_going = consume_stream_line(
            br#"{"message":{"content":"partial"},"done":false}"#,
            &|_: &str| false,
            &mut content,
            &mut calls,
            &mut done,
        )
        .unwrap();
        assert!(!keep_going);
        assert_eq!(content, "partial");
    }
}
