//! Real subprocess test for `orbit app serve --jsonl`.
//!
//! Spawns the actual compiled binary and speaks the JSON Lines protocol to
//! its stdin/stdout, exactly as a desktop or voice client would. Nothing
//! here calls a Rust function directly, so it is a genuine check of the
//! protocol boundary: every stdout line must be a JSON frame, malformed
//! input must not kill the process, and session/action/source events must
//! arrive in the documented order.
//!
//! No model is required. Ollama is deliberately pointed at a closed port,
//! which still exercises the whole pipeline up to the model call —
//! deterministic retrieval runs before the model and emits real action and
//! source events — and then reports a clean failure.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

use assert_cmd::cargo::CommandCargoExt;
use serde_json::{Value, json};

struct Bridge {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    /// Every line seen, so the whole transcript can be checked at the end.
    seen: Vec<Value>,
}

impl Bridge {
    fn spawn(workspace: &std::path::Path) -> Self {
        let mut cmd = Command::cargo_bin("orbit").expect("orbit binary must be built");
        cmd.args([
            "--workspace",
            workspace.to_str().unwrap(),
            // A closed port: the pipeline runs, the model call fails.
            "--ollama-endpoint",
            "http://127.0.0.1:1",
            "app",
            "serve",
            "--jsonl",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("failed to spawn orbit app serve");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            seen: Vec::new(),
        }
    }

    fn send(&mut self, message: Value) {
        writeln!(self.stdin, "{message}").expect("failed to write to orbit stdin");
        self.stdin.flush().unwrap();
    }

    fn send_raw(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("failed to write to orbit stdin");
        self.stdin.flush().unwrap();
    }

    /// Read one frame, asserting it is valid JSON. This is the stdout
    /// cleanliness guarantee: a stray `println!` anywhere in Orbit would
    /// fail here.
    fn read_frame(&mut self) -> Value {
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .expect("failed to read orbit stdout");
        assert!(read > 0, "orbit closed stdout unexpectedly");
        let trimmed = line.trim_end();
        let value: Value = serde_json::from_str(trimmed).unwrap_or_else(|e| {
            panic!("stdout line was not valid JSON (stdout is frames-only): {e}\nline: {trimmed:?}")
        });
        self.seen.push(value.clone());
        value
    }

    /// Read until a frame with this `type` arrives.
    fn read_until(&mut self, type_name: &str) -> Value {
        for _ in 0..200 {
            let frame = self.read_frame();
            if frame["type"] == type_name {
                return frame;
            }
        }
        panic!("never saw a `{type_name}` frame; saw: {:?}", self.types());
    }

    fn types(&self) -> Vec<String> {
        self.seen
            .iter()
            .map(|f| f["type"].as_str().unwrap_or("?").to_string())
            .collect()
    }

    fn shutdown(mut self) -> String {
        drop(self.stdin);
        let _ = self.child.wait();
        let mut stderr = String::new();
        if let Some(mut handle) = self.child.stderr.take() {
            use std::io::Read;
            let _ = handle.read_to_string(&mut stderr);
        }
        stderr
    }
}

fn write_project(root: &std::path::Path, name: &str, extra: &[(&str, &str)]) {
    std::fs::create_dir_all(root.join(".orbit")).unwrap();
    std::fs::write(
        root.join(".orbit/project.yaml"),
        format!(
            "version: 1\nproject:\n  name: {name}\ncontext:\n  include:\n    - \"**/*\"\n\
             permissions:\n  project.information: allow\n  project.list_files: allow\n  \
             project.read_file: allow\n  project.search: allow\n"
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

fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
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
            "// watchdog resets the system after a brownout\n",
        )],
    );
    std::fs::create_dir_all(root.join(".orbit")).unwrap();
    std::fs::write(
        root.join(".orbit/workspace.yaml"),
        "version: 1\n\
         workspace:\n  name: Orbit Lab\n  description: bridge test workspace\n\
         projects:\n\
         \x20\x20docs:\n    path: ./docs\n\
         \x20\x20obc:\n    path: ./obc\n\
         defaults:\n  project: docs\n",
    )
    .unwrap();
    tmp
}

