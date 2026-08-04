//! Owning the Orbit backend process.
//!
//! The frontend never touches a shell, a file, or the Orbit binary. It
//! sends four narrow commands to this module, and receives protocol
//! frames as Tauri events. Everything that could reach the filesystem —
//! the binary path, the workspace argument, the process lifetime —
//! stays on this side of the boundary.
//!
//! Three rules shape the implementation:
//!
//! - **stdout is frames, nothing else.** One JSON object per line, and a
//!   line that does not parse is reported as a diagnostic rather than
//!   dropped or guessed at. Orbit guarantees this (`crates/cli` asserts
//!   it in its own tests), so a parse failure means something is wrong
//!   and should be visible.
//! - **stderr never enters protocol parsing.** It is read on its own
//!   task and forwarded as a separate event, so a log line can never be
//!   mistaken for a frame.
//! - **Cancelling a turn is a message, not a kill.** The conversation
//!   lives in the backend's memory; killing the process to stop a turn
//!   would discard it.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

/// The protocol version this client understands.
///
/// `docs/APP_PROTOCOL.md`: adding an event or an optional field is
/// backwards-compatible; renaming or removing one increments this. A
/// mismatch is reported as an upgrade error rather than being ignored,
/// because the failure it produces otherwise is silent and confusing.
pub const SUPPORTED_PROTOCOL_VERSION: u32 = 1;

/// Tauri event names the frontend listens on.
pub const EVENT_FRAME: &str = "orbit://frame";
pub const EVENT_DIAGNOSTIC: &str = "orbit://diagnostic";
pub const EVENT_EXIT: &str = "orbit://exit";

/// A backend failure the UI has to explain to a person.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SidecarError {
    /// No Orbit binary at the configured or discovered path.
    BinaryNotFound { path: String },
    /// The binary started but never sent `ready`.
    NoReadyFrame { detail: String },
    /// `ready` arrived with a version this client cannot speak.
    ProtocolMismatch { expected: u32, actual: u32 },
    /// Spawning failed outright.
    SpawnFailed { detail: String },
    /// A command was sent with no backend running.
    NotRunning,
    /// Writing to the backend's stdin failed.
    WriteFailed { detail: String },
}

impl std::fmt::Display for SidecarError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BinaryNotFound { path } => {
                write!(f, "no Orbit binary at {path}")
            }
            Self::NoReadyFrame { detail } => {
                write!(f, "the backend never announced itself: {detail}")
            }
            Self::ProtocolMismatch { expected, actual } => write!(
                f,
                "this app speaks Orbit protocol v{expected}, the backend speaks v{actual}"
            ),
            Self::SpawnFailed { detail } => write!(f, "could not start Orbit: {detail}"),
            Self::NotRunning => write!(f, "the Orbit backend is not running"),
            Self::WriteFailed { detail } => write!(f, "could not send to Orbit: {detail}"),
        }
    }
}

/// What a successful start tells the UI.
#[derive(Debug, Serialize)]
pub struct StartedBackend {
    pub protocol_version: u32,
    pub binary_path: String,
    pub workspace: String,
}

/// Parse the `ready` frame and check its version.
///
/// Split out from the spawn path so the rule can be tested without a
/// process: this is the one check that decides whether talking to this
/// backend is safe at all.
pub fn validate_ready(line: &str) -> Result<u32, SidecarError> {
    let value: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| SidecarError::NoReadyFrame {
            detail: format!("first line was not JSON: {e}"),
        })?;

    if value.get("type").and_then(|t| t.as_str()) != Some("ready") {
        return Err(SidecarError::NoReadyFrame {
            detail: format!("first frame was {line}, expected a `ready` frame"),
        });
    }

    let actual = value
        .get("protocol_version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| SidecarError::NoReadyFrame {
            detail: "the `ready` frame carried no protocol_version".to_string(),
        })? as u32;

    if actual != SUPPORTED_PROTOCOL_VERSION {
        return Err(SidecarError::ProtocolMismatch {
            expected: SUPPORTED_PROTOCOL_VERSION,
            actual,
        });
    }
    Ok(actual)
}

/// One line of backend stdout, classified.
///
/// A line that does not parse is *reported*, never silently skipped:
/// Orbit guarantees stdout is frames only, so a malformed line means a
/// real problem and hiding it would make that problem invisible.
#[derive(Debug, PartialEq)]
pub enum StdoutLine {
    Frame(serde_json::Value),
    Malformed { raw: String, reason: String },
}

pub fn classify_stdout_line(line: &str) -> Option<StdoutLine> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }
    match serde_json::from_str::<serde_json::Value>(trimmed) {
        Ok(value) if value.is_object() => Some(StdoutLine::Frame(value)),
        Ok(_) => Some(StdoutLine::Malformed {
            raw: trimmed.to_string(),
            reason: "frame was not a JSON object".to_string(),
        }),
        Err(e) => Some(StdoutLine::Malformed {
            raw: trimmed.to_string(),
            reason: e.to_string(),
        }),
    }
}

