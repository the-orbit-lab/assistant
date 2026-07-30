# Agent Instructions

Read `docs/PROJECT_SPEC.md`, `docs/ARCHITECTURE.md`,
`docs/SECURITY.md`, and `docs/CONFIGURATION.md` before making changes.

Preserve the existing dependency boundaries between the agent, actions,
providers, MCP, projects, and workspaces.

Work from the current task scope. Do not implement unrelated roadmap items.

Do not weaken filesystem restrictions, permission enforcement, source
grounding, or MCP exposure filtering.

Run formatting, clippy, tests, and build before completing a task.

Do not commit, push, or modify remote resources unless explicitly requested.