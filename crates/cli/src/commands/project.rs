use orbit_actions::native::information;
use orbit_core::{ActionInput, AlwaysDeny, OrbitError};

use crate::args::GlobalArgs;
use crate::output::print_json;
use crate::resolve::resolve_project;
use crate::runtime::build_context;

pub async fn run(global: &GlobalArgs) -> Result<(), OrbitError> {
    let loaded = resolve_project(global)?;
    let ctx = build_context(loaded);
    let registry = orbit_actions::native_registry()?;
    let (_, result) = registry
        .execute(&ctx, information::NAME, ActionInput::empty(), &AlwaysDeny)
        .await;
    let output = result?;

    if global.json {
        print_json(&output.data);
        return Ok(());
    }

    let data = &output.data;
    println!("Name:        {}", data["name"].as_str().unwrap_or_default());
    println!("Type:        {}", data["type"].as_str().unwrap_or_default());
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
        "Provider:    {} ({} @ {})",
        data["provider"].as_str().unwrap_or_default(),
        data["model"].as_str().unwrap_or_default(),
        data["endpoint"].as_str().unwrap_or_default()
    );
    println!(
        "Files:       {}",
        data["discovered_file_count"].as_u64().unwrap_or_default()
    );

    println!("\nCommands:");
    let commands = data["commands"].as_array().cloned().unwrap_or_default();
    if commands.is_empty() {
        println!("  (none configured)");
    }
    for command in commands {
        let args: Vec<String> = command["args"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        println!(
            "  {}: {} {}",
            command["name"].as_str().unwrap_or_default(),
            command["program"].as_str().unwrap_or_default(),
            args.join(" ")
        );
    }

    println!("\nPermissions:");
    for entry in data["permissions"].as_array().cloned().unwrap_or_default() {
        println!(
            "  {}: {}",
            entry["action"].as_str().unwrap_or_default(),
            entry["permission"].as_str().unwrap_or_default()
        );
    }

    let mcp_servers = data["mcp_servers"].as_array().cloned().unwrap_or_default();
    println!(
        "\nMCP servers: {}",
        if mcp_servers.is_empty() {
            "(none configured)".to_string()
        } else {
            mcp_servers
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        }
    );

    Ok(())
}