#[test]
fn a_full_session_runs_over_the_protocol() {
    let tmp = fixture();
    let workspace = tmp.path().canonicalize().unwrap();
    let mut bridge = Bridge::spawn(&workspace);

    // 1. ready, with the protocol version a client should check.
    let ready = bridge.read_frame();
    assert_eq!(ready["type"], "ready");
    assert_eq!(
        ready["protocol_version"],
        orbit_core::EVENT_PROTOCOL_VERSION
    );

    // 2. session creation.
    bridge.send(json!({"type": "session.start", "permissions": "deny_all"}));
    let started = bridge.read_until("session_started");
    let session_id = started["session_id"].as_str().unwrap().to_string();
    assert_eq!(started["mode"], "workspace");
    assert_eq!(started["workspace"], "Orbit Lab");

    // 3. project selection is announced as an event.
    bridge.send(json!({
        "type": "projects.set",
        "session_id": session_id,
        "projects": ["docs"],
    }));
    let changed = bridge.read_until("active_projects_changed");
    assert_eq!(changed["projects"], json!(["docs"]));

    // 4. a message: retrieval, actions and sources all really happen
    //    before the model is reached.
    bridge.send(json!({
        "type": "message.send",
        "session_id": session_id,
        "text": "Why STM32?",
    }));

    let user = bridge.read_until("user_message_received");
    assert_eq!(user["text"], "Why STM32?");
    assert_eq!(user["session_id"], session_id.as_str());
    assert!(user["turn_id"].is_string(), "events carry a turn id");

    bridge.read_until("retrieval_started");
    let action = bridge.read_until("action_started");
    assert!(action["execution_id"].is_string());
    // A workspace-scoped action can touch several projects in one call,
    // so it is not attributed to any single one; per-project identity
    // arrives on the source events below.
    assert!(
        action["project"].is_null(),
        "workspace actions must not claim a project: {action}"
    );

    let source = bridge.read_until("source_found");
    assert_eq!(source["project"], "docs", "sources carry project identity");
    assert!(source["path"].is_string());

    bridge.read_until("retrieval_completed");

    // 5. the model is unreachable, so the turn fails cleanly rather than
    //    inventing an answer.
    let failure = bridge.read_until("failure");
    assert!(
        failure["message"].as_str().unwrap().contains("127.0.0.1:1")
            || failure["message"]
                .as_str()
                .unwrap()
                .to_lowercase()
                .contains("could not reach"),
        "unexpected failure message: {failure}"
    );

    // 6. status reflects the work done.
    bridge.send(json!({"type": "session.status", "session_id": session_id}));
    let status = bridge.read_until("status");
    assert_eq!(status["active_projects"], json!(["docs"]));
    assert_eq!(status["turns"], 1);
    assert!(status["source_count"].as_u64().unwrap() > 0);
    assert_eq!(status["running"], false);

    // 7. clean shutdown.
    bridge.send(json!({"type": "session.end", "session_id": session_id}));
    let ended = bridge.read_until("session_ended");
    assert_eq!(ended["session_id"], session_id.as_str());

    // Everything on stdout was a JSON frame (read_frame asserts per line).
    assert!(bridge.seen.len() > 8, "saw: {:?}", bridge.types());

    let stderr = bridge.shutdown();
    assert!(
        stderr.contains("Orbit application bridge ready"),
        "diagnostics belong on stderr: {stderr:?}"
    );
}

#[test]
fn malformed_and_invalid_requests_are_reported_without_killing_the_process() {
    let tmp = fixture();
    let workspace = tmp.path().canonicalize().unwrap();
    let mut bridge = Bridge::spawn(&workspace);
    assert_eq!(bridge.read_frame()["type"], "ready");

    // Not JSON at all.
    bridge.send_raw("{this is not json");
    let error = bridge.read_until("error");
    assert_eq!(error["code"], "malformed_json");

    // Valid JSON, unknown request type.
    bridge.send(json!({"type": "does.not.exist"}));
    let error = bridge.read_until("error");
    assert_eq!(error["code"], "unknown_request");

    // Valid type, missing a required field.
    bridge.send(json!({"type": "message.send", "text": "hi"}));
    let error = bridge.read_until("error");
    assert_eq!(error["code"], "unknown_request");

    // Well-formed but referring to a session that does not exist.
    bridge.send(json!({"type": "session.status", "session_id": "sess-nope"}));
    let error = bridge.read_until("error");
    assert_eq!(error["code"], "unknown_session");

    // Cancelling with nothing running is reported, not silently accepted.
    bridge.send(json!({"type": "session.start", "permissions": "deny_all"}));
    let started = bridge.read_until("session_started");
    let session_id = started["session_id"].as_str().unwrap().to_string();
    bridge.send(json!({"type": "execution.cancel", "session_id": session_id}));
    let error = bridge.read_until("error");
    assert_eq!(error["code"], "nothing_to_cancel");

    // After all of that the process is still serving normally.
    bridge.send(json!({"type": "session.status", "session_id": session_id}));
    let status = bridge.read_until("status");
    assert_eq!(status["session_id"], session_id.as_str());

    bridge.shutdown();
}

#[test]
fn a_single_project_session_works_over_the_protocol_too() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    write_project(&root, "obc", &[("src/watchdog.rs", "// watchdog\n")]);

    let mut cmd = Command::cargo_bin("orbit").unwrap();
    cmd.args([
        "--project",
        root.to_str().unwrap(),
        "--ollama-endpoint",
        "http://127.0.0.1:1",
        "app",
        "serve",
        "--jsonl",
    ])
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = cmd.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut read = || {
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(line.trim_end()).expect("stdout must be JSON frames only")
    };

    assert_eq!(read()["type"], "ready");
    writeln!(
        stdin,
        r#"{{"type":"session.start","permissions":"deny_all"}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    let started = read();
    assert_eq!(started["type"], "session_started");
    assert_eq!(started["mode"], "single_project");
    assert_eq!(started["projects"], json!(["obc"]));

    drop(stdin);
    let _ = child.wait();
}

/// `orbit app serve` without a transport flag must fail loudly rather than
/// starting something the client did not ask for.
#[test]
fn serve_requires_an_explicit_transport() {
    let tmp = fixture();
    let mut cmd = Command::cargo_bin("orbit").unwrap();
    let output = cmd
        .args(["--workspace", tmp.path().to_str().unwrap(), "app", "serve"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--jsonl"),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
