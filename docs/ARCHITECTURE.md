# Architecture

Orbit owns the agent and the actions. MCP only imports and exports
capabilities — it is an adapter at the edge, never the internal execution
mechanism.

## Layers

```mermaid
flowchart TD
    User --> CLI
    CLI --> Agent[Orbit Agent]
    Agent --> Provider[Model Provider\nOllama]
    Agent --> Runtime[Action Runtime\nActionRegistry]
    Runtime --> Native[Native actions\nproject.*, command.*]
    Runtime --> External[External MCP tools\nmcp.&lt;server&gt;.&lt;tool&gt;]
    External --> McpClient[orbit-mcp-client] --> ExtServer[External MCP server\nstdio child process]
    Native --> FS[Project files, Git, configured commands]

    Host[Claude Code / ChatGPT / other MCP host] --> McpServer[orbit-mcp-server]
    McpServer --> Runtime
```

The Orbit agent calls the Action Runtime **directly**. It does not connect
to Orbit's own MCP server to run a native action — `orbit-mcp-server`
exists purely so *external* hosts can reach the same actions, filtered by
`mcp.expose`.

## Crates

```text
crates/
├── core         shared types: messages, model requests/responses, actions,
│                permissions, sources, execution records, errors
├── project      .orbit/project.yaml, root discovery, path/exclude
│                security, file discovery, deterministic search
├── actions      Action trait + ActionRegistry + the six native actions
├── providers    ModelProvider trait, Ollama provider, mock provider
├── agent        the tool-calling loop; merges native + external-MCP
│                actions into one registry for a session
├── mcp-client   Orbit as an MCP client (consumes external stdio servers)
├── mcp-server   Orbit as an MCP server (exposes filtered native actions)
└── cli          the `orbit` binary
```

Dependency direction (no cycles):

```text
core
├── project
├── actions   (project)
└── providers

agent
├── core, actions, providers
└── mcp-client

mcp-server → actions (+ core)
mcp-client → actions, project (+ core)

cli → agent, mcp-server, actions, providers, project, core
```

`orbit-actions`' public API has no MCP types in it. `orbit-agent` depends
on `orbit-mcp-client` (to consume external servers as a session starts) but
never on `orbit-mcp-server`. Ollama-specific wire types stay inside
`orbit-providers::ollama` and never appear in `orbit-core` or `orbit-agent`.

## Request flow (`orbit ask`)

```mermaid
sequenceDiagram
    participant U as User
    participant CLI
    participant A as Agent
    participant M as Ollama
    participant R as ActionRegistry

    U->>CLI: orbit ask "..."
    CLI->>A: run(history, question)
    A->>M: chat(messages, tools)
    M-->>A: tool_calls: project.search(...)
    A->>R: execute("project.search", input)
    R->>R: validate input, check permission
    R-->>A: ActionOutput{data, sources}
    A->>M: chat(messages + tool result)
    M-->>A: final answer
    A-->>CLI: AgentOutcome{answer, sources, records}
    CLI-->>U: answer + Sources: list
```

Every tool call goes through the same `ActionRegistry::execute`: input
validation, then permission enforcement (`allow`/`ask`/`deny`), then
execution, then an `ExecutionRecord`. The model can request an action; it
can never grant itself permission to run one.

## MCP: both directions

- **Server** (`orbit mcp serve`): wraps `ActionRegistry` behind the MCP
  `stdio` transport. `list_tools`/`call_tool` are the only two things it
  adds; everything else — validation, permissions, execution — is the same
  code path the CLI and agent use. Only actions listed in `mcp.expose` are
  visible.
- **Client** (`orbit-mcp-client`, used by `orbit ask`/`orbit chat`):
  connects to each enabled server under `mcp.servers`, lists its tools, and
  wraps each one as an ordinary `Action` named `mcp.<server>.<tool>`. These
  get registered into the *same* `ActionRegistry` as native actions, so
  permission enforcement, execution records, and the agent loop do not
  need to know an action came from outside Orbit. A server that fails to
  start produces a warning, not a crash — the rest of the session still
  works.

See [MCP.md](MCP.md) for configuration and the Claude Code connection.

## What is not built yet

- Anthropic/OpenAI providers (the trait supports adding them without
  touching the agent).
- A desktop application or voice interface (the CLI is a thin layer over
  `orbit-agent`; neither interface requires moving agent logic).
- Persistent conversation memory (`orbit chat` keeps history in process
  memory only, by design — see [SECURITY.md](SECURITY.md)).
- A vector database or embedding-based search (local search in
  `orbit-project::search` is deterministic and file-based).
