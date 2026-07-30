use std::sync::Arc;

use orbit_agent::Agent;
use orbit_core::OrbitError;
use orbit_providers::OllamaProvider;
use serde_json::json;

use crate::args::{AskArgs, GlobalArgs};
use crate::confirm::build_confirmation_provider;
use crate::output::{print_json, print_sources};
use crate::resolve::resolve_project;
use crate::runtime::build_context;

pub async fn run(global: &GlobalArgs, args: AskArgs) -> Result<(), OrbitError> {
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
    let outcome = agent.run(&mut history, &args.question).await;
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
