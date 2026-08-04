//! `orbit chat`: the terminal front end for a session.
//!
//! This module is a **renderer**. It turns [`orbit_core::AgentEvent`]s into
//! terminal output and turns typed lines into session calls; it contains no
//! agent logic, no retrieval, and no permission policy. The JSONL bridge
//! and a future SwiftUI client consume the same events and render them
//! differently without any of this code changing.
//!
//! Conversation state lives in the session for the life of the process and
//! is never written to disk.

use std::io::{IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use orbit_core::{
    AgentEvent, EventPayload, EventSink, OrbitError, PermissionDecision, SessionMode,
};
use orbit_providers::OllamaProvider;
use orbit_session::{COMMAND_HELP, ConfirmationMode, ParsedInput, SessionCommand, SessionRuntime};
use orbit_workspace::DiscoveredRoot;

use crate::args::GlobalArgs;
use crate::commands::discover_root;
use crate::resolve::{resolve_project, resolve_workspace};
use crate::runtime::build_context;

/// Renders events to the terminal as they happen.
///
/// Streaming text prints inline as it arrives; everything else prints on
/// its own line. `in_stream` tracks whether a partial answer line is open,
/// so a status line never lands in the middle of a sentence.
struct TerminalRenderer {
    in_stream: AtomicBool,
    /// Sources for the turn being rendered, in arrival order.
    turn_sources: Mutex<Vec<String>>,
    session: Mutex<Option<Arc<SessionRuntime>>>,
    interactive: bool,
}

impl TerminalRenderer {
    fn new(interactive: bool) -> Self {
        Self {
            in_stream: AtomicBool::new(false),
            turn_sources: Mutex::new(Vec::new()),
            session: Mutex::new(None),
            interactive,
        }
    }

    fn attach(&self, session: Arc<SessionRuntime>) {
        *self.session.lock().expect("renderer mutex poisoned") = Some(session);
    }

    /// Close an in-progress streamed line before printing anything else.
    fn end_stream_line(&self) {
        if self.in_stream.swap(false, Ordering::SeqCst) {
            println!();
        }
    }

    fn status(&self, text: &str) {
        self.end_stream_line();
        println!("{text}");
    }

    fn take_turn_sources(&self) -> Vec<String> {
        std::mem::take(&mut *self.turn_sources.lock().expect("renderer mutex poisoned"))
    }
}

impl EventSink for TerminalRenderer {
    fn emit(&self, event: AgentEvent) {
        let project = event.project.clone();
        match event.payload {
            EventPayload::ActiveProjectsChanged { projects } => {
                self.status(&format!("• active project(s): {}", projects.join(", ")));
            }
            EventPayload::RetrievalStarted { scope } if !scope.is_empty() => {
                self.status(&format!("• gathering context from {}", scope.join(", ")));
            }
            EventPayload::ActionStarted { action } => {
                let where_ = project.map(|p| format!(" [{p}]")).unwrap_or_default();
                self.status(&format!("• {action}{where_}"));
            }
            EventPayload::ActionFailed { action, error } => {
                self.status(&format!("• {action} failed: {error}"));
            }
            EventPayload::SourceFound {
                path,
                line_start,
                line_end,
                section,
            } => {
                let location = match (line_start, line_end) {
                    (Some(a), Some(b)) if a == b => format!(":{a}"),
                    (Some(a), Some(b)) => format!(":{a}-{b}"),
                    (Some(a), None) => format!(":{a}"),
                    _ => String::new(),
                };
                let section = section.map(|s| format!(" ({s})")).unwrap_or_default();
                let rendered = match project {
                    Some(p) => format!("{p}:{path}{location}{section}"),
                    None => format!("{path}{location}{section}"),
                };
                let mut sources = self.turn_sources.lock().expect("renderer mutex poisoned");
                if !sources.contains(&rendered) {
                    sources.push(rendered);
                }
            }
            EventPayload::ResponseDelta { text } => {
                if !text.is_empty() {
                    if !self.in_stream.swap(true, Ordering::SeqCst) {
                        println!();
                    }
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                }
            }
            EventPayload::PermissionRequired {
                request_id,
                action,
                description,
                arguments,
            } => {
                self.end_stream_line();
                let session = self
                    .session
                    .lock()
                    .expect("renderer mutex poisoned")
                    .clone();
                let Some(session) = session else { return };

                if !self.interactive {
                    // Nothing here can answer, so deny explicitly rather
                    // than leaving the turn blocked forever.
                    println!(
                        "• {action} requires confirmation, but this session is not interactive; denying."
                    );
                    session.resolve_permission(&request_id, PermissionDecision::DenyOnce);
                    return;
                }

                println!("\nAction `{action}` requires confirmation.");
                if let Some(project) = &project {
                    println!("  Project:   {project}");
                }
                println!("  Action:    {description}");
                println!("  Arguments: {arguments}");

                // Prompting blocks on stdin, so it runs on its own thread:
                // the turn is awaiting this decision on the async runtime
                // and must not be blocked by the read.
                std::thread::spawn(move || {
                    print!("Allow? [y/N] ");
                    let _ = std::io::stdout().flush();
                    let mut line = String::new();
                    let approved = std::io::stdin().read_line(&mut line).is_ok()
                        && matches!(line.trim().to_lowercase().as_str(), "y" | "yes");
                    session.resolve_permission(
                        &request_id,
                        if approved {
                            PermissionDecision::AllowOnce
                        } else {
                            PermissionDecision::DenyOnce
                        },
                    );
                });
            }
            EventPayload::ExecutionCancelled { .. } => self.status("• cancelled"),
            EventPayload::Warning { message } => self.status(&format!("• warning: {message}")),
            EventPayload::Failure { message } => self.status(&format!("• failed: {message}")),
            _ => {}
        }
    }
}

pub async fn run(global: &GlobalArgs) -> Result<(), OrbitError> {
    let interactive = std::io::stdin().is_terminal();
    let renderer = Arc::new(TerminalRenderer::new(interactive));

    // `--yes` approves up front; otherwise decisions come from the prompt
    // the renderer raises in response to the same `PermissionRequired`
    // event every other front end sees.
    let confirmation_mode = if global.yes {
        ConfirmationMode::AutoAllow
    } else {
        ConfirmationMode::External
    };

    let use_workspace = global.workspace.is_some()
        || (global.project.is_none() && matches!(discover_root()?, DiscoveredRoot::Workspace(_)));

    let session = Arc::new(if use_workspace {
        let projects = resolve_workspace(global)?;
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
        SessionRuntime::workspace(
            projects,
            provider,
            renderer.clone(),
            confirmation_mode,
            true,
        )?
    } else {
        let loaded = resolve_project(global)?;
        let provider = Arc::new(OllamaProvider::new(
            loaded.config.model.endpoint.clone(),
            loaded.config.model.model.clone(),
        ));
        let context = build_context(loaded);
        SessionRuntime::single_project(context, provider, renderer.clone(), confirmation_mode, true)
            .await?
    });
    renderer.attach(session.clone());

    print_banner(&session);

    let stdin = std::io::stdin();
    loop {
        print!("\n> ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin
            .read_line(&mut line)
            .map_err(|e| OrbitError::io(".", e))?
            == 0
        {
            println!();
            break;
        }

        match orbit_session::parse(&line) {
            ParsedInput::Empty => continue,
            ParsedInput::UnknownCommand(message) => {
                println!("{message}");
                continue;
            }
            ParsedInput::Command(SessionCommand::Exit) => break,
            ParsedInput::Command(command) => handle_command(&session, command).await,
            ParsedInput::Message(text) => run_turn(&session, &renderer, &text).await,
        }
    }

    session.end("client exited").await;
    Ok(())
}

fn print_banner(session: &SessionRuntime) {
    match session.mode() {
        SessionMode::Workspace => {
            println!(
                "Orbit chat — workspace `{}` (session {}).",
                session.workspace_name().unwrap_or("workspace"),
                session.id()
            );
            println!("No project is selected yet; name one in a question or use `/use <project>`.");
        }
        SessionMode::SingleProject => {
            println!(
                "Orbit chat — project `{}` (session {}).",
                session
                    .known_projects()
                    .first()
                    .map(String::as_str)
                    .unwrap_or(""),
                session.id()
            );
        }
    }
    println!("Type `/help` for commands, `/exit` to quit.");
}

/// Run one turn. Ctrl-C cancels it rather than killing the process, so the
/// session, its history, and its completed results survive.
async fn run_turn(session: &Arc<SessionRuntime>, renderer: &TerminalRenderer, text: &str) {
    let cancel_target = session.clone();
    let turn = {
        let session = session.clone();
        let text = text.to_string();
        tokio::spawn(async move { session.send_message(&text).await })
    };

    let result = tokio::select! {
        result = turn => result.map_err(|e| e.to_string()),
        signal = tokio::signal::ctrl_c() => {
            if signal.is_ok() {
                cancel_target.cancel_current_turn();
            }
            // The turn observes the cancellation and returns on its own.
            // It is never aborted, so completed work is preserved.
            Err("cancelled".to_string())
        }
    };

    renderer.end_stream_line();

    match result {
        Ok(Ok(outcome)) => {
            if outcome.cancelled {
                println!("(cancelled — everything already completed in this session is kept)");
            } else if outcome.used_default_project {
                println!("\n(using the workspace default project)");
            }
            let sources = renderer.take_turn_sources();
            if !sources.is_empty() {
                println!("\nSources:");
                for source in sources {
                    println!("- {source}");
                }
            }
        }
        Ok(Err(err)) => eprintln!("Error: {err}"),
        Err(_) => {
            // Ctrl-C path: give the turn a moment to unwind so its final
            // events render before the next prompt is drawn.
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            renderer.take_turn_sources();
        }
    }
}

async fn handle_command(session: &Arc<SessionRuntime>, command: SessionCommand) {
    match command {
        SessionCommand::Help => {
            println!("Commands:");
            for (name, description) in COMMAND_HELP {
                println!("  {name:<16} {description}");
            }
        }
        SessionCommand::Projects => {
            for (name, available, aliases, error) in session.project_summaries() {
                let status = if available {
                    "available"
                } else {
                    "unavailable"
                };
                let alias = if aliases.is_empty() {
                    String::new()
                } else {
                    format!(" (aliases: {})", aliases.join(", "))
                };
                println!("  {name} [{status}]{alias}");
                if let Some(error) = error {
                    println!("      {error}");
                }
            }
        }
        SessionCommand::Use(projects) => match session.set_active_projects(&projects).await {
            Ok(names) => println!("Active project(s): {}", names.join(", ")),
            Err(err) => println!("Error: {err}"),
        },
        SessionCommand::Status => {
            let status = session.status().await;
            println!("Session:   {}", status.session_id);
            println!(
                "Mode:      {}",
                match status.mode {
                    SessionMode::SingleProject => "single project",
                    SessionMode::Workspace => "workspace",
                }
            );
            if let Some(workspace) = &status.workspace {
                println!("Workspace: {workspace}");
            }
            println!(
                "Projects:  {}",
                if status.active_projects.is_empty() {
                    "(none selected)".to_string()
                } else {
                    status.active_projects.join(", ")
                }
            );
            println!("Turns:     {}", status.turns);
            println!("Messages:  {}", status.message_count);
            println!("Actions:   {}", status.action_count);
            println!("Commands:  {}", status.command_run_count);
            println!("Sources:   {}", status.source_count);
            println!("Streaming: {}", status.streaming);
        }
        SessionCommand::Sources => {
            let sources = session.sources().await;
            if sources.is_empty() {
                println!("No sources yet.");
            } else {
                for source in sources {
                    println!("- {source}");
                }
            }
        }
        SessionCommand::Cancel => {
            if session.cancel_current_turn() {
                println!("Cancelling the current turn.");
            } else {
                println!("Nothing is running.");
            }
        }
        SessionCommand::Clear => {
            session.clear().await;
            println!("Conversation cleared. The session and project selection are kept.");
        }
        // Handled by the caller so the loop can exit.
        SessionCommand::Exit => {}
    }
}
