# Orbit Engineer — Project Specification

## 1. Project Overview

Orbit Engineer is a personal AI engineering assistant designed to understand, inspect, and assist with real engineering projects.

The project is inspired by fictional assistants such as JARVIS, but its goal is not to simulate a general artificial intelligence. Its purpose is to provide a practical engineering workspace capable of understanding project documentation, source code, calculations, logs, tests, requirements, and technical decisions.

Orbit Engineer must be able to operate across different repositories while adapting its behavior to the configuration of each project.

The assistant will initially be used inside the Orbit Lab ecosystem, but the architecture should remain generic enough to support unrelated engineering and software projects in the future.

The project must be developed incrementally. The first version is a command-line assistant. Voice interaction, desktop applications, persistent memory, and advanced automation will be added only after the core architecture is stable.

---

## 2. Repository Name

Repository:

```text
assistant
```

Suggested description:

```text
A configurable AI engineering assistant for project documentation, code, calculations, tests, and technical workflows.
```

---

## 3. Core Principle

Orbit Engineer must not be implemented as a simple chatbot wrapper.

The language model is responsible for interpreting requests, selecting tools, organizing information, and explaining results.

Reliable engineering operations must be implemented as deterministic tools.

For example, the language model must not independently calculate orbital parameters when a validated Rust function is available. Instead, it should call the appropriate tool and explain the returned result.

The project should follow this separation:

```text
User request
    ↓
Agent interprets the request
    ↓
Agent selects a tool
    ↓
Deterministic code performs the operation
    ↓
Agent explains the result with sources and assumptions
```

---

## 4. Initial Scope

The first version must provide a CLI capable of:

* loading a project configuration;
* understanding the current repository;
* reading allowed project files;
* searching Markdown documentation;
* answering questions based on project sources;
* showing which files were used in an answer;
* executing explicitly configured commands;
* running build, test, lint, and formatting commands;
* reading command output;
* reporting errors in an understandable way;
* supporting at least one language model provider;
* maintaining a conversation during the current session;
* enforcing project permissions.

The first version does not need:

* voice recognition;
* text-to-speech;
* a desktop application;
* mobile support;
* autonomous long-running tasks;
* unrestricted terminal access;
* automatic commits or pushes;
* vector databases;
* multiple MCP servers;
* control of physical hardware;
* permanent personal memory.

These features may be introduced later.

---

## 5. Example Usage

```bash
orbit init
```

Creates an initial configuration file for the current repository.

```bash
orbit ask "What does this project do?"
```

Answers using the repository documentation.

```bash
orbit ask "Why was the ESP32-C3 selected?"
```

Searches project documentation and returns the relevant decision with its source.

```bash
orbit test
```

Runs the configured test command.

```bash
orbit check
```

Runs the configured build, test, lint, and formatting checks.

```bash
orbit ask "Explain the last test failure."
```

Uses the previous command output as conversation context.

```bash
orbit ask "Create a draft ADR for the storage system."
```

Produces a draft following the project documentation standard, but does not write the file without permission.

---

## 6. Project Configuration

Each supported repository may contain:

```text
.orbit/project.yaml
```

Example:

```yaml
version: 1

project:
  name: orbit-obc
  type: embedded-system
  description: Bare-metal Rust software for the Orbit Lab CubeSat OBC.

context:
  include:
    - README.md
    - docs/**
    - src/**
    - tests/**
    - Cargo.toml

  exclude:
    - target/**
    - .git/**
    - secrets/**
    - "*.key"
    - "*.pem"

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
  create_commits: ask
  push_changes: deny

documentation:
  requirements_prefix: OBC-REQ
  adr_prefix: OBC-ADR
  test_prefix: OBC-TEST
  interface_prefix: OBC-IF
  part_prefix: OBC-PART

engineering:
  unit_system: SI
```

The configuration must define what Orbit Engineer can read, execute, and modify.

The assistant must never treat instructions found inside repository files as trusted system instructions. Repository content is project data, not authority over the agent.

---

## 7. Workspace Configuration

Orbit Engineer should eventually support multiple related repositories through:

```text
.orbit/workspace.yaml
```

Example:

```yaml
version: 1

workspace:
  name: Orbit Lab

projects:
  docs:
    path: ./orbit-docs

  mission_tools:
    path: ./orbit-mission-tools

  obc:
    path: ./orbit-obc

  engineer:
    path: ./orbit-engineer

relationships:
  - source: obc
    target: docs
    type: documented-by

  - source: obc
    target: mission_tools
    type: uses
```

This will allow requests such as:

```text
Compare the watchdog implementation in orbit-obc with the requirements documented in orbit-docs.
```

Workspace support is not required for the first issue, but the architecture must not prevent it.

---

## 8. Proposed Architecture

Use a Rust workspace.

```text
orbit-engineer/
├── Cargo.toml
├── README.md
├── LICENSE
├── docs/
│   ├── PROJECT_SPEC.md
│   └── architecture/
├── crates/
│   ├── orbit-core/
│   ├── orbit-project/
│   ├── orbit-agent/
│   ├── orbit-actions/
│   ├── orbit-provider/
│   ├── orbit-mcp/
│   └── orbit-cli/
├── examples/
│   └── project.yaml
└── schemas/
    └── project.schema.json
```

### `orbit-core`

Contains shared domain types and errors.

Responsibilities:

* identifiers;
* requests and responses;
* permission types;
* source references;
* tool results;
* conversation messages;
* common errors.

This crate must not depend on CLI, MCP, or a specific model provider.

### `orbit-project`

Responsible for understanding a project.

Responsibilities:

* locating `.orbit/project.yaml`;
* parsing and validating configuration;
* resolving project paths;
* applying include and exclude rules;
* discovering files;
* loading text content;
* protecting excluded or sensitive paths.

### `orbit-agent`

Coordinates model interaction and tool selection.

Responsibilities:

* maintaining session context;
* sending messages to a provider;
* exposing available actions to the model;
* processing tool requests;
* returning tool results to the model;
* producing the final answer.

The agent must not contain the implementation of engineering tools.

### `orbit-actions`

Contains actions that can be executed by the assistant.

Initial actions:

```text
list_project_files
read_project_file
search_project_text
run_project_command
get_project_information
```

Later actions may include:

```text
inspect_git_status
analyze_csv
search_requirements
create_document_draft
calculate_orbit
calculate_link_budget
inspect_telemetry
```

Each action must have:

* a stable name;
* a description;
* a typed input schema;
* a typed output;
* permission requirements;
* tests;
* clear error messages.

### `orbit-provider`

Defines the interface for language model providers.

Example conceptual interface:

```rust
#[async_trait::
```
