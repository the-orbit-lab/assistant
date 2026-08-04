use std::sync::Mutex;

use orbit_core::{ModelRequest, ModelResponse, OrbitError, ProviderError};

use crate::provider::{DeltaHandler, ModelProvider};

/// A scripted provider for tests: returns queued responses in order, or an
/// error once the queue is exhausted. Lets agent-loop tests exercise
/// multi-turn tool-calling without a live Ollama server.
///
/// Every request it receives is recorded (see [`MockProvider::recorded_requests`])
/// so tests can assert on exactly what context reached "the model" -- e.g.
/// that grounding content from a deterministic retrieval step was actually
/// present in the messages, not just that the final answer happened to
/// look right.
pub struct MockProvider {
    responses: Mutex<std::collections::VecDeque<ModelResponse>>,
    recorded: Mutex<Vec<ModelRequest>>,
    streaming: bool,
    delta_chunk_chars: usize,
}

impl MockProvider {
    /// A provider that does not stream. `chat_streaming` therefore takes
    /// the [`ModelProvider`] default path, which is exactly what a real
    /// non-streaming provider would do — so tests using this constructor
    /// verify that compatibility.
    pub fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            recorded: Mutex::new(Vec::new()),
            streaming: false,
            delta_chunk_chars: 4,
        }
    }

    /// A provider that reports itself as streaming and hands back each
    /// scripted response's content in small deltas, so the agent's
    /// streaming path (and mid-stream cancellation) can be exercised
    /// without a live model.
    pub fn streaming(responses: Vec<ModelResponse>) -> Self {
        Self {
            streaming: true,
            ..Self::new(responses)
        }
    }

    pub fn with_delta_chunk_chars(mut self, chars: usize) -> Self {
        self.delta_chunk_chars = chars.max(1);
        self
    }

    pub fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.recorded
            .lock()
            .expect("mock provider mutex poisoned")
            .clone()
    }

    fn take_next(&self, request: &ModelRequest) -> Result<ModelResponse, OrbitError> {
        self.recorded
            .lock()
            .expect("mock provider mutex poisoned")
            .push(request.clone());
        let mut queue = self.responses.lock().expect("mock provider mutex poisoned");
        queue.pop_front().ok_or_else(|| {
            OrbitError::Provider(ProviderError::Other {
                reason: "mock provider has no more scripted responses".to_string(),
            })
        })
    }
}

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    fn describe(&self) -> String {
        "mock".to_string()
    }

    fn supports_streaming(&self) -> bool {
        self.streaming
    }

    async fn chat(&self, request: &ModelRequest) -> Result<ModelResponse, OrbitError> {
        self.take_next(request)
    }

    async fn chat_streaming(
        &self,
        request: &ModelRequest,
        handler: &dyn DeltaHandler,
    ) -> Result<ModelResponse, OrbitError> {
        if !self.streaming {
            // Exercise the real default path rather than reimplementing it.
            let response = self.chat(request).await?;
            if !response.message.content.is_empty() {
                handler.on_delta(&response.message.content);
            }
            return Ok(response);
        }

        let response = self.take_next(request)?;
        // Split on character boundaries so the accumulated text is exactly
        // the original content, matching the trait's delta invariant.
        let chars: Vec<char> = response.message.content.chars().collect();
        let mut delivered = String::new();
        for piece in chars.chunks(self.delta_chunk_chars) {
            let text: String = piece.iter().collect();
            delivered.push_str(&text);
            if !handler.on_delta(&text) {
                // Stopped early: report only what was actually delivered,
                // so a cancelled stream cannot claim more output than the
                // caller saw.
                let mut partial = response;
                partial.message.content = delivered;
                return Ok(partial);
            }
        }
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_core::{FinishReason, Message};

    #[tokio::test]
    async fn returns_scripted_responses_in_order() {
        let provider = MockProvider::new(vec![ModelResponse {
            message: Message::assistant("hi"),
            finish_reason: FinishReason::Stop,
        }]);
        let request = ModelRequest {
            model: "mock".to_string(),
            messages: vec![],
            tools: vec![],
            timeout: std::time::Duration::from_secs(1),
        };
        let response = provider.chat(&request).await.unwrap();
        assert_eq!(response.message.content, "hi");
        assert!(provider.chat(&request).await.is_err());
    }
}

#[cfg(test)]
mod streaming_tests {
    use super::*;
    use crate::provider::IgnoreDeltas;
    use orbit_core::{FinishReason, Message, ToolCall};
    use std::sync::Mutex as StdMutex;

    fn request() -> ModelRequest {
        ModelRequest {
            model: "mock".to_string(),
            messages: vec![],
            tools: vec![],
            timeout: std::time::Duration::from_secs(1),
        }
    }

    fn stop(content: &str) -> ModelResponse {
        ModelResponse {
            message: Message::assistant(content),
            finish_reason: FinishReason::Stop,
        }
    }

    /// The invariant every consumer relies on: concatenated deltas equal
    /// the returned content.
    #[tokio::test]
    async fn streaming_deltas_accumulate_to_the_final_content() {
        let provider = MockProvider::streaming(vec![stop("The watchdog resets the system.")]);
        let seen = StdMutex::new(String::new());
        let response = provider
            .chat_streaming(&request(), &|delta: &str| {
                seen.lock().unwrap().push_str(delta);
                true
            })
            .await
            .unwrap();

        assert_eq!(*seen.lock().unwrap(), response.message.content);
        assert_eq!(response.message.content, "The watchdog resets the system.");
    }

    /// A provider that cannot stream must still work through
    /// `chat_streaming`, reporting its whole answer as one delta.
    #[tokio::test]
    async fn non_streaming_provider_still_satisfies_the_delta_invariant() {
        let provider = MockProvider::new(vec![stop("Brownout recovery is documented.")]);
        assert!(!provider.supports_streaming());

        let seen = StdMutex::new(String::new());
        let response = provider
            .chat_streaming(&request(), &|delta: &str| {
                seen.lock().unwrap().push_str(delta);
                true
            })
            .await
            .unwrap();

        assert_eq!(*seen.lock().unwrap(), response.message.content);
    }

    #[tokio::test]
    async fn a_handler_that_stops_truncates_the_response_to_what_was_delivered() {
        let provider =
            MockProvider::streaming(vec![stop("aaaabbbbccccdddd")]).with_delta_chunk_chars(4);
        let seen = StdMutex::new(String::new());
        let response = provider
            .chat_streaming(&request(), &|delta: &str| {
                seen.lock().unwrap().push_str(delta);
                // Stop after the first delta.
                false
            })
            .await
            .unwrap();

        assert_eq!(*seen.lock().unwrap(), "aaaa");
        assert_eq!(
            response.message.content, "aaaa",
            "a cancelled stream must not report text the caller never saw"
        );
    }

    /// Streaming must not change tool-calling behavior.
    #[tokio::test]
    async fn tool_calls_survive_the_streaming_path() {
        let provider = MockProvider::streaming(vec![ModelResponse {
            message: Message::assistant_tool_calls(vec![ToolCall {
                id: "call_0".to_string(),
                name: "workspace.search".to_string(),
                arguments: serde_json::json!({"query": "STM32"}),
            }]),
            finish_reason: FinishReason::ToolCalls,
        }]);

        let response = provider
            .chat_streaming(&request(), &IgnoreDeltas)
            .await
            .unwrap();
        assert_eq!(response.finish_reason, FinishReason::ToolCalls);
        assert_eq!(response.message.tool_calls.len(), 1);
        assert_eq!(response.message.tool_calls[0].name, "workspace.search");
    }
}
