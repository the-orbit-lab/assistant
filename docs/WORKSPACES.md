# Workspaces

A workspace lets Orbit work across several sibling repositories — e.g. a
lab directory containing `assistant/`, `docs/`, `obc/`, `mission-tools/` —
without merging them into one filesystem root and without manually `cd`ing
between them for every question.

A workspace is an **orchestration layer**, not a new kind of project:

```text
Workspace Runtime (orbit-workspace)
    |
Project Registry (name/alias resolution, per-project availability)
    |
One or more selected Project Runtimes (each project's own ActionContext)
    |
Existing Action Runtime (orbit-actions, unchanged)
```

Every registered project keeps its own canonical root, its own
`.orbit/project.yaml` (include/exclude, commands, permissions, model,
MCP exposure), and its own security boundary, exactly as in single-project
mode. Workspace actions never duplicate native project logic — they
resolve *which* project(s) a request targets and then call the same
`ActionRegistry::execute` a single-project session would, against that
project's own `ActionContext`. Single-project mode (running `orbit` from
inside one repository, with no `.orbit/workspace.yaml` anywhere above it)
is completely unaffected: it is the same code path as before this feature
existed.

## Directory layout

```text
orbit-lab/
├── .orbit/
│   └── workspace.yaml
├── assistant/
│   └── .orbit/project.yaml
├── docs/
│   └── .orbit/project.yaml
├── mission-tools/
│   └── .orbit/project.yaml
└── obc/
    └── .orbit/project.yaml
```

`.orbit/workspace.yaml` lives at the lab root. Each sibling directory is a
normal, independent Orbit project with its own `.orbit/project.yaml` —
nothing about a project's own configuration changes when it's registered
in a workspace.

## Creating a workspace

```bash
cd orbit-lab
orbit workspace init
```

`orbit workspace init` registers every **immediate child directory** that
already contains `.orbit/project.yaml` (so an unrelated sibling folder is
never silently turned into a registered project) and writes a starter
`.orbit/workspace.yaml`. It never descends further than one level. Flags:
`--force` (overwrite an existing workspace config), `--name`, `--description`.

Edit the generated file to add aliases, descriptions, relationships, and a
default project — `orbit workspace init` deliberately leaves those blank
rather than guessing at them.

## `.orbit/workspace.yaml`

```yaml
version: 1

workspace:
  name: Orbit Lab
  description: Sibling repositories for the Orbit Lab project.

projects:
  assistant:
    path: ./assistant
    description: The Orbit assistant itself.
  docs:
    path: ./docs
    description: Cross-project documentation and ADRs.
  obc:
    path: ./obc
    aliases: [flight-computer]
    description: CubeSat onboard computer software.
  mission-tools:
    path: ./mission-tools
    description: Mission planning and analysis tooling.

# Directional, purely descriptive today -- shown by `orbit workspace` and
# workspace.information, never used to grant access between projects.
relationships:
  - source: obc
    target: docs
    type: documented-by

defaults:
  # Used for safe, read-only overview questions and single-project
  # commands when no project is explicitly selected. Commands that change
  # anything (e.g. `orbit run`) never use this -- see "Default project".
  project: assistant
```

See [`examples/workspace.yaml`](../examples/workspace.yaml) for a
fully-commented copy and [`schemas/workspace.schema.json`](../schemas/workspace.schema.json)
for the JSON Schema, kept in sync with `crates/workspace/src/config.rs`.

### Validation

`WorkspaceConfig::load` rejects, with a specific message, at parse time
(no filesystem access yet):

- an unsupported `version`;
- an empty `workspace.name`;
- an empty project name or an empty alias;
- two project names that collide once case/separator-normalized (`obc`
  vs `OBC`);
- an alias that collides with another project's name, or that's claimed
  by two different projects, or listed twice on the same project;
- `defaults.project` naming a project that isn't registered;
- a relationship referencing an unregistered `source`/`target`, an empty
  `type`, or a duplicate `(source, target, type)` triple;
- any unrecognized top-level or nested field (`deny_unknown_fields`).