/// The running backend, or nothing.
#[derive(Default)]
pub struct SidecarState {
    inner: Mutex<Option<RunningBackend>>,
}

struct RunningBackend {
    child: Child,
    stdin: ChildStdin,
}

/// Where to find the Orbit binary.
///
/// A development build points at the workspace's own `target/release`;
/// a packaged build uses the bundled sidecar next to the executable.
/// The frontend supplies neither — it can ask to start a backend, but
/// not to start an *arbitrary program*.
pub fn resolve_binary(configured: Option<&str>) -> Result<PathBuf, SidecarError> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(path) = configured.filter(|p| !p.trim().is_empty()) {
        candidates.push(PathBuf::from(path));
    }
    // Bundled sidecar, next to the app executable.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("orbit"));
    }
    // Development: this repository's own release build.
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("../../target/release/orbit"));
        candidates.push(cwd.join("../../../target/release/orbit"));
    }

    for candidate in &candidates {
        if candidate.is_file() {
            return candidate
                .canonicalize()
                .map_err(|e| SidecarError::BinaryNotFound {
                    path: format!("{}: {e}", candidate.display()),
                });
        }
    }
    Err(SidecarError::BinaryNotFound {
        path: candidates
            .iter()
            .map(|c| c.display().to_string())
            .collect::<Vec<_>>()
            .join(", "),
    })
}

impl SidecarState {
    /// Spawn the backend for `workspace`, read and validate `ready`, and
    /// begin forwarding frames.
    ///
    /// The argument list is fixed here. The frontend chooses a workspace
    /// directory and nothing else — it cannot add flags, so it cannot
    /// turn this into a way to run something other than the bridge.
    pub fn start(
        &self,
        app: AppHandle,
        workspace: &Path,
        configured_binary: Option<&str>,
    ) -> Result<StartedBackend, SidecarError> {
        self.stop();

        let binary = resolve_binary(configured_binary)?;
        let mut child = Command::new(&binary)
            .arg("--workspace")
            .arg(workspace)
            .arg("app")
            .arg("serve")
            .arg("--jsonl")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| SidecarError::SpawnFailed {
                detail: e.to_string(),
            })?;

        let stdin = child.stdin.take().ok_or(SidecarError::SpawnFailed {
            detail: "no stdin pipe".to_string(),
        })?;
        let stdout = child.stdout.take().ok_or(SidecarError::SpawnFailed {
            detail: "no stdout pipe".to_string(),
        })?;
        let stderr = child.stderr.take().ok_or(SidecarError::SpawnFailed {
            detail: "no stderr pipe".to_string(),
        })?;

        let mut reader = BufReader::new(stdout);
        let mut first = String::new();
        // The handshake is synchronous on purpose: nothing else may be
        // sent until the version is known to be one we can speak.
        reader
            .read_line(&mut first)
            .map_err(|e| SidecarError::NoReadyFrame {
                detail: e.to_string(),
            })?;
        let protocol_version = validate_ready(&first).inspect_err(|_| {
            let _ = child.kill();
        })?;

        // stderr on its own task, so a log line can never be parsed as a
        // frame no matter how much of it there is.
        let diagnostics = app.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let _ = diagnostics.emit(EVENT_DIAGNOSTIC, line);
            }
        });

        let frames = app.clone();
        std::thread::spawn(move || {
            for line in reader.lines().map_while(Result::ok) {
                match classify_stdout_line(&line) {
                    Some(StdoutLine::Frame(value)) => {
                        let _ = frames.emit(EVENT_FRAME, value);
                    }
                    Some(StdoutLine::Malformed { raw, reason }) => {
                        let _ = frames.emit(
                            EVENT_DIAGNOSTIC,
                            format!("malformed frame ignored ({reason}): {raw}"),
                        );
                    }
                    None => {}
                }
            }
            // stdout closing means the backend is gone.
            let _ = frames.emit(EVENT_EXIT, "the Orbit backend stopped");
        });

        *self.inner.lock().unwrap() = Some(RunningBackend { child, stdin });

        Ok(StartedBackend {
            protocol_version,
            binary_path: binary.display().to_string(),
            workspace: workspace.display().to_string(),
        })
    }

    /// Write one command line to the backend's stdin.
    pub fn send(&self, command: &serde_json::Value) -> Result<(), SidecarError> {
        let mut guard = self.inner.lock().unwrap();
        let backend = guard.as_mut().ok_or(SidecarError::NotRunning)?;
        let line = format!("{command}\n");
        backend
            .stdin
            .write_all(line.as_bytes())
            .and_then(|_| backend.stdin.flush())
            .map_err(|e| SidecarError::WriteFailed {
                detail: e.to_string(),
            })
    }

    pub fn is_running(&self) -> bool {
        self.inner.lock().unwrap().is_some()
    }

    /// Close stdin and wait briefly, then kill.
    ///
    /// Closing stdin is how the protocol says goodbye — it ends every
    /// live session cleanly. Killing is the fallback for a backend that
    /// does not exit on its own.
    pub fn stop(&self) {
        let Some(mut backend) = self.inner.lock().unwrap().take() else {
            return;
        };
        drop(backend.stdin);
        for _ in 0..20 {
            match backend.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        let _ = backend.child.kill();
        let _ = backend.child.wait();
    }
}

