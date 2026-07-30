use std::sync::Arc;

use orbit_core::{ActionInput, AlwaysDeny, OrbitError};

use crate::args::{GlobalArgs, WorkspaceAction, WorkspaceArgs};
use crate::commands::workspace_init;
use crate::output::print_json;
use crate::resolve::resolve_workspace;

pub async fn run(global: &GlobalArgs, args: WorkspaceArgs) -> Result<(), OrbitError> {
    match args.action {
        Some(WorkspaceAction::Init(init_args)) => workspace_init::run(global, init_args),
        None => info(global).await,
    }
}

async fn info(global: &GlobalArgs) -> Result<(), OrbitError> {
    let project_registry = resolve_workspace(global)?;
    let registry = orbit_workspace::build_registry(project_registry.clone(), Arc::new(AlwaysDeny))?;
    let ctx = project_registry.workspace_action_context();
    let (_, result) = registry
        .execute(
            &ctx,
            orbit_workspace::native::information::NAME,
            ActionInput::empty(),
            &AlwaysDeny,
        )
        .await;
    let output = result?;

    if global.json {
        print_json(&output.data);
        return Ok(());
    }

    let data = &output.data;
    println!("Name:        {}", data["name"].as_str().unwrap_or_default());
    let description = data["description"].as_str().unwrap_or_default();
    if !description.is_empty() {
        println!("Description: {description}");
    }
    println!("Root:        {}", data["root"].as_str().unwrap_or_default());
    println!(
        "Config:      {}",
        data["config_path"].as_str().unwrap_or_default()
    );
    println!(
        "Default:     {}",
        data["default_project"]
            .as_str()
            .unwrap_or("(none configured)")
    );
    println!(
        "Projects:    {} registered, {} available",
        data["project_count"].as_u64().unwrap_or_default(),
        data["available_project_count"].as_u64().unwrap_or_default()
    );
    println!("Mode:        workspace");

    let unavailable = data["unavailable_projects"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !unavailable.is_empty() {
        println!("\nUnavailable projects:");
        for entry in unavailable {
            println!(
                "  {}: {}",
                entry["project"].as_str().unwrap_or_default(),
                entry["error"].as_str().unwrap_or_default()
            );
        }
    }

    let relationships = data["relationships"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !relationships.is_empty() {
        println!("\nRelationships:");
        for r in relationships {
            println!(
                "  {} --{}--> {}",
                r["source"].as_str().unwrap_or_default(),
                r["type"].as_str().unwrap_or_default(),
                r["target"].as_str().unwrap_or_default()
            );
        }
    }

    Ok(())
}
