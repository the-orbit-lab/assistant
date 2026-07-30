use orbit_core::{ModelRequest, ModelResponse, OrbitError};

/// A provider-independent chat model. Nothing about a specific provider
/// (Ollama's wire format, an HTTP client, auth headers) is visible past
/// this trait — the agent only ever sees [`ModelRequest`]/[`ModelResponse`].
#[async_trait::async_trait]
pub trait ModelProvider: Send + Sync {
    async fn chat(&self, request: &ModelRequest) -> Result<ModelResponse, OrbitError>;

    /// Human-readable identity for logs and `orbit doctor` (e.g.
    /// `ollama (qwen2.5:latest @ http://localhost:11434)`).
    fn describe(&self) -> String;
}
