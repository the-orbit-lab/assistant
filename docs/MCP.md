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

An action left off this list does not appear in `tools/list` and cannot be
called by name; the server never prompts interactively, so any exposed
action left at the `ask` permission is effectively unreachable through MCP
until the project sets it to `allow`.

### Connecting Claude Code

Add Orbit as a project-scoped or user-scoped MCP server, pointing at the
built binary and the project you want it to serve:

```json
{
  "mcpServers": {
    "orbit": {
      "command": "/path/to/orbit",
      "args": ["mcp", "serve", "--project", "/path/to/your/project"]
    }
  }
}
```

(Exact file location and CLI for registering this depends on how you run
Claude Code — see Claude Code's own MCP documentation for where this
config lives.) Any MCP-compatible host works the same way, since Orbit
speaks the standard stdio JSON-RPC protocol — Orbit does not depend on
Claude Code specifically, and never requires an Anthropic API key.

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

## Status and limitations

- Transport: `stdio` only, both directions. Streamable HTTP is not wired
  up in this version.
- The client reconnects fresh at the start of every `ask`/`chat` session;
  there's no persistent daemon or connection pooling across invocations.
- External tool results are passed through as text/structured content;
  Orbit does not currently synthesize `SourceReference`s for MCP tool
  output the way it does for native file-backed actions.

## Testing without a live external server

- `crates/mcp-server` tests spin up the real server against a real client
  over an in-process `tokio::io::duplex` pipe — a genuine `initialize` +
  `tools/list` + `tools/call` round trip, not a hand-built request
  context.
- `crates/mcp-client` tests cover namespacing and graceful degradation
  (an unreachable command produces a warning, not a panic) without
  needing a real external server installed.
