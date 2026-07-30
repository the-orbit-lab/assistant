# Orbit

Orbit is a local-first AI engineering assistant. It understands a single
project at a time — its documentation, source code, configuration, and
configured commands — and answers questions grounded in that project's
own files, with sources shown for every claim. It is not a chatbot
wrapper: reliable operations (file access, search, running a build) are
implemented as deterministic, permission-checked actions, not left to the
model's judgment.

Orbit is both:

1. an independent agent that uses a local [Ollama](https://ollama.com)
   model to interpret requests and call actions;
2. an MCP server, so the same actions can be used from Claude Code,
   ChatGPT, or any other MCP-compatible host — and an MCP client, so Orbit
   can in turn consume other MCP servers.

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for how these fit
together, and [docs/SECURITY.md](docs/SECURITY.md) for what's actually
enforced and where.

## Status

This is a working first version, not a prototype with placeholders: every
command below runs end-to-end against real project files and a real local
Ollama instance, and the MCP server has been exercised against a real MCP
client over the wire protocol. What's *not* built yet: Anthropic/OpenAI
providers, a desktop or voice interface, persistent conversation memory,
and vector search — see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#what-is-not-built-yet)
and [ADR 0001](docs/architecture/0001-scope-deviation-from-project-spec.md)
for the reasoning.

## Installation

Requires Rust (edition 2024; stable toolchain, 1.91+) and, for the agent
and `orbit doctor`, a local Ollama install.

```bash
cargo build --workspace --release
# binary at target/release/orbit
```

Or run directly with `cargo run -p orbit-cli --`.

## Ollama setup

```bash
brew install ollama          # or see https://ollama.com/download
ollama serve                  # if not already running
ollama pull qwen2.5:latest    # or any tool-calling-capable model
```

No API key is used or required. See [docs/OLLAMA.md](docs/OLLAMA.md) for
details, error handling, and how model availability is checked.

## Quick start

```bash
cd your-project
orbit init                       # creates .orbit/project.yaml
orbit doctor                     # checks config, Ollama, file discovery
orbit ask "What does this project do?"
```

## CLI

| Command | Does |
|---|---|
| `orbit init` | Create `.orbit/project.yaml` (`--force` to overwrite). |
| `orbit project` | Show name, root, provider, commands, effective permissions, MCP config. |
| `orbit files` | List every file the config allows Orbit to see. |
| `orbit search <query>` | Deterministic local search — no model involved. |
| `orbit ask <question>` | One agent turn: model + tools, answer + sources. |
| `orbit commands` | List configured commands and their required permission. |
| `orbit run <name>` | Run one configured command (permission-checked). |
| `orbit doctor` | Check config, Ollama connectivity, model availability, file discovery. |
| `orbit chat` | Multi-turn session; history lives in memory only. |
| `orbit mcp serve` | Serve this project's exposed actions over MCP stdio. |

Global flags: `--project <dir>`, `--config <path>`, `--json`, `--model
<name>`, `--ollama-endpoint <url>`, `--yes` (approve `ask`-permission
actions non-interactively), `--verbose` (log resolved project root/config,
discovered-file counts, tools offered to the model, tool calls it made,
and action results to stderr — never file contents or secrets; `RUST_LOG`
overrides it with full `tracing` filter syntax). Exit code `2` means "no
project configuration found"; `1` is a general error; `0` is success.

```bash
orbit search "watchdog"
orbit ask "Why was the ESP32-C3 selected?"
orbit run test --yes
orbit --json project
```

## MCP

```bash
orbit mcp serve --project /path/to/project
```

exposes whatever `mcp.expose` lists in that project's config to any MCP
host over stdio — see [docs/MCP.md](docs/MCP.md) for the Claude Code
connection config and how Orbit consumes *other* MCP servers as a client.

## Configuration

Everything lives in `.orbit/project.yaml`: include/exclude context rules,
configured commands, per-action permissions (`allow`/`ask`/`deny`), model
provider settings, and MCP export/consumption config. See
[docs/CONFIGURATION.md](docs/CONFIGURATION.md),
[examples/project.yaml](examples/project.yaml),
[examples/mcp-project.yaml](examples/mcp-project.yaml), and the JSON
Schema at [schemas/project.schema.json](schemas/project.schema.json).

## Security model

Enforced in application code, never by asking the model nicely: a path
security boundary (traversal/symlink/absolute-path rejection, mandatory
excludes for `.git`, `.orbit`, secrets, `.env*`, keys/certs), commands run
as `program` + argument array with no shell, bounded file sizes and
command output, and a permission system the model cannot change. Full
details in [docs/SECURITY.md](docs/SECURITY.md).

## Known limitations

- Only Ollama is implemented as a model provider.
- MCP transport is `stdio` only (no streamable HTTP yet).
- Duplicate YAML mapping keys in `.orbit/project.yaml` are not detected as
  a config error (standard YAML last-value-wins behavior).
- `orbit chat` history is in-process only; nothing is persisted to disk.
- Local search is filename/heading/content matching with simple ranking —
  no embeddings, no vector database.

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace
```

Tests that require a live Ollama instance are `#[ignore]`d by default; run
them explicitly with `cargo test -p orbit-providers -- --ignored`.

## License

Apache-2.0. See [LICENSE](LICENSE).
