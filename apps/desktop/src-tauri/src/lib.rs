//! Orbit desktop: the native half.
//!
//! The webview gets four commands and no filesystem, no shell, and no
//! say in which program runs. Everything it can ask for is either a
//! protocol message forwarded to a backend this module started, or a
//! directory the user picked in a native dialog.

mod sidecar;

use std::path::PathBuf;

use sidecar::{Sidecar, SidecarError, SidecarState, StartedBackend};
use tauri::{AppHandle, Manager, State};

/// Every protocol command this app is allowed to send.
///
/// This list is the security boundary between the webview and the
/// backend: a request type absent from it never reaches Orbit's stdin.
const ALLOWED_COMMANDS: &[&str] = &[
    "session.start",
    "message.send",
    "permission.resolve",
    "execution.cancel",
    "projects.set",
    "projects.list",
    "session.status",
    "session.end",
];

/// Start the backend for a workspace directory.
///
/// `binary_path` is a development convenience -- a packaged build passes
/// `None` and gets the bundled sidecar. It is not a way to run an
/// arbitrary program from the UI: the argument list is fixed in
/// `SidecarState::start`, so whatever binary is used is always invoked
/// as `app serve --jsonl` against a directory the user chose.
#[tauri::command]
fn start_backend(
    app: AppHandle,
    state: State<'_, Sidecar>,
    workspace: String,
    binary_path: Option<String>,
) -> Result<StartedBackend, SidecarError> {
    let path = PathBuf::from(&workspace);
    if !path.is_dir() {
        return Err(SidecarError::SpawnFailed {
            detail: format!("{workspace} is not a directory"),
        });
    }
    state.start(app, &path, binary_path.as_deref())
}

/// Forward one protocol command.
#[tauri::command]
fn send_command(state: State<'_, Sidecar>, command: serde_json::Value) -> Result<(), SidecarError> {
    let kind = command.get("type").and_then(|t| t.as_str()).unwrap_or("");
    if !ALLOWED_COMMANDS.contains(&kind) {
        return Err(SidecarError::WriteFailed {
            detail: format!("`{kind}` is not a command this app sends"),
        });
    }
    state.send(&command)
}

#[tauri::command]
fn stop_backend(state: State<'_, Sidecar>) {
    state.stop();
}

#[tauri::command]
fn backend_running(state: State<'_, Sidecar>) -> bool {
    state.is_running()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            app.manage(Sidecar::new(SidecarState::default()));
            Ok(())
        })
        .on_window_event(|window, event| {
            // The backend outlives the webview otherwise, holding the
            // conversation and a child process nobody can reach.
            if matches!(event, tauri::WindowEvent::Destroyed)
                && let Some(state) = window.try_state::<Sidecar>()
            {
                state.stop();
            }
        })
        .invoke_handler(tauri::generate_handler![
            start_backend,
            send_command,
            stop_backend,
            backend_running
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::ALLOWED_COMMANDS;

    /// The allow-list is what stops the webview reaching anything but
    /// the protocol, so it is asserted rather than assumed.
    #[test]
    fn only_known_protocol_commands_are_forwarded() {
        assert!(ALLOWED_COMMANDS.contains(&"message.send"));
        assert!(ALLOWED_COMMANDS.contains(&"execution.cancel"));
        assert!(!ALLOWED_COMMANDS.contains(&"command.run_configured"));
        assert!(!ALLOWED_COMMANDS.contains(&"shell.execute"));
    }
}
