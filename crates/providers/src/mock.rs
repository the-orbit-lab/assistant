use std::sync::Mutex;

use orbit_core::{ModelRequest, ModelResponse, OrbitError, ProviderError};

use crate::provider::ModelProvider;

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
}

impl MockProvider {
    pub fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
            recorded: Mutex::new(Vec::new()),
        }
    }

    pub fn recorded_requests(&self) -> Vec<ModelRequest> {
        self.recorded
            .lock()
            .expect("mock provider mutex poisoned")
            .clone()
    }
}

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    fn describe(&self) -> String {
        "mock".to_string()
    }

    async fn chat(&self, request: &ModelRequest) -> Result<ModelResponse, OrbitError> {
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
