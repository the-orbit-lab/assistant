use orbit_actions::native::command_list;
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
        .execute(&ctx, command_list::NAME, ActionInput::empty(), &AlwaysDeny)
        .await;
    let output = result?;

    if global.json {
        print_json(&output.data);
        return Ok(());
    }

    let commands = output.data["commands"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if commands.is_empty() {
        println!("No commands configured.");
        return Ok(());
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
            "{} [{}]: {} {}",
            command["name"].as_str().unwrap_or_default(),
            command["permission"].as_str().unwrap_or_default(),
            command["program"].as_str().unwrap_or_default(),
            args.join(" ")
        );
    }
    Ok(())
}
