//! Provider-independent model abstraction, plus the Ollama and mock
//! implementations. Ollama-specific wire types never escape [`ollama`] —
//! everything else in Orbit talks to [`ModelProvider`].

pub mod mock;
pub mod ollama;
pub mod provider;

pub use mock::MockProvider;
pub use ollama::OllamaProvider;
pub use provider::ModelProvider;