`ProjectRegistry::load` then performs the filesystem-dependent checks,
resolving each `projects.<name>.path` against the workspace root with the
same `orbit_project::security::resolve_within_root` used for path safety
everywhere else in Orbit:

- a path that resolves **outside** the workspace root — via `..`
  traversal, an absolute path elsewhere, or a symlink — fails the whole
  workspace load (`WorkspaceProjectEscapesRoot`); this is a configuration
  error, not a per-project availability issue, because it's a security
  boundary;
- two different project names resolving to the same canonical directory:
  the second one is marked unavailable rather than silently sharing state
  with the first.

A project directory that doesn't exist, or exists but has no
`.orbit/project.yaml`, or has an `.orbit/project.yaml` that itself fails
to load, does **not** fail the workspace — that project is marked
`available: false` with a stored `error`, and every other project keeps
working. See [Unavailable projects](#unavailable-projects).

## Discovery precedence

Resolving "what am I operating on" follows a fixed order, checked in this
sequence — an explicit choice always wins over anything inferred from the
current directory:

1. **`--project <name-or-path>`** — a registered project name/alias
   (resolved against the active workspace, found the same way as below),
   or a filesystem path. A value starting with `./`, `../`, `/`, or a
   platform path prefix (e.g. `C:\`) is always treated as a path, never a
   name, so a project literally named like a path string is never
   ambiguous.
2. **`--workspace <dir>`** — use this workspace directly, no upward search.
3. **Current directory inside a registered project** — if the nearest
   `.orbit` marker walking upward from `cwd` is a `project.yaml`, Orbit
   behaves exactly as it does with no workspace involved at all, *even if*
   an enclosing directory has a `.orbit/workspace.yaml`. A project nested
   inside a workspace is never silently promoted to workspace mode.
4. **Current directory at or inside a workspace** (no nearer project
   marker) — resolves to workspace mode, using `defaults.project` for
   commands that accept a default (see below).
5. Otherwise: a clear "no `.orbit/project.yaml` or `.orbit/workspace.yaml`
   found" error.

This is why running `orbit` from inside `orbit-lab/obc/` behaves exactly
as single-project mode always has, while running it from `orbit-lab/`
itself (or from `orbit-lab/outside/`, a directory with no project of its
own) enters workspace mode.

## Project names and aliases

Resolution (`ProjectRegistry::resolve_project`) is tiered and
**deterministic — never fuzzy**:

1. exact registered name;
2. exact alias;
3. case/separator-normalized name (`OBC` / `obc`, `mission_tools` /
   `mission-tools`);
4. case/separator-normalized alias.

A selector that doesn't land at any tier is rejected with the list of
available project names — never guessed at. A model may *suggest* a
project name in conversation, but only this resolution logic — application
code, not the model — ever decides which project(s) a request actually
touches.

## CLI

| Command | Does |
|---|---|
| `orbit workspace` | Show workspace name, root, config path, default project, registered/available counts, unavailable projects, relationships. |
| `orbit workspace init` | Bootstrap `.orbit/workspace.yaml`, registering immediate children that already have `.orbit/project.yaml`. |
| `orbit projects` | List every registered project: name, aliases, path, description, availability. |
| `orbit --project <name>` | Run any single-project command (`project`, `files`, `search`, `ask`, `run`, `chat`, `mcp serve`, `doctor`, ...) against one registered project by name or alias. |
| `orbit search --projects a,b <query>` | Deterministic local search across named projects, no model involved. |
| `orbit ask --projects a,b "..."` | One agent turn scoped to named projects. |
| `orbit ask "..."` (no `--projects`) | Deterministic scan of the question text for project names/aliases; see [Natural-language routing](#natural-language-routing). |
| `orbit chat` | Interactive session; supports switching the active project(s) mid-conversation (see below). |
| `orbit doctor` | Workspace-aware diagnostics; see [Diagnostics](#diagnostics). |
| `orbit mcp serve --workspace <dir>` | Serve the six `workspace.*` actions over MCP stdio. |

```bash
orbit --project obc project
orbit --project docs search "STM32"
orbit search "STM32" --projects docs,obc
orbit ask "What does the OBC project do?"
orbit ask "In mission-tools, explain the RF link-budget implementation."
orbit ask --projects docs,obc "Compare the documented STM32 decision with the OBC implementation."
```

## Natural-language routing

Before the model's first turn, `orbit-workspace::retrieval` scans the
question for exact registered names/aliases (`find_project_mentions`,
matching up to three-word phrases, so multi-word aliases work too) and
resolves scope deterministically:

- **One project named** ("What does the OBC project do?") → that project
  only. If a search-worthy phrase remains after stripping the mention and
  a fixed stopword list, `workspace.search` runs against just that
  project; otherwise (a pure overview question) the project's own
  overview docs are read directly, mirroring single-project deterministic
  retrieval.
- **Two or more projects named** ("Compare the documented STM32 decision
  with the OBC implementation.") → `workspace.search` runs against exactly
  those projects with whatever query phrase remains.
- **A workspace-listing question** ("What projects are available?",
  "list projects") → workspace-level only: no project is searched: the
  model is expected to answer from `workspace.list_projects`/
  `workspace.information`, not from file contents.
- **Nothing named, not a listing question** → falls back to
  `defaults.project` if one is configured (shown in the response as
  "using default project: ..."), otherwise stays workspace-level with no
  files read.

This is deterministic substring/phrase matching, **not** semantic or
fuzzy project routing — a question has to actually contain a registered
name or alias to scope to that project.

`orbit search --projects` and `orbit ask --projects` bypass this scanning
entirely: naming projects explicitly always wins, and an unregistered name
fails immediately (`UnknownProject`) rather than silently dropping it from
the request.

### `orbit chat`

The active project set is explicit [session](SESSIONS.md) state, never
inferred from a single ambiguous turn:

```text
> /use obc
Active project(s): obc
> Why was STM32 selected?
• active project(s): obc
> Now compare that with docs
• active project(s): obc, docs
```

`/use <a[,b]>` **replaces** the active set, resolving each name through
the same deterministic registry. Naming a project in an ordinary question
**adds** it — so a follow-up like "compare that with docs" broadens scope
instead of losing the current project. Every change is announced with an
`active_projects_changed` event, so the active project(s) are always
visible and never change silently.

`/projects` lists what is registered, and `/status` shows the current
selection. The full command set is in [SESSIONS.md](SESSIONS.md#session-commands).

### Default project

`defaults.project` may be used for **safe, read-only** overview questions
and single-project commands issued at the workspace root with nothing
else selected — and the response always says so explicitly (`(using
default project: ...)`), never silently. It is never used for:

- **`orbit run`** at the workspace root with no `--project`: refused with
  the list of available projects, not defaulted. Running a configured
  command against a project the caller didn't select would be too easy to
  get wrong.
- **`orbit run` under `resolve_project_with_mode(global, strict=true)`**:
  the default-project fallback is disabled entirely for this command,
  independent of whether `defaults.project` is even configured.
- Multi-project requests (`--projects`) or requests where the question
  text names specific projects: those always take precedence over the
  default.

## Multi-project sources

Search and read results are attributed with `WorkspaceSourceReference
{ project, path, line_start, line_end, section }`. The plain-text and
CLI-printed form is `<project>:<path>[:<line_start>[-<line_end>]] [(section)]`,
e.g.:

```text
docs:obc/adr/OBC-ADR-0004.md:18-41
```

Deduplication (`dedupe_workspace_sources`) always includes project
identity in its key: `docs:README.md` and `obc:README.md` are never
merged just because the path string matches. As with single-project
sources, a project-scoped path-only reference is dropped in favor of a
more precise line-ranged reference to the *same* `(project, path)` pair
when both exist, first-seen order is otherwise preserved, and a source can
only ever come from a deterministic-retrieval read or an actually-executed
action's return value — the model's answer text can never introduce a
source that wasn't returned by something that really ran.

(Structured consumers — JSON output, MCP tool content, a future event
stream — see the typed `WorkspaceSourceReference` directly; the
`project:path` encoding is specifically for flowing through the existing,
project-agnostic `orbit_core::SourceReference` pipeline — Agent
aggregation, dedup, CLI printing — with zero changes to `orbit-core`.)

## Context budgeting

Bounded independently per project rather than dynamically redistributed,
so behavior doesn't change based on how many *other* projects happen to
be selected (`crates/workspace/src/budget.rs`):

| Limit | Value | Applies to |
|---|---|---|
| `MAX_PROJECTS_PER_REQUEST` | 8 | Projects touched by one `workspace.search` call. |
| `MAX_RESULTS_PER_PROJECT` | 5 | Search results per project. |
| `MAX_EXCERPT_BYTES_PER_RESULT` | 400 | Bytes per search-result excerpt (UTF-8 boundary safe). |
| `MAX_READ_BYTES_PER_PROJECT` | 8,000 | A single `workspace.read_file` call. |
| `MAX_TOTAL_CONTEXT_BYTES` | 24,000 | Backstop across a whole `workspace.search` response. |

Ranking (filename/heading matches first, from `orbit_project::search`)
means whatever gets truncated to fit `MAX_TOTAL_CONTEXT_BYTES` is the
lowest-relevance content, and every kept result keeps its full project,
path, and line-range metadata even if its excerpt text was shortened.

## Workspace-native actions

Six actions, always thin orchestrators over existing per-project actions
— none of them duplicate native project logic:

| Action | Input | Delegates to |
|---|---|---|
| `workspace.information` | *(none)* | — reads the registry directly |
| `workspace.list_projects` | *(none)* | — reads the registry directly |
| `workspace.project_information` | `{ project }` | that project's `project.information` |
| `workspace.search` | `{ projects: [...], query, limit_per_project? }` | that project's `project.search`, once per named project |
| `workspace.read_file` | `{ project, path, max_bytes? }` | that project's `project.read_file` |
| `workspace.list_project_files` | `{ project }` | that project's `project.list_files` |

`workspace.search` and every project-scoped action reject an unregistered
project name outright (`UnknownProject`) rather than silently skipping it
or searching a subset — a multi-project request either resolves every
named project or fails clearly. `workspace.search`'s `projects` array must
be non-empty: it never implicitly means "every project."

Every one of these builds a fresh, per-project `ActionContext` from that
project's *own* loaded `.orbit/project.yaml` before delegating — the exact
same `ActionContext` a single-project session would use — so permission
resolution, path security, and include/exclude rules are that project's
own, unmodified.

## Permission isolation

**One project's permissions never authorize another.** Each
`workspace.*` action resolves the target project's own effective
permission via that project's own `ActionContext`/`permissions` map —
never the workspace's, and never another project's. For example:

```yaml
# docs/.orbit/project.yaml
permissions:
  project.search: allow

# obc/.orbit/project.yaml
permissions:
  project.search: allow
  command.run_configured: ask
```

A `project.search: allow` in `docs` says nothing about `obc`; `obc`'s own
`command.run_configured: ask` still requires confirmation there,
regardless of what's configured in `docs` or in `workspace.yaml` itself.
Workspace mode does not expose command execution at all today (see
[Workspace-native actions](#workspace-native-actions) — there is no
`workspace.run_configured`), so this isolation is enforced structurally
for the actions that do exist, and single-project `orbit --project obc
run <name>` always shows the resolved identity before applying the normal
permission flow:

```text
Project: obc
Command: build
Program: cargo
Arguments: [build]
```

`workspace.*` actions themselves default to `allow` (they're read-only
orchestration; the real access control happens per-project underneath),
and a workspace's own `permissions` map (distinct from any project's) can
tighten that per action name if desired.

Also enforced, all backed by existing primitives rather than new
workspace-specific security code:

- A project path in `workspace.yaml` cannot escape the workspace root
  (`resolve_within_root`, including symlink escapes) — see
  [Validation](#validation).
- `workspace.read_file`/`workspace.list_project_files`/
  `workspace.project_information` all resolve their target project first,
  then delegate to *that* project's own `project.read_file` — a path like
  `../obc/secret.rs` passed while `project: docs` is rejected by `docs`'s
  own path security, never resolved against `obc`.
- An unregistered or ambiguous project name is always a hard error, never
  guessed at by prefix-matching or fuzzy comparison.
- `orbit run` at the workspace root always requires an explicit
  `--project`; it is never executed against every project or against a
  default project.

## MCP: workspace mode

```bash
orbit --workspace /path/to/orbit-lab mcp serve
```

exposes **exactly the six `workspace.*` actions** listed above — never one
dynamically generated tool per registered repository, and never a
project's native command-execution actions. This reuses
`OrbitMcpServer`/`compute_exposure`/`self_check` unchanged: workspace mode
needed zero changes to `orbit-mcp-server` itself, because that crate's
exposure and permission logic already only depends on a generic
`ActionRegistry` + `ActionContext`, not on anything project-specific.

Tool call results (e.g. `workspace.search`) carry the same
project-scoped, structured `sources` payload described above, so an MCP
host can display or reason about which project each result came from.

### Claude Code: single-project mode (unchanged)

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

### Claude Code: workspace mode

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

Both use the exact same `mcpServers` config shape and the same binary —
only the flag changes. See [MCP.md](MCP.md) for the general MCP behavior
(permission resolution for `allow`/`ask`/`deny`, verifying the connection)
that applies identically in both modes.

## Diagnostics: `orbit doctor`

In workspace mode, `orbit doctor` reports:

- `workspace configuration` — `OK` with the resolved config path, or
  `FAIL` with the specific parse/validation error.
- `workspace root`, `default project`, `relationships` — informational.
- One line **per registered project**, in the exact format used
  throughout this feature:

  ```text
  [OK] workspace configuration
  [OK] project assistant
  [OK] project docs
  [WARN] project obc: model `qwen3:8b` is unavailable
  [FAIL] project mission-tools: `.orbit/project.yaml` was not found
  ```

  `OK` includes the discovered-file and configured-permission counts for
  that project (via its own `project.information`); `FAIL` is the stored
  registry error for an unavailable project (missing directory, missing
  or invalid `.orbit/project.yaml`); a separate `WARN` line follows an
  otherwise-`OK` project if *that project's own* configured Ollama model
  isn't pulled — each project can configure a different model, so this is
  checked per project, not once globally.
- `workspace action registry` / `workspace mcp exposure` — constructs the
  real workspace `ActionRegistry` and reports the six exposed action
  names.
- `workspace mcp self-check` — builds a real `OrbitMcpServer` for the
  workspace and runs an `initialize` + `tools/list` round trip against it
  over an in-process transport, reusing `orbit_mcp_server::self_check`
  exactly as single-project mode does.
- `ollama connectivity` / `ollama model` — checked against the workspace
  invocation's own model/endpoint (CLI overrides or defaults), separate
  from each project's own per-project model check above.

One `FAIL`ed project does not abort the whole report — every other check
still runs, so `orbit doctor` at a partially-broken workspace root still
tells you about the projects that *do* work.

## Known limitations

- Project routing (both natural-language scanning and name/alias
  resolution) is deterministic text matching, not semantic or fuzzy
  matching — a question must contain an actual registered name or alias.
- `orbit-project::search` is substring-based, not tokenized: a two-word
  extracted query like "STM32 selection" only matches text containing
  that literal phrase, not documents containing both words separately.
  This is a pre-existing single-project limitation that also applies to
  workspace-scoped search.
- Relationships (`relationships:` in `workspace.yaml`) are descriptive
  only — shown by `orbit workspace` and `workspace.information` — and are
  never used to grant or infer access between projects.
- `workspace.*` MCP tool call results do not currently carry per-project
  Ollama-model metadata; that's only surfaced by `orbit doctor`.
- No workspace-level command execution action exists; running a
  configured command always requires selecting one project explicitly
  (`orbit --project <name> run <command>`).
- A workspace-scoped action can touch several projects in one call, so
  its `action_*` [events](EVENTS.md) carry no single `project` field.
  Per-project identity for that work arrives on the `source_found`
  events instead.
