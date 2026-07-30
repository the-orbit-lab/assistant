//! CLI-level tests for multi-repository workspace support. These spawn the
//! real `orbit` binary (via `assert_cmd`) against a temp directory tree
//! shaped like the target Orbit Lab layout: a workspace root with several
//! sibling project directories, one of which (`mission-tools`) is
//! deliberately left without `.orbit/project.yaml` so unavailable-project
//! handling is exercised end to end, not just at the crate level.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;

fn orbit() -> Command {
    Command::cargo_bin("orbit").unwrap()
}

fn write_project(root: &Path, name: &str, extra_files: &[(&str, &str)]) {
    fs::create_dir_all(root.join(".orbit")).unwrap();
    fs::write(
        root.join(".orbit/project.yaml"),
        format!(
            "version: 1\nproject:\n  name: {name}\ncontext:\n  include:\n    - \"**/*\"\n\
             permissions:\n  project.information: allow\n  project.list_files: allow\n  \
             project.read_file: allow\n  project.search: allow\n"
        ),
    )
    .unwrap();
    fs::write(root.join("README.md"), format!("# {name}\n")).unwrap();
    for (path, content) in extra_files {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, content).unwrap();
    }
}

/// Mirrors the target Orbit Lab layout: `workspace/{assistant,docs,obc,
/// mission-tools,outside}`. `mission-tools` is intentionally left without
/// `.orbit/project.yaml` -- registered in `workspace.yaml` but unavailable,
/// matching the doctor `[FAIL] project mission-tools: ...` example from the
/// spec. `outside` is a sibling directory that is never registered and
/// used to exercise workspace-root escape rejection.
fn scaffold_workspace() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    write_project(&root.join("assistant"), "assistant", &[]);
    write_project(
        &root.join("docs"),
        "docs",
        &[(
            "obc/architecture.md",
            "# OBC Architecture\n\nSTM32 selection rationale: low power draw.\n",
        )],
    );
    write_project(
        &root.join("obc"),
        "obc",
        &[(
            "src/watchdog.rs",
            "// resets the system on timeout\nfn watchdog() {}\n",
        )],
    );
    fs::create_dir_all(root.join("mission-tools")).unwrap();
    fs::write(root.join("mission-tools/README.md"), "# mission-tools\n").unwrap();
    fs::create_dir_all(root.join("outside")).unwrap();
    fs::write(root.join("outside/secret.md"), "outside the workspace\n").unwrap();

    fs::create_dir_all(root.join(".orbit")).unwrap();
    fs::write(
        root.join(".orbit/workspace.yaml"),
        "version: 1\n\
         workspace:\n  name: Orbit Lab\n  description: Test workspace fixture\n\
         projects:\n\
         \x20\x20assistant:\n    path: ./assistant\n    description: The assistant itself\n\
         \x20\x20docs:\n    path: ./docs\n    description: Documentation\n\
         \x20\x20obc:\n    path: ./obc\n    aliases: [flight-computer]\n    description: Onboard computer\n\
         \x20\x20mission-tools:\n    path: ./mission-tools\n    description: Mission tooling\n\
         relationships:\n\
         \x20\x20- source: obc\n    target: docs\n    type: documented-by\n\
         defaults:\n  project: assistant\n",
    )
    .unwrap();

    tmp
}

#[test]
fn workspace_init_registers_only_directories_with_project_config() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    write_project(&root.join("assistant"), "assistant", &[]);
    write_project(&root.join("obc"), "obc", &[]);
    fs::create_dir_all(root.join("not-a-project")).unwrap();

    orbit()
        .args(["--workspace", root.to_str().unwrap(), "workspace", "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Registered projects: assistant, obc",
        ))
        .stdout(predicate::str::contains("Skipped").and(predicate::str::contains("not-a-project")));

    assert!(root.join(".orbit/workspace.yaml").exists());

    orbit()
        .args(["--workspace", root.to_str().unwrap(), "workspace", "init"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));

    orbit()
        .args([
            "--workspace",
            root.to_str().unwrap(),
            "workspace",
            "init",
            "--force",
        ])
        .assert()
        .success();
}

