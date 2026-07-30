# ADR 0001: This implementation follows a broader spec than `docs/PROJECT_SPEC.md`

## Status

Accepted.

## Context

`docs/PROJECT_SPEC.md`, as checked into the repository, describes a smaller
first version of Orbit: Anthropic as the model provider, no MCP support,
and a six-crate workspace (`core`, `project`, `actions`, `provider`,
`agent`, `cli`).

The implementation task actually given for this build is more detailed and
broader in three material ways:

1. **Model provider.** Ollama (local, no API key) instead of Anthropic.
2. **MCP.** Orbit both exposes native actions to external MCP hosts
   (server) and consumes external MCP servers itself (client), in this
   first version rather than a later one.
3. **Workspace shape.** Eight crates — `core`, `project`, `actions`,
   `providers`, `agent`, `mcp-client`, `mcp-server`, `cli` — so MCP-specific
   code has its own crate boundary and never leaks into `actions` or
   `agent`'s public API.

Per the workflow both documents describe ("make reasonable engineering
decisions," "document the change clearly" when it materially differs from
`PROJECT_SPEC.md" instead of rewriting that file to match implementation
shortcuts), this ADR records the deviation rather than editing
`docs/PROJECT_SPEC.md`.

## Decision

Build to the broader spec: Ollama provider, MCP server *and* client in
this version, eight-crate workspace. `docs/PROJECT_SPEC.md` is left as
historical context, not updated.

## Consequences

- Anthropic support is not implemented. The provider trait
  (`orbit-providers::ModelProvider`) is provider-independent, so adding it
  later does not require touching the agent or action layers.
- The MCP client (`orbit-mcp-client`) degrades gracefully: a configured
  external server that fails to start or initialize produces a warning,
  not a hard failure, so a session still works with native actions alone.
- Two extra crate boundaries (`mcp-client`, `mcp-server`) exist so that
  `orbit-actions`' public API never has to import an MCP type, and
  `orbit-agent` never has to import an MCP *server* type — it only depends
  on `orbit-mcp-client` to merge external tools into the same registry it
  already uses for native actions.
