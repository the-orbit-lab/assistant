//! The handshake, against the real Orbit binary.
//!
//! Unit tests cover the rules; this covers the assumption underneath
//! them — that the binary this app ships alongside actually speaks the
//! protocol this app validates. It spawns the real `orbit app serve
//! --jsonl`, so a change to Orbit's first frame fails here rather than
//! in front of a user.
//!
//! Skipped when no release binary exists, so a clean checkout does not
//! fail before `cargo build --release`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// The workspace to open, when this checkout sits inside one.
///
/// `--workspace` needs a directory holding `.orbit/workspace.yaml`; the
/// assistant repository itself is a *project*, one level down. Skipped
/// rather than failed when the checkout stands alone.
fn workspace_root() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../..")
        .canonicalize()
        .ok()?;
    root.join(".orbit/workspace.yaml").is_file().then_some(root)
}

fn orbit_binary() -> Option<PathBuf> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../target/release/orbit")
        .canonicalize()
        .ok()?;
    path.is_file().then_some(path)
}

#[test]
fn the_real_backend_announces_a_protocol_version_this_app_supports() {
    let Some(binary) = orbit_binary() else {
        eprintln!("skipping: no release build of orbit");
        return;
    };

    let Some(workspace) = workspace_root() else {
        eprintln!("skipping: this checkout is not inside an Orbit workspace");
        return;
    };
    let mut child = Command::new(&binary)
        .arg("--workspace")
        .arg(&workspace)
        .arg("app")
        .arg("serve")
        .arg("--jsonl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("orbit should start");

    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut first = String::new();
    stdout.read_line(&mut first).unwrap();

    // The rule the app enforces at startup, against the real thing.
    let value: serde_json::Value = serde_json::from_str(first.trim()).expect("first line is JSON");
    assert_eq!(value["type"], "ready", "first frame must be `ready`");
    assert_eq!(
        value["protocol_version"], 1,
        "this app supports protocol v1; update SUPPORTED_PROTOCOL_VERSION together with the UI"
    );

    // A session starts and reports its id on the event stream.
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"type":"session.start","workspace":"{}","permissions":"external"}}"#,
        workspace.display()
    )
    .unwrap();
    stdin.flush().unwrap();

    let mut second = String::new();
    stdout.read_line(&mut second).unwrap();
    let started: serde_json::Value = serde_json::from_str(second.trim()).expect("frame is JSON");
    assert_eq!(started["type"], "session_started");
    assert!(
        started["session_id"]
            .as_str()
            .is_some_and(|s| !s.is_empty()),
        "session_started must carry a session_id: {second}"
    );

    // Closing stdin is how the protocol says goodbye.
    drop(stdin);
    let _ = child.wait();
}

/// Every stdout line is a frame. The app parses them without filtering,
/// so a stray print anywhere in Orbit would corrupt the stream.
#[test]
fn every_stdout_line_from_the_real_backend_parses_as_a_frame() {
    let Some(binary) = orbit_binary() else {
        eprintln!("skipping: no release build of orbit");
        return;
    };
    let Some(workspace) = workspace_root() else {
        eprintln!("skipping: this checkout is not inside an Orbit workspace");
        return;
    };

    let mut child = Command::new(&binary)
        .arg("--workspace")
        .arg(&workspace)
        .arg("app")
        .arg("serve")
        .arg("--jsonl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("orbit should start");

    let mut stdin = child.stdin.take().unwrap();
    // A malformed line must produce an error frame, not kill the bridge.
    writeln!(stdin, "{{not json").unwrap();
    writeln!(stdin, r#"{{"type":"nonsense.request"}}"#).unwrap();
    stdin.flush().unwrap();
    drop(stdin);

    let mut saw_error = false;
    for line in BufReader::new(child.stdout.take().unwrap())
        .lines()
        .map_while(Result::ok)
    {
        if line.trim().is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("not a frame: {line} ({e})"));
        assert!(value.is_object(), "frame must be an object: {line}");
        if value["type"] == "error" {
            saw_error = true;
        }
    }
    assert!(saw_error, "malformed input should produce an error frame");
    let _ = child.wait();
}
