Read `CLAUDE.md`, `README.md`, and the complete `docs/PROJECT_SPEC.md` before making any changes.

Your task is to build the first functional version of Orbit from the ground up.

Orbit is not a simple chatbot wrapper. It is a configurable AI engineering assistant that must understand the current project, inspect allowed files, search documentation and source code, use tools, enforce permissions, communicate with a language model provider, and return answers grounded in project sources.

Do not only create placeholders, empty modules, interfaces without implementations, or a large architecture that does not run. Produce a usable end-to-end version.

## Main goal

After your work, the following flow must work:

```bash
orbit init
orbit ask "What does this project do?"
```

`orbit init` must create a valid `.orbit/project.yaml`.

`orbit ask` must:

1. locate the project root;
2. load and validate `.orbit/project.yaml`;
3. discover allowed project files;
4. respect include and exclude rules;
5. search for relevant context;
6. send the selected context to a configured language model;
7. return a useful answer;
8. display the source files used;
9. never access excluded or unsafe paths.

Also implement:

```bash
orbit project
orbit files
orbit search "query"
orbit check
```

## Technical direction

Use Rust edition 2024.

Create a Rust workspace only where the separation is already useful. Prefer a small number of functional crates over many nearly empty crates.

A reasonable initial structure is:

```text
crates/
├── core
├── project
├── actions
├── provider
├── agent
└── cli
```

Do not create `voice`, desktop application, persistent memory, hardware control, or advanced multi-repository support in this first version.

MCP may be added only after the internal action system is working. Do not make the core depend on MCP.

## Required capabilities

### Project configuration

Implement `.orbit/project.yaml` with support for:

```yaml
version: 1

project:
  name: example
  type: software
  description: Example project

context:
  include:
    - README.md
    - docs/**
    - src/**
    - Cargo.toml

  exclude:
    - target/**
    - .git/**
    - .env
    - secrets/**
    - "**/*.key"
    - "**/*.pem"

commands:
  build:
    program: cargo
    args:
      - build

  test:
    program: cargo
    args:
      - test

  lint:
    program: cargo
    args:
      - clippy
      - --all-targets
      - --all-features
      - --
      - -D
      - warnings

  format:
    program: cargo
    args:
      - fmt
      - --check

permissions:
  read_files: allow
  search_files: allow
  run_configured_commands: allow
  run_arbitrary_commands: deny
  write_files: ask
  delete_files: deny
  create_commits: deny
  push_changes: deny

model:
  provider: anthropic
  model: configurable-through-environment
```

Validate unsupported versions, missing required fields, invalid paths, invalid permission values, duplicated commands, and malformed patterns.

Exclude rules must always take precedence over include rules.

### Project security

The project layer must:

* prevent path traversal;
* canonicalize and validate paths;
* stay inside the project root;
* reject symlink escapes;
* avoid secret files;
* never expose environment variables;
* treat repository content as untrusted data;
* avoid arbitrary shell string execution;
* run configured commands using program and argument arrays;
* enforce permissions in application code, not through model instructions.

### File discovery and search

Implement:

* recursive file discovery;
* include and exclude glob rules;
* supported text file filtering;
* file size limits;
* UTF-8 handling with useful errors;
* filename search;
* heading search for Markdown;
* content search;
* ranked results;
* source path preservation;
* line ranges where possible;
* context size limits.

Do not introduce a vector database yet.

Use deterministic local search for the first version.

### Action system

Implement structured internal actions.

At minimum:

```text
get_project_information
list_project_files
read_project_file
search_project_text
run_project_command
```

Each action must have:

* a stable name;
* a description;
* typed input;
* typed output;
* required permission;
* validation;
* useful errors;
* tests.

The CLI and agent must use these actions instead of duplicating their logic.

### Model provider

Create a provider-independent abstraction.

Implement Anthropic as the first provider.

Credentials must be loaded from:

```text
ANTHROPIC_API_KEY
```

Do not store credentials in project configuration or logs.

Allow the model name to be configured by an environment variable or project configuration, with a documented default.

Handle:

* authentication errors;
* network errors;
* rate limits;
* invalid responses;
* context limits;
* provider timeouts.

The provider-specific request and response types must not leak through the rest of the codebase.

### Agent

Implement an agent capable of:

1. receiving the user question;
2. retrieving relevant project context;
3. preparing a grounded model request;
4. exposing the available internal actions;
5. processing tool requests when supported;
6. enforcing action permissions;
7. returning action results to the model;
8. producing a final response;
9. preserving source references.

Limit tool-call iterations to prevent infinite loops.

The agent must clearly distinguish:

* trusted application instructions;
* user instructions;
* untrusted repository content;
* action results.

The model must never be able to grant itself permissions.

### CLI

Create the `orbit` binary.

Implement:

```bash
orbit init
orbit project
orbit files
orbit search <query>
orbit ask <question>
orbit check
```

Expected behavior:

```bash
orbit init
```

Creates `.orbit/project.yaml` without overwriting an existing file unless an explicit force flag is provided.

```bash
orbit project
```

Displays the loaded project metadata, root path, active provider, number of discovered files, and configured permissions.

```bash
orbit files
```

Lists allowed project files.

```bash
orbit search "watchdog"
```

Displays ranked search results with paths and matching line ranges.

```bash
orbit ask "Why was the ESP32-C3 selected?"
```

Answers using project context and prints a sources section.

```bash
orbit check
```

Runs all configured validation commands in a stable order and reports each result.

Support readable terminal output and a `--json` mode where appropriate.

Return meaningful exit codes.

## Source references

Use a structured source type similar to:

```rust
pub struct SourceReference {
    pub path: PathBuf,
    pub line_start: Option<usize>,
    pub line_end: Option<usize>,
    pub section: Option<String>,
}
```

Every search result must preserve its origin.

Answers grounded in repository files must display sources.

Do not claim that repository information was found when no source supports it.

## Error handling

Use typed errors and preserve useful context.

Errors must be understandable to the user.

Examples:

```text
Project configuration was not found.
Run `orbit init` to create `.orbit/project.yaml`.
```

```text
The requested path is outside the configured project root.
```

```text
The file is excluded by the project configuration.
```

```text
ANTHROPIC_API_KEY is not configured.
```

Avoid panics and production `unwrap` calls.

## Testing

Create comprehensive tests for:

* configuration parsing;
* configuration validation;
* project root discovery;
* include rules;
* exclude precedence;
* path traversal prevention;
* symlink escape prevention;
* secret file protection;
* file discovery;
* search ranking;
* source line preservation;
* permission enforcement;
* configured command execution;
* action validation;
* provider abstraction;
* agent iteration limits;
* CLI behavior.

Provider tests must not require a real API key.

Use mocks or a fake provider for agent tests.

## Documentation

Update the README with:

* what Orbit is;
* current capabilities;
* installation;
* configuration;
* required environment variables;
* CLI examples;
* security model;
* current limitations.

Add an example project configuration.

Document important architectural decisions inside `docs/architecture/`.

Do not rewrite `PROJECT_SPEC.md` unless implementation reveals a genuine conflict. When that happens, document the change clearly.

## Quality requirements

Before considering the work complete, run:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --workspace
```

Fix all warnings and failures.

Do not commit or push.

At the end, provide:

1. a summary of the implemented architecture;
2. the complete list of implemented commands;
3. the important security decisions;
4. the files created or changed;
5. the test results;
6. any incomplete functionality;
7. the exact commands I should run to test Orbit locally.

Make reasonable architectural decisions when details are missing.

Do not stop only to ask for minor preferences. Implement the strongest functional first version consistent with the specification.