#[test]
fn workspace_info_reports_projects_and_default() {
    let tmp = scaffold_workspace();
    orbit()
        .args(["--workspace", tmp.path().to_str().unwrap(), "workspace"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Orbit Lab"))
        .stdout(predicate::str::contains("Default:     assistant"))
        .stdout(predicate::str::contains("mission-tools"));
}

#[test]
fn projects_command_lists_availability_for_every_registered_project() {
    let tmp = scaffold_workspace();
    orbit()
        .args(["--workspace", tmp.path().to_str().unwrap(), "projects"])
        .assert()
        .success()
        .stdout(predicate::str::contains("assistant [available]"))
        .stdout(predicate::str::contains("obc [available]"))
        .stdout(predicate::str::contains("flight-computer"))
        .stdout(predicate::str::contains("mission-tools [unavailable]"));
}

#[test]
fn explicit_project_selector_resolves_registered_name_not_just_a_path() {
    let tmp = scaffold_workspace();
    orbit()
        .args([
            "--workspace",
            tmp.path().to_str().unwrap(),
            "--project",
            "obc",
            "project",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("obc"));

    // The alias resolves too.
    orbit()
        .args([
            "--workspace",
            tmp.path().to_str().unwrap(),
            "--project",
            "flight-computer",
            "project",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("obc"));
}

#[test]
fn explicit_project_selector_from_inside_the_workspace_root_needs_no_workspace_flag() {
    let tmp = scaffold_workspace();
    orbit()
        .current_dir(tmp.path())
        .args(["--project", "docs", "files"])
        .assert()
        .success()
        .stdout(predicate::str::contains("obc/architecture.md"));
}

#[test]
fn unknown_project_selector_fails_clearly() {
    let tmp = scaffold_workspace();
    orbit()
        .args([
            "--workspace",
            tmp.path().to_str().unwrap(),
            "--project",
            "nope",
            "project",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nope"));
}

#[test]
fn unavailable_project_selector_fails_clearly_rather_than_silently_degrading() {
    let tmp = scaffold_workspace();
    orbit()
        .args([
            "--workspace",
            tmp.path().to_str().unwrap(),
            "--project",
            "mission-tools",
            "project",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("mission-tools"));
}

#[test]
fn multi_project_search_prefixes_results_with_project_identity() {
    let tmp = scaffold_workspace();
    orbit()
        .args([
            "--workspace",
            tmp.path().to_str().unwrap(),
            "search",
            "--projects",
            "docs,obc",
            "STM32",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Project: docs"))
        .stdout(predicate::str::contains("obc/architecture.md"));
}

#[test]
fn multi_project_search_rejects_an_unregistered_project_name() {
    let tmp = scaffold_workspace();
    orbit()
        .args([
            "--workspace",
            tmp.path().to_str().unwrap(),
            "search",
            "--projects",
            "docs,nonexistent",
            "STM32",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("nonexistent"));
}

#[test]
fn search_never_crosses_into_a_sibling_directory_outside_the_workspace() {
    let tmp = scaffold_workspace();
    orbit()
        .args([
            "--workspace",
            tmp.path().to_str().unwrap(),
            "search",
            "--projects",
            "docs",
            "outside the workspace",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
}

#[test]
fn run_at_workspace_root_without_an_explicit_project_is_refused_not_defaulted() {
    let tmp = scaffold_workspace();
    orbit()
        .current_dir(tmp.path())
        .args(["--yes", "run", "build"])
        .assert()
        .failure();
}

#[test]
fn doctor_reports_workspace_and_per_project_status_in_the_specified_format() {
    let tmp = scaffold_workspace();
    orbit()
        .args([
            "--workspace",
            tmp.path().to_str().unwrap(),
            "--ollama-endpoint",
            "http://127.0.0.1:1",
            "doctor",
        ])
        .assert()
        .stdout(predicate::str::contains("[OK] workspace configuration"))
        .stdout(predicate::str::contains("[OK] project assistant"))
        .stdout(predicate::str::contains("[OK] project docs"))
        .stdout(predicate::str::contains("[OK] project obc"))
        .stdout(predicate::str::contains(
            "[FAIL] project mission-tools: `.orbit/project.yaml` was not found",
        ));
}

#[test]
fn doctor_json_output_is_valid_json_in_workspace_mode() {
    let tmp = scaffold_workspace();
    let output = orbit()
        .args([
            "--workspace",
            tmp.path().to_str().unwrap(),
            "--json",
            "--ollama-endpoint",
            "http://127.0.0.1:1",
            "doctor",
        ])
        .assert()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let checks = parsed["checks"].as_array().unwrap();
    assert!(
        checks
            .iter()
            .any(|c| c["check"] == "workspace configuration")
    );
    assert!(
        checks
            .iter()
            .any(|c| c["check"] == "project mission-tools" && c["status"] == "FAIL")
    );
}

#[test]
fn chat_reports_workspace_mode_and_exits_cleanly() {
    let tmp = scaffold_workspace();
    orbit()
        .args(["--workspace", tmp.path().to_str().unwrap(), "chat"])
        .write_stdin("exit\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("workspace"));
}

#[test]
fn single_project_mode_from_inside_a_plain_project_is_unaffected_by_workspace_support() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("README.md"), "# Demo\n").unwrap();

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success();

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "files"])
        .assert()
        .success()
        .stdout(predicate::str::contains("README.md"));

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "doctor"])
        .assert()
        .stdout(predicate::str::contains("project configuration"))
        .stdout(predicate::str::contains("workspace configuration").not());
}
