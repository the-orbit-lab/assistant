//! Shared fixtures: a realistic temp workspace shaped like Orbit Lab, and
//! a temp single project. Nothing here needs Ollama.
//!
//! Each integration test file includes this module separately, so any one
//! file uses only part of it.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;

use orbit_actions::ActionContext;
use orbit_core::{FinishReason, Message, ModelResponse, ToolCall};
use orbit_workspace::{ProjectRegistry, WorkspaceConfig};

pub fn write_project(root: &Path, name: &str, extra: &[(&str, &str)]) {
    std::fs::create_dir_all(root.join(".orbit")).unwrap();
    std::fs::write(
        root.join(".orbit/project.yaml"),
        format!(
            "version: 1\nproject:\n  name: {name}\ncontext:\n  include:\n    - \"**/*\"\n\
             commands:\n  test:\n    program: echo\n    args: [ran-{name}-tests]\n\
             permissions:\n  project.information: allow\n  project.list_files: allow\n  \
             project.read_file: allow\n  project.search: allow\n  command.run_configured: ask\n"
        ),
    )
    .unwrap();
    std::fs::write(root.join("README.md"), format!("# {name}\n")).unwrap();
    for (path, content) in extra {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
    }
}

/// `workspace/{docs,obc}` with STM32 and watchdog/brownout content, plus a
/// registered-but-unavailable `mission-tools`.
pub fn workspace_fixture() -> (tempfile::TempDir, Arc<ProjectRegistry>) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    write_project(
        &root.join("docs"),
        "docs",
        &[(
            "obc/ADR-0004.md",
            "# ADR-0004\n\nSTM32 selection rationale: low power draw.\nBrownout recovery is \
             documented here.\n",
        )],
    );
    write_project(
        &root.join("obc"),
        "obc",
        &[(
            "src/watchdog.rs",
            "// watchdog resets the system after a brownout\nfn feed() {}\n",
        )],
    );
    std::fs::create_dir_all(root.join("mission-tools")).unwrap();

    std::fs::create_dir_all(root.join(".orbit")).unwrap();
    let yaml = "version: 1\n\
        workspace:\n  name: Orbit Lab\n  description: test workspace\n\
        projects:\n\
        \x20\x20docs:\n    path: ./docs\n\
        \x20\x20obc:\n    path: ./obc\n    aliases: [flight-computer]\n\
        \x20\x20mission-tools:\n    path: ./mission-tools\n\
        defaults:\n  project: docs\n";
    let config_path = root.join(".orbit/workspace.yaml");
    std::fs::write(&config_path, yaml).unwrap();

    let config = WorkspaceConfig::parse(yaml).unwrap();
    let registry = ProjectRegistry::load(root, config_path, config).unwrap();
    (tmp, Arc::new(registry))
}

pub fn single_project_fixture() -> (tempfile::TempDir, ActionContext) {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_project(
        &root,
        "obc",
        &[(
            "src/watchdog.rs",
            "// watchdog resets the system after a brownout\n",
        )],
    );
    let config_path = root.join(".orbit/project.yaml");
    let config = orbit_project::ProjectConfig::load(&config_path).unwrap();
    (
        tmp,
        ActionContext {
            root,
            config_path,
            config,
        },
    )
}

pub fn answer(text: &str) -> ModelResponse {
    ModelResponse {
        message: Message::assistant(text),
        finish_reason: FinishReason::Stop,
    }
}

pub fn tool_call(name: &str, arguments: serde_json::Value) -> ModelResponse {
    ModelResponse {
        message: Message::assistant_tool_calls(vec![ToolCall {
            id: "call_0".to_string(),
            name: name.to_string(),
            arguments,
        }]),
        finish_reason: FinishReason::ToolCalls,
    }
}