/// Shared handle for Tauri's managed state.
pub type Sidecar = Arc<SidecarState>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ready_frame_at_the_supported_version_is_accepted() {
        let version = validate_ready(r#"{"type":"ready","protocol_version":1}"#).unwrap();
        assert_eq!(version, SUPPORTED_PROTOCOL_VERSION);
    }

    /// A version this client cannot speak must produce an upgrade error,
    /// not a session that fails later in a confusing way.
    #[test]
    fn a_future_protocol_version_is_rejected_with_an_upgrade_error() {
        let err = validate_ready(r#"{"type":"ready","protocol_version":99}"#).unwrap_err();
        assert_eq!(
            err,
            SidecarError::ProtocolMismatch {
                expected: 1,
                actual: 99
            }
        );
        assert!(err.to_string().contains("v99"));
    }

    #[test]
    fn a_first_frame_that_is_not_ready_is_rejected() {
        let err = validate_ready(r#"{"type":"session_started"}"#).unwrap_err();
        assert!(matches!(err, SidecarError::NoReadyFrame { .. }));
    }

    #[test]
    fn a_non_json_first_line_is_rejected() {
        let err = validate_ready("Orbit application bridge ready").unwrap_err();
        assert!(matches!(err, SidecarError::NoReadyFrame { .. }));
    }

    #[test]
    fn a_ready_frame_without_a_version_is_rejected() {
        let err = validate_ready(r#"{"type":"ready"}"#).unwrap_err();
        assert!(matches!(err, SidecarError::NoReadyFrame { .. }));
    }

    #[test]
    fn a_well_formed_frame_is_parsed() {
        let line = r#"{"type":"response_delta","turn_id":"turn-1","text":"hello"}"#;
        match classify_stdout_line(line) {
            Some(StdoutLine::Frame(value)) => {
                assert_eq!(value["type"], "response_delta");
                assert_eq!(value["text"], "hello");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    /// A malformed line is surfaced rather than dropped. Orbit
    /// guarantees stdout is frames only, so one that is not means
    /// something is wrong and hiding it would hide the problem.
    #[test]
    fn a_malformed_line_is_reported_not_dropped() {
        match classify_stdout_line("{not json") {
            Some(StdoutLine::Malformed { raw, .. }) => assert_eq!(raw, "{not json"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_json_scalar_is_not_a_frame() {
        assert!(matches!(
            classify_stdout_line("42"),
            Some(StdoutLine::Malformed { .. })
        ));
    }

    #[test]
    fn blank_lines_are_ignored() {
        assert_eq!(classify_stdout_line("   "), None);
        assert_eq!(classify_stdout_line(""), None);
    }

    /// A configured path that does not exist falls through to
    /// discovery rather than failing: a stale setting should not stop
    /// the app from finding the binary sitting next to it.
    #[test]
    fn a_stale_configured_path_falls_back_to_discovery() {
        match resolve_binary(Some("/nonexistent/orbit")) {
            // In this repository the development build is discoverable,
            // so the fallback finds it.
            Ok(path) => assert!(path.ends_with("orbit"), "{}", path.display()),
            // Elsewhere nothing is discoverable, and the error has to
            // name what was tried so the message is actionable.
            Err(SidecarError::BinaryNotFound { path }) => {
                assert!(path.contains("/nonexistent/orbit"), "{path}");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn a_missing_binary_error_names_the_paths_it_tried() {
        let err = SidecarError::BinaryNotFound {
            path: "/a/orbit, /b/orbit".to_string(),
        };
        assert!(err.to_string().contains("/a/orbit"));
    }

    #[test]
    fn sending_with_no_backend_is_an_error_not_a_panic() {
        let state = SidecarState::default();
        assert!(!state.is_running());
        assert_eq!(
            state.send(&serde_json::json!({"type":"session.status"})),
            Err(SidecarError::NotRunning)
        );
    }

    #[test]
    fn stopping_an_unstarted_backend_is_harmless() {
        SidecarState::default().stop();
    }
}
