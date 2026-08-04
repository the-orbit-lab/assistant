//! `orbit app serve --jsonl`: drive Orbit sessions over stdin/stdout.
//!
//! stdout carries JSON frames only — every event and every reply. All
//! logging and diagnostics go to stderr. A malformed or unusable request
//! produces an `error` frame and the process keeps serving; the loop only
//! ends when stdin closes or the client asks it to.

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};

use orbit_core::{AgentEvent, EventSink, OrbitError, PermissionRequestId, SessionMode};
use orbit_providers::OllamaProvider;
use orbit_session::{ConfirmationMode, SessionRuntime};
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::app::protocol::{
    BridgeFrame, BridgeRequest, PermissionMode, ProjectSummary, error_code, parse_request,
};
use crate::args::{AppServeArgs, GlobalArgs};
use crate::resolve::{resolve_project, resolve_workspace};
use crate::runtime::build_context;

/// Serializes writes to stdout so frames from concurrent turns never
/// interleave mid-line.
#[derive(Clone)]
struct FrameWriter {
    out: Arc<Mutex<std::io::Stdout>>,
}

impl FrameWriter {
    fn new() -> Self {
        Self {
            out: Arc::new(Mutex::new(std::io::stdout())),
        }
    }

    fn write<T: serde::Serialize>(&self, frame: &T) {
        let Ok(line) = serde_json::to_string(frame) else {
            // Serialization of our own types should not fail; if it ever
            // does, dropping the frame is better than emitting a partial
            // line that would corrupt the client's parser.
            eprintln!("Warning: failed to serialize an outgoing frame");
            return;
        };
        let mut out = self.out.lock().expect("stdout mutex poisoned");
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// The event sink: every agent event becomes one stdout line.
struct JsonlSink {
    writer: FrameWriter,
}

impl EventSink for JsonlSink {
    fn emit(&self, event: AgentEvent) {
        self.writer.write(&event);
    }
}

struct Bridge {
    writer: FrameWriter,
    sessions: Mutex<HashMap<String, Arc<SessionRuntime>>>,
    global: GlobalArgs,
}

impl Bridge {
    fn error(&self, code: &'static str, message: impl Into<String>) {
        self.writer.write(&BridgeFrame::Error {
            code,
            message: message.into(),
        });
    }

    fn session(&self, id: &str) -> Option<Arc<SessionRuntime>> {
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .get(id)
            .cloned()
    }

    async fn start_session(
        &self,
        workspace: Option<String>,
        project: Option<String>,
        streaming: Option<bool>,
        permissions: Option<PermissionMode>,
    ) -> Result<Arc<SessionRuntime>, OrbitError> {
        // Reuse the CLI's own resolution so the bridge honors exactly the
        // documented discovery precedence, rather than inventing its own.
        let mut global = self.global.clone();
        if workspace.is_some() {
            global.workspace = workspace.map(std::path::PathBuf::from);
        }
        if project.is_some() {
            global.project = project;
        }

        let mode = match permissions.unwrap_or_default() {
            PermissionMode::External => ConfirmationMode::External,
            PermissionMode::AllowAll => ConfirmationMode::AutoAllow,
            PermissionMode::DenyAll => ConfirmationMode::AutoDeny,
        };
        let streaming = streaming.unwrap_or(true);
        let sink = Arc::new(JsonlSink {
            writer: self.writer.clone(),
        });

        let use_workspace = global.workspace.is_some()
            || (global.project.is_none()
                && matches!(
                    crate::commands::discover_root(),
                    Ok(orbit_workspace::DiscoveredRoot::Workspace(_))
                ));

        let runtime = if use_workspace {
            let projects = resolve_workspace(&global)?;
            let provider = Arc::new(OllamaProvider::new(
                global
                    .ollama_endpoint
                    .clone()
                    .unwrap_or_else(|| orbit_project::config::DEFAULT_OLLAMA_ENDPOINT.to_string()),
                global
                    .model
                    .clone()
                    .unwrap_or_else(|| orbit_project::config::DEFAULT_OLLAMA_MODEL.to_string()),
            ));
            SessionRuntime::workspace(projects, provider, sink, mode, streaming)?
        } else {
            let loaded = resolve_project(&global)?;
            let provider = Arc::new(OllamaProvider::new(
                loaded.config.model.endpoint.clone(),
                loaded.config.model.model.clone(),
            ));
            let context = build_context(loaded);
            SessionRuntime::single_project(context, provider, sink, mode, streaming).await?
        };

        let runtime = Arc::new(runtime);
        self.sessions
            .lock()
            .expect("sessions mutex poisoned")
            .insert(runtime.id().0.clone(), runtime.clone());
        Ok(runtime)
    }

    async fn handle(self: &Arc<Self>, request: BridgeRequest) {
        match request {
            BridgeRequest::SessionStart {
                workspace,
                project,
                streaming,
                permissions,
            } => {
                // `session_started` is emitted by the runtime itself, so a
                // client learns the new id from the event stream.
                if let Err(err) = self
                    .start_session(workspace, project, streaming, permissions)
                    .await
                {
                    self.error(error_code::SESSION_START_FAILED, err.to_string());
                }
            }

            BridgeRequest::MessageSend { session_id, text } => {
                let Some(runtime) = self.session(&session_id) else {
                    self.error(
                        error_code::UNKNOWN_SESSION,
                        format!("no live session `{session_id}`"),
                    );
                    return;
                };
                // Spawned so cancellation and permission decisions can be
                // read from stdin while the turn is still running.
                let bridge = self.clone();
                tokio::spawn(async move {
                    if let Err(err) = runtime.send_message(&text).await {
                        bridge.error(error_code::REQUEST_FAILED, err.to_string());
                    }
                });
            }

            BridgeRequest::PermissionResolve {
                request_id,
                decision,
            } => {
                let id = PermissionRequestId(request_id.clone());
                // A decision may target any live session; the request id
                // is globally unique, so try each until one accepts it.
                let sessions: Vec<_> = self
                    .sessions
                    .lock()
                    .expect("sessions mutex poisoned")
                    .values()
                    .cloned()
                    .collect();
                let handled = sessions
                    .iter()
                    .any(|runtime| runtime.resolve_permission(&id, decision.into()));
                if handled {
                    self.writer.write(&BridgeFrame::Ack {
                        request: "permission.resolve",
                        session_id: None,
                    });
                } else {
                    self.error(
                        error_code::UNKNOWN_PERMISSION_REQUEST,
                        format!("no pending permission request `{request_id}`"),
                    );
                }
            }

            BridgeRequest::ExecutionCancel { session_id, .. } => {
                let Some(runtime) = self.session(&session_id) else {
                    self.error(
                        error_code::UNKNOWN_SESSION,
                        format!("no live session `{session_id}`"),
                    );
                    return;
                };
                if runtime.cancel_current_turn() {
                    self.writer.write(&BridgeFrame::Ack {
                        request: "execution.cancel",
                        session_id: Some(session_id),
                    });
                } else {
                    self.error(
                        error_code::NOTHING_TO_CANCEL,
                        format!("session `{session_id}` has no turn running"),
                    );
                }
            }

            BridgeRequest::ProjectsSet {
                session_id,
                projects,
            } => {
                let Some(runtime) = self.session(&session_id) else {
                    self.error(
                        error_code::UNKNOWN_SESSION,
                        format!("no live session `{session_id}`"),
                    );
                    return;
                };
                match runtime.set_active_projects(&projects).await {
                    // `active_projects_changed` is emitted by the runtime.
                    Ok(_) => self.writer.write(&BridgeFrame::Ack {
                        request: "projects.set",
                        session_id: Some(session_id),
                    }),
                    Err(err) => self.error(error_code::REQUEST_FAILED, err.to_string()),
                }
            }

            BridgeRequest::ProjectsList { session_id } => {
                let Some(runtime) = self.session(&session_id) else {
                    self.error(
                        error_code::UNKNOWN_SESSION,
                        format!("no live session `{session_id}`"),
                    );
                    return;
                };
                let projects = runtime
                    .project_summaries()
                    .into_iter()
                    .map(|(name, available, aliases, error)| ProjectSummary {
                        name,
                        available,
                        aliases,
                        error,
                    })
                    .collect();
                self.writer.write(&BridgeFrame::Projects {
                    session_id,
                    projects,
                });
            }

            BridgeRequest::SessionStatus { session_id } => {
                let Some(runtime) = self.session(&session_id) else {
                    self.error(
                        error_code::UNKNOWN_SESSION,
                        format!("no live session `{session_id}`"),
                    );
                    return;
                };
                let status = runtime.status().await;
                self.writer.write(&BridgeFrame::Status {
                    session_id: status.session_id.0,
                    mode: match status.mode {
                        SessionMode::SingleProject => "single_project".to_string(),
                        SessionMode::Workspace => "workspace".to_string(),
                    },
                    workspace: status.workspace,
                    active_projects: status.active_projects,
                    turns: status.turns,
                    message_count: status.message_count,
                    source_count: status.source_count,
                    action_count: status.action_count,
                    command_run_count: status.command_run_count,
                    running: matches!(status.execution, orbit_session::ExecutionState::Running(_)),
                    pending_permissions: status
                        .pending_permissions
                        .into_iter()
                        .map(|p| p.0)
                        .collect(),
                    streaming: status.streaming,
                });
            }

            BridgeRequest::SessionEnd { session_id } => {
                let runtime = self
                    .sessions
                    .lock()
                    .expect("sessions mutex poisoned")
                    .remove(&session_id);
                match runtime {
                    Some(runtime) => runtime.end("client requested").await,
                    None => self.error(
                        error_code::UNKNOWN_SESSION,
                        format!("no live session `{session_id}`"),
                    ),
                }
            }
        }
    }
}

pub async fn run(global: &GlobalArgs, args: AppServeArgs) -> Result<(), OrbitError> {
    // JSON Lines is the only transport today. Requiring the flag keeps
    // `orbit app serve` from silently meaning something different once
    // another transport exists.
    if !args.jsonl {
        return Err(OrbitError::Mcp(
            "`orbit app serve` requires --jsonl (the only transport currently implemented)."
                .to_string(),
        ));
    }

    let writer = FrameWriter::new();
    writer.write(&BridgeFrame::Ready {
        protocol_version: orbit_core::EVENT_PROTOCOL_VERSION,
    });
    eprintln!(
        "Orbit application bridge ready on stdio (JSON Lines, protocol v{}).",
        orbit_core::EVENT_PROTOCOL_VERSION
    );

    let bridge = Arc::new(Bridge {
        writer,
        sessions: Mutex::new(HashMap::new()),
        global: global.clone(),
    });

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| OrbitError::io("<stdin>", e))?
    {
        if line.trim().is_empty() {
            continue;
        }
        match parse_request(&line) {
            Ok(request) => bridge.handle(request).await,
            // Recoverable by design: report and keep serving.
            Err(frame) => bridge.writer.write(&frame),
        }
    }

    // stdin closed: end every session so external MCP servers shut down.
    let sessions: Vec<_> = bridge
        .sessions
        .lock()
        .expect("sessions mutex poisoned")
        .drain()
        .map(|(_, runtime)| runtime)
        .collect();
    for runtime in sessions {
        runtime.end("input stream closed").await;
    }
    Ok(())
}
