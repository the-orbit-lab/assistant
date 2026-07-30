use std::io::Write;
use std::sync::Arc;

use orbit_agent::Agent;
use orbit_core::OrbitError;
use orbit_providers::OllamaProvider;
use orbit_workspace::{DiscoveredRoot, ProjectRegistry};

use crate::args::GlobalArgs;
use crate::confirm::build_confirmation_provider;
use crate::output::print_sources;
use crate::resolve::{resolve_project, resolve_workspace};
use crate::runtime::build_context;

/// A basic interactive session: conversation messages, tool calls, action
/// results, and source references all live in `history` for the duration
/// of the process. Nothing here is written to disk — closing the session
/// discards it, per Orbit's "no silent persistence" rule for
/// conversations.
pub async fn run(global: &GlobalArgs) -> Result<(), OrbitError> {
    if global.project.is_none() {
        let root = discover_root(global)?;
        if let DiscoveredRoot::Workspace(_) = root {
            return run_workspace(global).await;
        }
    }
    run_single_project(global).await
}

fn discover_root(global: &GlobalArgs) -> Result<DiscoveredRoot, OrbitError> {
    if let Some(dir) = &global.workspace {
        return Ok(DiscoveredRoot::Workspace(
            orbit_workspace::workspace_paths_at(dir)?,
        ));
    }
    let cwd = std::env::current_dir().map_err(|e| OrbitError::io(".", e))?;
    orbit_workspace::discover(&cwd)
}

async fn run_single_project(global: &GlobalArgs) -> Result<(), OrbitError> {
    let loaded = resolve_project(global)?;
    let project_name = loaded.config.project.name.clone();
    let (registry, mcp_manager, warnings) = orbit_agent::build_registry(&loaded.config).await?;
    for warning in &warnings {
        eprintln!(
            "Warning: MCP server `{}`: {}",
            warning.server, warning.message
        );
    }

    let provider = Arc::new(OllamaProvider::new(
        loaded.config.model.endpoint.clone(),
        loaded.config.model.model.clone(),
    ));
    let confirmation = build_confirmation_provider(global.yes);
    let ctx = Arc::new(build_context(loaded));
    let agent = Agent::new(provider, Arc::new(registry), ctx, confirmation);

    println!("Orbit chat — project `{project_name}`. Type `exit` or press Ctrl-D to quit.");

    let mut history = Vec::new();
    let stdin = std::io::stdin();
    loop {
        print!("\n> ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        let bytes_read = stdin
            .read_line(&mut line)
            .map_err(|e| OrbitError::io(".", e))?;
        if bytes_read == 0 {
            println!();
            break;
        }
        let question = line.trim();
        if question.is_empty() {
            continue;
        }
        if question.eq_ignore_ascii_case("exit") || question.eq_ignore_ascii_case("quit") {
            break;
        }

        match agent.run(&mut history, question).await {
            Ok(outcome) => {
                println!("{}", outcome.answer);
                print_sources(&outcome.sources);
            }
            Err(err) => eprintln!("Error: {err}"),
        }
    }

    mcp_manager.shutdown().await;
    Ok(())
}

const SWITCH_PREFIXES: &[&str] = &["work in ", "work on ", "switch to ", "use the ", "use "];

/// Deterministic, not fuzzy: recognizes a fixed set of lead-in phrases and
/// then requires an exact registered name/alias in the remainder. Nothing
/// here infers intent from arbitrary phrasing.
fn detect_switch_target<'a>(question: &'a str, registry: &ProjectRegistry) -> Option<Vec<String>> {
    let lower = question.to_lowercase();
    let prefix = SWITCH_PREFIXES.iter().find(|p| lower.starts_with(*p))?;
    let remainder: &'a str = question[prefix.len()..].trim_end_matches(['.', '!']).trim();
    let mentions = orbit_workspace::retrieval::find_project_mentions(remainder, registry);
    if mentions.is_empty() {
        None
    } else {
        Some(mentions)
    }
}

async fn run_workspace(global: &GlobalArgs) -> Result<(), OrbitError> {
    let project_registry = resolve_workspace(global)?;
    let confirmation = build_confirmation_provider(global.yes);
    let registry = Arc::new(orbit_workspace::build_registry(
        project_registry.clone(),
        confirmation.clone(),
    )?);
    let action_ctx = Arc::new(project_registry.workspace_action_context());

    let model = global
        .model
        .clone()
        .unwrap_or_else(|| orbit_project::config::DEFAULT_OLLAMA_MODEL.to_string());
    let endpoint = global
        .ollama_endpoint
        .clone()
        .unwrap_or_else(|| orbit_project::config::DEFAULT_OLLAMA_ENDPOINT.to_string());
    let provider = Arc::new(OllamaProvider::new(endpoint, model));

    println!(
        "Orbit chat — workspace `{}`. Type `exit` or press Ctrl-D to quit.",
        project_registry.config.workspace.name
    );
    println!("Say e.g. \"work in obc\" to set the active project explicitly.");

    let mut history = Vec::new();
    let mut active_projects: Vec<String> = Vec::new();
    let stdin = std::io::stdin();

    loop {
        print!("\n> ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        let bytes_read = stdin
            .read_line(&mut line)
            .map_err(|e| OrbitError::io(".", e))?;
        if bytes_read == 0 {
            println!();
            break;
        }
        let question = line.trim();
        if question.is_empty() {
            continue;
        }
        if question.eq_ignore_ascii_case("exit") || question.eq_ignore_ascii_case("quit") {
            break;
        }

        if let Some(target) = detect_switch_target(question, &project_registry) {
            match project_registry.resolve_projects(&target) {
                Ok(_) => {
                    active_projects = target;
                    println!("Active project(s): {}", active_projects.join(", "));
                }
                Err(err) => eprintln!("Error: {err}"),
            }
            continue;
        }

        // A new mention while a session is already active adds to (rather
        // than replaces) the active set -- "work in X" replaces, naming Y
        // in a follow-up question extends the conversation to X and Y.
        let mentions =
            orbit_workspace::retrieval::find_project_mentions(question, &project_registry);
        let mut changed = false;
        for project in &mentions {
            if !active_projects.contains(project) {
                active_projects.push(project.clone());
                changed = true;
            }
        }
        if changed {
            println!("Active project(s): {}", active_projects.join(", "));
        }

        let explicit = if active_projects.is_empty() {
            None
        } else {
            Some(active_projects.clone())
        };

        if history.is_empty() {
            history.push(orbit_core::Message::system(
                orbit_agent::prompt::system_prompt(
                    &project_registry.config.workspace.name,
                    &project_registry.config.workspace.description,
                ),
            ));
        }
        history.push(orbit_core::Message::user(question));

        let (scope, retrieved_sources, retrieved_records) = orbit_workspace::retrieval::run(
            &registry,
            &action_ctx,
            &project_registry,
            confirmation.as_ref(),
            question,
            explicit.as_deref(),
            &mut history,
        )
        .await;
        let _ = retrieved_records;

        let agent = Agent::new(
            provider.clone(),
            registry.clone(),
            action_ctx.clone(),
            confirmation.clone(),
        )
        .with_builtin_overview_retrieval(false);
        match agent.continue_from_history(&mut history, question).await {
            Ok(mut outcome) => {
                let mut sources = retrieved_sources;
                sources.append(&mut outcome.sources);
                if scope.used_default {
                    println!("(using default project: {})", scope.projects.join(", "));
                }
                println!("{}", outcome.answer);
                print_sources(&sources);
            }
            Err(err) => eprintln!("Error: {err}"),
        }
    }

    Ok(())
}
