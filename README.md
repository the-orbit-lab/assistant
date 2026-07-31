# Orbit

Orbit is a local-first AI engineering assistant. It understands a project
— its documentation, source code, configuration, and configured commands
— and answers questions grounded in that project's own files, with
sources shown for every claim. It is not a chatbot wrapper: reliable
operations (file access, search, running a build) are implemented as
deterministic, permission-checked actions, not left to the model's
judgment. Orbit also understands **workspaces**: a directory of several
sibling repositories, each still resolved and secured as its own
independent project — see [docs/WORKSPACES.md](docs/WORKSPACES.md).

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
providers, the desktop and voice interfaces themselves (their foundation —
stateful sessions, a structured event stream, streaming responses,
structured permissions, cancellation, and a JSON Lines bridge — *is*
built), persistent conversation memory, and vector search — see [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#what-is-not-built-yet)
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
| `orbit search <query>` | Deterministic local lexical search (BM25) — no model involved. `--verbose` explains the ranking. |
| `orbit ask <question>` | One agent turn: model + tools, answer + sources. |
| `orbit commands` | List configured commands and their required permission. |
| `orbit run <name>` | Run one configured command (permission-checked). |
| `orbit doctor` | Check config, Ollama connectivity, model availability, file discovery, MCP exposure and server initialization. |
| `orbit chat` | Stateful multi-turn session: streaming answers, live action status, project switching, permission prompts, cancellation. History lives in memory only. |
| `orbit mcp serve` | Serve this project's (or, with `--workspace`, workspace's) exposed actions over MCP stdio. |
| `orbit workspace` | Show workspace info; `orbit workspace init` to create one. |
| `orbit projects` | List every project registered in the active workspace. |
| `orbit app serve --jsonl` | Drive sessions over a JSON Lines protocol, for a desktop or voice front end. |

Global flags: `--project <name-or-dir>` (a registered project name/alias
when a workspace is active, or always a filesystem path), `--workspace
<dir>`, `--config <path>`, `--json`, `--model <name>`, `--ollama-endpoint
<url>`, `--yes` (approve `ask`-permission actions non-interactively),
`--verbose` (log resolved project root/config, discovered-file counts,
tools offered to the model, tool calls it made, and action results to
stderr — never file contents or secrets; `RUST_LOG` overrides it with
full `tracing` filter syntax). Exit code `2` means "no project
configuration found"; `1` is a general error; `0` is success.

```bash
orbit search "watchdog"
orbit ask "Why was the ESP32-C3 selected?"
orbit run test --yes
orbit --json project

# Multiple sibling repositories registered under one workspace:
orbit workspace
orbit --project obc project
orbit search "STM32" --projects docs,obc
orbit ask "Compare the documented STM32 decision with the OBC implementation."
```

See [docs/WORKSPACES.md](docs/WORKSPACES.md) for directory layout,
`.orbit/workspace.yaml`, discovery precedence, natural-language project
routing, permission isolation between projects, and workspace MCP mode.

## Sessions

`orbit chat` is a stateful session: it remembers the conversation, the
active project(s), every action it ran, and every source it collected, for
as long as the process lives. Inside a session:

```text
/projects        list the projects registered in this workspace
/use <a[,b]>     set the active project(s)
/status          show session id, mode, active projects, counters
/sources         re-print the sources collected in this session
/cancel          cancel the turn currently running (or press Ctrl-C)
/clear           forget the conversation, keep the session
/help            show this list
/exit            end the session
```

Answers stream as they are generated, actions report as they run, and an
`ask` permission pauses the turn until you answer it. Commands are handled
by Orbit itself and are never sent to the model. Nothing is written to
disk — closing the process discards the conversation.

See [docs/SESSIONS.md](docs/SESSIONS.md).

## Building a front end

Every interface — the terminal, a future desktop app, a future voice
interface — observes the same structured [Agent Event
Stream](docs/EVENTS.md): session and turn lifecycle, deterministic
retrieval, action start/finish, permission requests, per-project source
citations, streamed response deltas, and cancellation.

```bash
orbit app serve --jsonl
```

speaks that stream over stdin/stdout as newline-delimited JSON, so a GUI
never reimplements agent logic. See
[docs/APP_PROTOCOL.md](docs/APP_PROTOCOL.md) for the commands, the frames,
and notes for a SwiftUI client.

## MCP

```bash
orbit mcp serve --project /path/to/project
```

exposes whatever `mcp.expose` lists in that project's config to any MCP
host over stdio — see [docs/MCP.md](docs/MCP.md) for the Claude Code
connection config and how Orbit consumes *other* MCP servers as a client.
`orbit --workspace /path/to/orbit-lab mcp serve` exposes the six
`workspace.*` actions instead — see [docs/WORKSPACES.md](docs/WORKSPACES.md#mcp-workspace-mode).

## Configuration

Everything lives in `.orbit/project.yaml`: include/exclude context rules,
configured commands, per-action permissions (`allow`/`ask`/`deny`), model
provider settings, and MCP export/consumption config. See
[docs/CONFIGURATION.md](docs/CONFIGURATION.md),
[examples/project.yaml](examples/project.yaml),
[examples/mcp-project.yaml](examples/mcp-project.yaml), and the JSON
Schema at [schemas/project.schema.json](schemas/project.schema.json). A
workspace of several projects adds one more file, `.orbit/workspace.yaml`
— see [examples/workspace.yaml](examples/workspace.yaml) and
[schemas/workspace.schema.json](schemas/workspace.schema.json).

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
- Search is deterministic lexical ranking (BM25 over normalized tokens,
  with filename/path/heading/symbol signals) — no embeddings, no vector
  database, and no semantic matching. See
  [docs/SEARCH.md](docs/SEARCH.md#known-limitations).
- Workspace project routing (both natural-language scanning and
  name/alias resolution) is deterministic text matching, not semantic —
  see [docs/WORKSPACES.md](docs/WORKSPACES.md#known-limitations).
- Sessions are in-memory only; there is no persistent conversation
  memory, and a session runs one turn at a time.
- The desktop and voice interfaces are not built. Their foundation —
  sessions, events, streaming, structured permissions, cancellation, and
  the JSONL bridge — is.

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
