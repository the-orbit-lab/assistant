# Configuration

Every project Orbit works on has a `.orbit/project.yaml`. Run `orbit init`
to create one, or copy [`examples/project.yaml`](../examples/project.yaml).
A JSON Schema is kept in sync with the Rust types at
[`schemas/project.schema.json`](../schemas/project.schema.json).

## Locating the config

Orbit walks upward from the current directory looking for
`.orbit/project.yaml`, stopping at the first one found — like `.git`
discovery. Two flags override this:

- `--project <dir>`: use this directory's `.orbit/project.yaml` directly,
  no searching.
- `--config <path>`: use this exact config file path.

If the discovered config would be *outside* the nearest `.git` repository
containing the current directory, Orbit refuses to use it
(`AmbiguousProjectRoot`) rather than silently applying a distant ancestor
project's rules to an unrelated nested repo. Run Orbit from within the
intended project, or pass `--project` explicitly.

## Fields

```yaml
version: 1                 # only 1 is currently supported

project:
  name: obc                # required
  type: embedded-system    # defaults to "software"
  description: "..."       # defaults to ""

model:
  provider: ollama          # defaults to "ollama" (the only provider today)
  model: qwen2.5:latest     # defaults to "qwen2.5:latest"
  endpoint: http://localhost:11434

context:
  include: [ ... ]          # glob patterns; sensible defaults if omitted
  exclude: [ ... ]          # combined with a mandatory set, see SECURITY.md

commands:
  <name>:
    program: cargo           # executable, never a shell string
    args: [build]            # argument array

permissions:
  <action-name>: allow|ask|deny   # e.g. project.read_file, command.run_configured

mcp:
  expose: [ ... ]            # native action names exported over MCP
  servers:
    <name>:
      transport: stdio
      command: some-mcp-server
      args: []
      enabled: true
```

Unknown top-level or nested fields are rejected at load time
(`deny_unknown_fields`), so a typo'd key fails loudly instead of being
silently ignored.

## Validation

`ProjectConfig::load` rejects, with a specific message:

- an unsupported `version`;
- an empty `project.name`, `project.type`, `model.provider`, or
  `model.model`;
- an empty command name or a command with an empty `program`;
- an invalid `permissions` value (only `allow`/`ask`/`deny` parse at all);
- an empty permission key;
- an empty `mcp.expose` entry, an empty MCP server name, or an MCP server
  with an empty `command`;
- an invalid glob pattern anywhere in `context.include`/`context.exclude`.

## Effective permissions

Every action declares its own conservative default permission. A
project's `permissions` map overrides that default by exact action name;
anything not listed keeps the action's default. Run `orbit project` (or
`orbit doctor`) to see the effective permission for every registered
action, not just the ones explicitly configured.

## Overrides

`--model <name>` and `--ollama-endpoint <url>` override the configured
provider settings for a single invocation without editing the file —
useful for trying a different local model without committing to it.
