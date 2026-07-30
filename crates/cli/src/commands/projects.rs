use std::sync::Arc;

use orbit_core::{ActionInput, AlwaysDeny, OrbitError};

use crate::args::GlobalArgs;
use crate::output::print_json;
use crate::resolve::resolve_workspace;

pub async fn run(global: &GlobalArgs) -> Result<(), OrbitError> {
    let project_registry = resolve_workspace(global)?;
    let registry = orbit_workspace::build_registry(project_registry.clone(), Arc::new(AlwaysDeny))?;
    let ctx = project_registry.workspace_action_context();
    let (_, result) = registry
        .execute(
            &ctx,
            orbit_workspace::native::list_projects::NAME,
            ActionInput::empty(),
            &AlwaysDeny,
        )
        .await;
    let output = result?;

    if global.json {
        print_json(&output.data);
        return Ok(());
    }

    let projects = output.data["projects"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for project in &projects {
        let status = if project["available"].as_bool().unwrap_or(false) {
            "available"
        } else {
            "unavailable"
        };
        let aliases: Vec<&str> = project["aliases"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        println!(
            "{} [{status}] path={} aliases={}",
            project["name"].as_str().unwrap_or_default(),
            project["path"].as_str().unwrap_or_default(),
            if aliases.is_empty() {
                "(none)".to_string()
            } else {
                aliases.join(", ")
            }
        );
        let description = project["description"].as_str().unwrap_or_default();
        if !description.is_empty() {
            println!("    {description}");
        }
        if let Some(error) = project["error"].as_str() {
            println!("    error: {error}");
        }
    }
    println!("\n{} project(s)", projects.len());
    Ok(())
}
