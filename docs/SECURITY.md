# Security model

Security is enforced in application code, not by asking the model nicely.
Repository content — file contents, search excerpts, command output — is
always treated as untrusted data, never as instructions. This is stated
once in the agent's system prompt (see `crates/agent/src/prompt.rs`) and
backed by enforcement that does not depend on the model obeying it.

## Path handling (`crates/project/src/security.rs`)

Every filesystem-facing action goes through `resolve_within_root`:

- Relative paths are joined to the project root; `..` components are
  resolved manually, and if resolution would pop past the root, the
  request is rejected (`PathOutsideProject`).
- Absolute paths outside the root are rejected outright.
- If the resolved path exists, it is canonicalized and the canonical path
  must still start with the canonical root — this catches a symlink
  inside the project that points outside it (`SymlinkEscape`).

File discovery (`walkdir`) never follows symlinks
(`follow_links(false)`), so a symlinked directory can't be used to widen
the walk beyond the root either.

## Include/exclude precedence

Exclude always wins. A mandatory, non-overridable exclude set is applied
in addition to whatever a project configures:

```text
.git/**  .orbit/**  target/**  node_modules/**
.env  .env.*  secrets/**  **/*.key  **/*.pem
```

A project's own `context.exclude` is added to this set, never replaces it.
`orbit-project::discovery` prunes excluded directories during the walk
(it never descends into `.git` or `target` in the first place), and the
same include/exclude check runs again on every single-file read
(`project.read_file`), so a file can't become reachable just because it
was missed by one code path.

## Resource bounds

- File discovery stops after `MAX_DISCOVERED_FILES` (20,000) entries.
- A file is only treated as searchable text if it is under 2 MiB and has a
  recognized text extension; other files are still listed by
  `project.list_files` but excluded from search/read content.
- `project.read_file` enforces a caller-supplied `max_bytes`, itself
  capped at a hard 5 MiB ceiling regardless of what's requested.
- `command.run_configured` runs with a 300s timeout and captures at most
  64 KiB each of stdout/stderr, so a runaway or chatty command can't hang
  the session or blow up the model's context.

## Commands: no shell, no arbitrary execution

Configured commands run as `Command::new(program).args(args)` — an
executable plus an argument array, spawned without a shell. There is no
code path that builds a shell command string, so there is no command
injection surface via argument content.

`command.run_configured` accepts exactly one input: the *name* of an
already-configured command. Any other fields in the model's tool-call
arguments (e.g. an injected `"program"` or `"args"`) are ignored — the
action always looks up the program and arguments from
`.orbit/project.yaml`, never from the caller. There is no native action
that runs an arbitrary program chosen by the model.

## Permissions

Three values: `allow` (run without confirmation), `ask` (require explicit
confirmation), `deny` (never run). Every action declares a conservative
default; a project's `permissions` map overrides it by exact action name.
The model never sees or can change this map — `ActionRegistry::execute`
resolves the effective permission from configuration before an action's
`execute` method is ever called, and rejects the request in application
code if it's `deny` or an unconfirmed `ask`.

External MCP tools (`mcp.<server>.<tool>`) default to `ask`, stricter than
native actions, because they run code Orbit did not write.

In non-interactive contexts (no TTY, or Orbit's own `mcp serve`), `ask`
actions are denied by default unless the CLI's `--yes` flag was passed
explicitly. Nothing ever prompts silently or approves itself.

## MCP server exposure

`orbit mcp serve` only exposes actions listed in `mcp.expose`. An
unlisted action is invisible: it does not appear in `tools/list`, and
calling it by name returns an error rather than running it. The MCP
server reuses the exact same `ActionRegistry` and permission enforcement
as the CLI and agent — there is no separate, weaker code path for MCP.

## Conversation memory

`orbit chat` keeps history in process memory for the life of the process
only. Nothing about a conversation is written to disk. Closing the
session discards it.

## Known limitations

- YAML mapping keys that are literally duplicated in `.orbit/project.yaml`
  (e.g. two `build:` entries under `commands`) are not detected as an
  error; the YAML parser keeps the last one, matching standard YAML
  behavior. This is a parser-level limitation, not a bypass of any
  permission or path check.
- Command execution inherits the parent process's environment (needed to
  find `cargo`, `git`, etc. on `PATH`). Orbit never injects, forwards, or
  logs environment variable *values*; it also never lets repository
  content or model output set environment variables for a command.
