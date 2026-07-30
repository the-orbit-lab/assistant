# MCP

Orbit is both an MCP **server** (other hosts consume Orbit's actions) and
an MCP **client** (Orbit consumes other servers' tools). Both use the
official Rust SDK, [`rmcp`](https://crates.io/crates/rmcp), over the
`stdio` transport.

## As a server: exposing Orbit to Claude Code

```bash
orbit mcp serve
```

This starts a JSON-RPC server on stdio, reusing the exact same
`ActionRegistry`, `ActionContext`, and permission enforcement as the CLI
and agent (see [ARCHITECTURE.md](ARCHITECTURE.md)). Only actions listed in
`mcp.expose` are visible — nothing is exported by default:

```yaml
mcp:
  expose:
    - project.information
    - project.search
    - project.read_file
```

### Permission behavior for `allow` / `ask` / `deny`

`mcp.expose` lists action *names*; whether an exposed name is actually
reachable depends on its effective permission, resolved by
`orbit_mcp_server::compute_exposure` the same way the CLI and agent
resolve permissions everywhere else:

```text
allow → listed in tools/list and callable
ask   → never listed. This transport has no way to obtain a safe,
        explicit per-call approval, so rather than inventing an automatic
        approval mechanism, the action is excluded. Calling it by name
        anyway returns a specific error naming the fix: set it to
        `allow` in .orbit/project.yaml.
deny  → never listed, exactly as if it were absent from mcp.expose.
```

A name in `mcp.expose` that doesn't match any registered action is also
excluded, with its own warning (a likely typo) rather than silently doing
nothing. Duplicate entries collapse to one. Every one of these cases
produces an actionable line from `orbit mcp serve` (stderr, at startup)
and from `orbit doctor` — see below.

### Connecting Claude Code

Point Claude Code at the built `orbit` binary and the project it should
serve. Two ways to register it (confirmed against Claude Code 2.1.220):

**`claude mcp add`** (recommended — validates the command for you):

```bash
claude mcp add orbit -- /absolute/path/to/orbit --project /absolute/path/to/project mcp serve
```

**A project-scoped `.mcp.json`** (or your global Claude Code MCP config),
using the same `mcpServers` shape either file uses:

```json
{
  "mcpServers": {
    "orbit": {
      "command": "/absolute/path/to/orbit",
      "args": [
        "--project",
        "/absolute/path/to/project",
        "mcp",
        "serve"
      ]
    }
  }
}
```

Both forms are equivalent — `orbit`'s `--project` flag is a normal global
flag, so it can appear before or after the `mcp serve` subcommand. Use
absolute paths for both the binary and the project; replace the
placeholders above with your own. Any MCP-compatible host works the same
way, since Orbit speaks the standard stdio JSON-RPC protocol — Orbit does
not depend on Claude Code specifically, and never requires an
`ANTHROPIC_API_KEY` (Claude Code's own authentication is external to, and
irrelevant to, the Orbit MCP server it's talking to).

### Verifying the connection

Ask the connected host (in Claude Code, a normal prompt) to:

1. list Orbit's tools;
2. call `project.information`;
3. search for `engineering assistant`;
4. read `README.md`;
5. summarize the project using the sources those calls returned.

Or drive the same sequence yourself with any raw JSON-RPC-over-stdio
client — `crates/cli/tests/mcp_protocol.rs` does exactly this against a
real `orbit mcp serve` subprocess and is a working reference for the
message shapes.

## As a server: workspace mode

```bash
orbit --workspace /path/to/orbit-lab mcp serve
```

exposes **exactly the six `workspace.*` actions**
(`workspace.information`, `workspace.list_projects`,
`workspace.project_information`, `workspace.search`,
`workspace.read_file`, `workspace.list_project_files`) —
never one dynamically generated tool per registered repository, and never
a project's own native command-execution actions. This is the same
`OrbitMcpServer`, the same `compute_exposure`, and the same `self_check`
as single-project mode, pointed at a workspace-scoped `ActionRegistry`
and a synthetic workspace-level `ActionContext` instead of one project's —
workspace mode required zero changes to this crate. Tool call results
(e.g. `workspace.search`) carry the same project-scoped, structured
`sources` payload described in [WORKSPACES.md](WORKSPACES.md), so a host
can tell which registered project each result came from.

**Claude Code, single-project mode** (unchanged):

```json
{
  "mcpServers": {
    "orbit": {
      "command": "/absolute/path/to/orbit",
      "args": ["--project", "/absolute/path/to/obc", "mcp", "serve"]
    }
  }
}
```

**Claude Code, workspace mode:**

```json
{
  "mcpServers": {
    "orbit-workspace": {
      "command": "/absolute/path/to/orbit",
      "args": ["--workspace", "/absolute/path/to/orbit-lab", "mcp", "serve"]
    }
  }
}
```

Both forms use the same binary and the same `mcpServers` shape — only the
flag changes. See [WORKSPACES.md](WORKSPACES.md#mcp-workspace-mode) for
the full workspace design.

## As a client: consuming other MCP servers

```yaml
mcp:
  servers:
    github:
      transport: stdio
      command: github-mcp-server
      args: []
      enabled: true
```

At the start of an `orbit ask` or `orbit chat` session, Orbit spawns every
`enabled` server, lists its tools, and wraps each one as an action named
`mcp.<server>.<tool>` (e.g. `mcp.github.create_issue`). These are
registered into the *same* `ActionRegistry` as native actions:

- **Permission enforcement is identical.** External tools default to
  `ask` (stricter than most native actions, since they run code Orbit did
  not write); a project can override this per action name, same as any
  native action.
- **Namespacing prevents collisions.** A server literally named `project`
  or `command` is rejected at connect time rather than allowed to shadow
  a native action.
- **A failed server degrades gracefully.** If `github-mcp-server` isn't
  installed, or fails to initialize, `orbit ask`/`orbit chat` prints a
  warning and continues with whatever did connect — native actions still
  work.
- **Clean shutdown.** Every connected server is cancelled when the
  session ends.

## `orbit doctor`

Checks, in addition to the project/Ollama checks in [OLLAMA.md](OLLAMA.md):

- `mcp export configuration` / `mcp exposure`: runs the exact exposure
  resolution above and reports one `WARN` line per excluded entry (e.g.
  `MCP exposure `project.write_file` requires confirmation and cannot be
  used through the current non-interactive MCP transport.`), or one `OK`
  with a count if everything configured resolves cleanly.
- `mcp server initialization`: actually constructs `OrbitMcpServer` for
  the active project and runs a real `initialize` + `tools/list` round
  trip against it over an in-process transport (no subprocess, no real
  `stdio`) — a genuine check that the server can start and answer, not
  just that its constructor doesn't panic.
- `mcp stdout reservation`: informational; the real guarantee (nothing but
  protocol frames ever reaches stdout) is enforced by the protocol
  integration tests below, which read every line of a real subprocess's
  stdout and fail if any of it isn't valid JSON-RPC.

In workspace mode, `orbit doctor` additionally reports `workspace action
registry` (the workspace `ActionRegistry` builds cleanly), `workspace mcp
exposure` (the six exposed action names), and `workspace mcp self-check`
(a real `initialize` + `tools/list` round trip against a workspace-scoped
`OrbitMcpServer`, reusing `self_check` exactly as above) — see
[WORKSPACES.md](WORKSPACES.md#diagnostics-orbit-doctor) for the full
per-project diagnostic format.

## Status and limitations

- Transport: `stdio` only, both directions. Streamable HTTP is not wired
  up in this version.
- The client reconnects fresh at the start of every `ask`/`chat` session;
  there's no persistent daemon or connection pooling across invocations.
- External tool results are passed through as text/structured content;
  Orbit does not currently synthesize `SourceReference`s for MCP tool
  output the way it does for native file-backed actions.

## Testing

- `crates/cli/tests/mcp_protocol.rs`: spawns the real, compiled `orbit`
  binary as a subprocess and speaks newline-delimited JSON-RPC to its
  actual stdin/stdout — `initialize`, `tools/list`, and `tools/call`
  against `project.information`/`list_files`/`read_file`/`search`, plus
  rejection of `.env`, path traversal, and unexposed/unknown tool names.
  Every line read from stdout is asserted to be valid JSON: this is what
  actually proves the protocol channel is never polluted by a stray
  `println!`, not just that the code looks like it wouldn't do that.
- `crates/cli/tests/workspace_mcp_protocol.rs`: the same real-subprocess
  approach against `orbit --workspace <dir> mcp serve` — asserts exactly
  the six `workspace.*` tools are listed (never one per repository),
  that `workspace.search` results carry correct per-project attribution
  and never cross-attribute a match to the wrong project, that
  `workspace.read_file` can't escape into a sibling project, and that an
  unregistered project name is rejected over the wire.
- `crates/mcp-server` unit/protocol tests spin up the real server against
  a real client over an in-process `tokio::io::duplex` pipe — genuine
  `initialize`/`tools/list`/`tools/call` round trips, not hand-built
  request contexts — covering exposure filtering for `allow`/`ask`/`deny`,
  unknown-action warnings, and structured content.
- `crates/mcp-client` tests cover namespacing and graceful degradation
  (an unreachable command produces a warning, not a panic) without
  needing a real external server installed.
