use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;

fn orbit() -> Command {
    Command::cargo_bin("orbit").unwrap()
}

fn scaffold_project() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("README.md"), "# Demo\nA demo project.\n").unwrap();
    fs::create_dir_all(tmp.path().join("docs")).unwrap();
    fs::write(
        tmp.path().join("docs/watchdog.md"),
        "# Watchdog\nresets the system\n",
    )
    .unwrap();
    tmp
}

#[test]
fn init_creates_config_and_refuses_to_overwrite() {
    let tmp = tempfile::tempdir().unwrap();

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success()
        .stdout(predicate::str::contains("Created"));

    assert!(tmp.path().join(".orbit/project.yaml").exists());

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("already exists"));

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init", "--force"])
        .assert()
        .success();
}

#[test]
fn project_command_fails_clearly_without_config() {
    let tmp = tempfile::tempdir().unwrap();
    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "project"])
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("orbit init"));
}

#[test]
fn files_and_search_reflect_project_content() {
    let tmp = scaffold_project();
    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success();

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "files"])
        .assert()
        .success()
        .stdout(predicate::str::contains("README.md"))
        .stdout(predicate::str::contains("docs/watchdog.md"));

    orbit()
        .args([
            "--project",
            tmp.path().to_str().unwrap(),
            "search",
            "watchdog",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("docs/watchdog.md"));
}

#[test]
fn json_output_is_valid_json() {
    let tmp = scaffold_project();
    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success();

    let output = orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "--json", "files"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(parsed["files"].is_array());
}

#[test]
fn run_rejects_unconfigured_command_name() {
    let tmp = scaffold_project();
    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success();

    orbit()
        .args([
            "--project",
            tmp.path().to_str().unwrap(),
            "--yes",
            "run",
            "does-not-exist",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not configured"));
}

#[test]
fn run_without_yes_in_non_interactive_mode_is_denied() {
    let tmp = scaffold_project();
    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success();

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "run", "build"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not confirmed"));
}

#[test]
fn search_never_reads_excluded_secret_files() {
    let tmp = scaffold_project();
    fs::write(tmp.path().join(".env"), "SECRET=super-secret-value").unwrap();

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success();

    orbit()
        .args([
            "--project",
            tmp.path().to_str().unwrap(),
            "search",
            "super-secret-value",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("No results"));
}

#[test]
fn run_executes_an_allowed_configured_command() {
    let tmp = scaffold_project();
    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success();
    fs::write(
        tmp.path().join(".orbit/project.yaml"),
        "version: 1\nproject:\n  name: demo\ncommands:\n  greet:\n    program: echo\n    args: [hello-from-orbit]\npermissions:\n  command.run_configured: allow\ncontext:\n  include:\n    - \"**/*\"\n",
    )
    .unwrap();

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "run", "greet"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello-from-orbit"))
        .stdout(predicate::str::contains("exited successfully"));
}

#[test]
fn chat_exits_cleanly_on_the_exit_command_without_calling_the_model() {
    let tmp = scaffold_project();
    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success();

    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "chat"])
        .write_stdin("exit\n")
        .assert()
        .success()
        .stdout(predicate::str::contains("Orbit chat"));
}

#[test]
fn mcp_serve_fails_fast_without_a_project_config() {
    let tmp = tempfile::tempdir().unwrap();
    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "mcp", "serve"])
        .assert()
        .failure()
        .code(2);
}

#[test]
fn doctor_reports_without_crashing_even_if_ollama_is_unreachable() {
    let tmp = scaffold_project();
    orbit()
        .args(["--project", tmp.path().to_str().unwrap(), "init"])
        .assert()
        .success();

    orbit()
        .args([
            "--project",
            tmp.path().to_str().unwrap(),
            "--ollama-endpoint",
            "http://127.0.0.1:1",
            "doctor",
        ])
        .assert()
        .stdout(predicate::str::contains("ollama connectivity"));
}
