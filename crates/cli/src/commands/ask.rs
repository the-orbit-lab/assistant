use std::sync::Arc;

use orbit_agent::Agent;
use orbit_core::OrbitError;
use orbit_providers::OllamaProvider;
use orbit_workspace::DiscoveredRoot;
use serde_json::json;

use crate::args::{AskArgs, GlobalArgs};
use crate::confirm::build_confirmation_provider;
use crate::output::{print_json, print_sources};
use crate::resolve::{resolve_project, resolve_workspace};
use crate::runtime::build_context;

pub async fn run(global: &GlobalArgs, args: AskArgs) -> Result<(), OrbitError> {
    if !args.projects.is_empty() {
        return run_workspace(global, &args.question, Some(args.projects)).await;
    }
    if global.project.is_some() {
        // A specific project was named explicitly (name, alias, or path):
        // this is exactly single-project `ask`, unchanged, whether or not
        // a workspace happens to be involved in resolving that name.
        return run_single_project(global, &args.question).await;
    }

    match discover_root(global)? {
        DiscoveredRoot::Project(_) => run_single_project(global, &args.question).await,
        DiscoveredRoot::Workspace(_) => run_workspace(global, &args.question, None).await,
    }
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

async fn run_single_project(global: &GlobalArgs, question: &str) -> Result<(), OrbitError> {
    let loaded = resolve_project(global)?;
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
    let mut history = Vec::new();
    let outcome = agent.run(&mut history, question).await;
    mcp_manager.shutdown().await;
    let outcome = outcome?;

    if global.json {
        print_json(&json!({
            "answer": outcome.answer,
            "sources": outcome.sources,
        }));
        return Ok(());
    }

    println!("{}", outcome.answer);
    print_sources(&outcome.sources);
    Ok(())
}

async fn run_workspace(
    global: &GlobalArgs,
    question: &str,
    explicit_projects: Option<Vec<String>>,
) -> Result<(), OrbitError> {
    let project_registry = resolve_workspace(global)?;
    if let Some(explicit) = &explicit_projects {
        // Fail fast and clearly on an unknown project rather than
        // depending on the model to notice.
        project_registry.resolve_projects(explicit)?;
    }

    let confirmation = build_confirmation_provider(global.yes);
    let registry = Arc::new(orbit_workspace::build_registry(
        project_registry.clone(),
        confirmation.clone(),
    )?);
    let action_ctx = project_registry.workspace_action_context();

    let mut history = vec![
        orbit_core::Message::system(orbit_agent::prompt::system_prompt(
            &project_registry.config.workspace.name,
            &project_registry.config.workspace.description,
        )),
        orbit_core::Message::user(question),
    ];
    let (scope, sources, records) = orbit_workspace::retrieval::run(
        &registry,
        &action_ctx,
        &project_registry,
        confirmation.as_ref(),
        question,
        explicit_projects.as_deref(),
        &mut history,
        // `orbit ask` is a one-shot command with no event consumer; a
        // null emitter keeps the retrieval path identical either way.
        &orbit_core::EventEmitter::null(),
    )
    .await;

    let model = global
        .model
        .clone()
        .unwrap_or_else(|| orbit_project::config::DEFAULT_OLLAMA_MODEL.to_string());
    let endpoint = global
        .ollama_endpoint
        .clone()
        .unwrap_or_else(|| orbit_project::config::DEFAULT_OLLAMA_ENDPOINT.to_string());
    let provider = Arc::new(OllamaProvider::new(endpoint, model));

    let agent = Agent::new(provider, registry, Arc::new(action_ctx), confirmation)
        .with_builtin_overview_retrieval(false);

    let mut agent_sources = sources;
    let mut agent_records = records;
    let outcome = agent.continue_from_history(&mut history, question).await?;
    agent_sources.extend(outcome.sources);
    agent_records.extend(outcome.records);

    if global.json {
        print_json(&json!({
            "answer": outcome.answer,
            "scope": scope.projects,
            "used_default_project": scope.used_default,
            "sources": agent_sources,
        }));
        return Ok(());
    }

    if scope.projects.is_empty() {
        println!("(workspace-level: no specific project selected)\n");
    } else if scope.used_default {
        println!("(using default project: {})\n", scope.projects.join(", "));
    } else {
        println!("Active project(s): {}\n", scope.projects.join(", "));
    }

    println!("{}", outcome.answer);
    print_sources(&agent_sources);
    Ok(())
}
