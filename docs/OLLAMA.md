# Ollama integration

Orbit's first-class model provider is a local [Ollama](https://ollama.com)
instance. No API key is used or required.

## Setup

```bash
brew install ollama   # or see https://ollama.com/download
ollama serve           # if it isn't already running as a service
ollama pull qwen2.5:latest
```

Orbit talks to `http://localhost:11434` by default. Override it per
project in `.orbit/project.yaml` (`model.endpoint`) or per invocation with
`--ollama-endpoint <url>`.

## Model choice

Orbit works best with a model that supports Ollama's tool-calling API
(`qwen2.5`, `qwen3`, `llama3.1`, and similar). A model without tool-calling
support can still answer directly, but won't be able to call
`project.search`, `project.read_file`, or any other action, so its answers
won't be grounded in project sources.

## What Orbit sends and expects

`orbit-providers::ollama` posts to `/api/chat` with `stream: false`,
translating Orbit's provider-independent `ModelRequest` into Ollama's wire
format: messages with `role`/`content`, and tools as
`{"type": "function", "function": {name, description, parameters}}`.

A tool-calling response comes back as:

```json
{
  "message": {
    "role": "assistant",
    "content": "",
    "tool_calls": [{ "function": { "name": "...", "arguments": { } } }]
  },
  "done_reason": "stop"
}
```

Ollama does not assign an id to each tool call, so Orbit generates one
(`call_0`, `call_1`, ...) to correlate the follow-up tool-result message.
None of this wire format is visible outside `orbit-providers::ollama` —
the rest of Orbit only sees `orbit-core::{ModelRequest, ModelResponse}`.

## Error handling

- **Connection failures / timeouts** surface as `ProviderError::ConnectionFailed`
  or `ProviderError::Timeout`, not a panic.
- **Missing model** (`HTTP 404` from `/api/chat`) surfaces as
  `ProviderError::ModelUnavailable`, with the exact `ollama pull <model>`
  command to run — Orbit never downloads a model on its own.
- **Malformed responses** (unexpected JSON shape) surface as
  `ProviderError::InvalidResponse`.

## `orbit doctor`

Checks, in order: config validity, project root, permission
configuration, MCP export configuration, file discovery, Ollama
connectivity (`GET /api/tags`), and whether the configured model is
already pulled. A missing model is a `WARN`, not a `FAIL` — Orbit tells
you the exact pull command instead of failing the whole check.

## Testing

`crates/providers/src/ollama.rs` has one `#[ignore]`d test,
`live_ollama_reports_connectivity_and_chats`, that exercises a real local
Ollama instance. It is opt-in and excluded from the default test run:

```bash
cargo test -p orbit-providers -- --ignored live_ollama
```

Every other provider test (wire-format mapping, tag matching, the mock
provider) runs without Ollama installed.
