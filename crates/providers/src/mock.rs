use std::sync::Mutex;

use orbit_core::{ModelRequest, ModelResponse, OrbitError, ProviderError};

use crate::provider::ModelProvider;

/// A scripted provider for tests: returns queued responses in order, or an
/// error once the queue is exhausted. Lets agent-loop tests exercise
/// multi-turn tool-calling without a live Ollama server.
pub struct MockProvider {
    responses: Mutex<std::collections::VecDeque<ModelResponse>>,
}

impl MockProvider {
    pub fn new(responses: Vec<ModelResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into()),
        }
    }
}

#[async_trait::async_trait]
impl ModelProvider for MockProvider {
    fn describe(&self) -> String {
        "mock".to_string()
    }

    async fn chat(&self, _request: &ModelRequest) -> Result<ModelResponse, OrbitError> {
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
