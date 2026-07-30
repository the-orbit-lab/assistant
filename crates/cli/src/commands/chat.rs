use std::io::Write;
use std::sync::Arc;

use orbit_agent::Agent;
use orbit_core::OrbitError;
use orbit_providers::OllamaProvider;

use crate::args::GlobalArgs;
use crate::confirm::build_confirmation_provider;
use crate::output::print_sources;
use crate::resolve::resolve_project;
use crate::runtime::build_context;

/// A basic interactive session: conversation messages, tool calls, action
/// results, and source references all live in `history` for the duration
/// of the process. Nothing here is written to disk — closing the session
/// discards it, per Orbit's "no silent persistence" rule for
/// conversations.
pub async fn run(global: &GlobalArgs) -> Result<(), OrbitError> {
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
