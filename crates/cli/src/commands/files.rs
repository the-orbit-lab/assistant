use orbit_actions::native::list_files;
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
        .execute(&ctx, list_files::NAME, ActionInput::empty(), &AlwaysDeny)
        .await;
    let output = result?;

    if global.json {
        print_json(&output.data);
        return Ok(());
    }

    for file in output.data["files"].as_array().cloned().unwrap_or_default() {
        println!("{}", file["path"].as_str().unwrap_or_default());
    }
    println!(
        "\n{} file(s)",
        output.data["count"].as_u64().unwrap_or_default()
    );
    Ok(())
}
