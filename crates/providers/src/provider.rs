use orbit_core::{ModelRequest, ModelResponse, OrbitError};

/// Receives assistant text as it arrives from a streaming provider.
///
/// Returning `false` asks the provider to stop reading the stream and
/// return whatever it has accumulated so far. That is how a cancelled turn
/// stops generation mid-response without the provider layer needing to
/// know anything about sessions or cancellation tokens.
pub trait DeltaHandler: Send + Sync {
    fn on_delta(&self, text: &str) -> bool;
}

impl<F> DeltaHandler for F
where
    F: Fn(&str) -> bool + Send + Sync,
{
    fn on_delta(&self, text: &str) -> bool {
        self(text)
    }
}

/// A [`DeltaHandler`] that ignores every delta, for callers that want a
/// streaming call's other properties but not the text incrementally.
pub struct IgnoreDeltas;

impl DeltaHandler for IgnoreDeltas {
    fn on_delta(&self, _text: &str) -> bool {
        true
    }
}

/// A provider-independent chat model. Nothing about a specific provider
/// (Ollama's wire format, an HTTP client, auth headers) is visible past
/// this trait — the agent only ever sees [`ModelRequest`]/[`ModelResponse`].
#[async_trait::async_trait]
pub trait ModelProvider: Send + Sync {
    async fn chat(&self, request: &ModelRequest) -> Result<ModelResponse, OrbitError>;

    /// Whether [`ModelProvider::chat_streaming`] actually streams for this
    /// provider, as opposed to falling back to the default implementation.
    /// Front ends use it only to label the stream (see
    /// `orbit_core::EventPayload::ModelResponseStarted`); correctness never
    /// depends on it.
    fn supports_streaming(&self) -> bool {
        false
    }

    /// Complete `request`, reporting assistant text to `handler` as it
    /// becomes available.
    ///
    /// The default implementation delegates to [`ModelProvider::chat`] and
    /// reports the finished content as a single delta, so a provider that
    /// cannot stream stays fully usable and callers never need two code
    /// paths. In both the streaming and non-streaming case the same
    /// invariant holds, and it is the one the agent relies on:
    /// **concatenating every delta yields exactly the returned message's
    /// content.**
    ///
    /// Tool calling is unaffected by streaming: a response that requests
    /// tools comes back with the same `tool_calls` and
    /// [`orbit_core::FinishReason::ToolCalls`] either way.
    ///
    /// If `handler` returns `false`, the partial response accumulated so
    /// far is returned; the caller is responsible for knowing it asked for
    /// that and interpreting the result accordingly.
    async fn chat_streaming(
        &self,
        request: &ModelRequest,
        handler: &dyn DeltaHandler,
    ) -> Result<ModelResponse, OrbitError> {
        let response = self.chat(request).await?;
        if !response.message.content.is_empty() {
            handler.on_delta(&response.message.content);
        }
        Ok(response)
    }

    /// Human-readable identity for logs and `orbit doctor` (e.g.
    /// `ollama (qwen2.5:latest @ http://localhost:11434)`).
    fn describe(&self) -> String;
}
