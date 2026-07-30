use orbit_actions::native::search;
use orbit_core::{ActionInput, AlwaysDeny, OrbitError};
use serde_json::json;

use crate::args::{GlobalArgs, SearchArgs};
use crate::output::print_json;
use crate::resolve::resolve_project;
use crate::runtime::build_context;

pub async fn run(global: &GlobalArgs, args: SearchArgs) -> Result<(), OrbitError> {
    let loaded = resolve_project(global)?;
    let ctx = build_context(loaded);
    let registry = orbit_actions::native_registry()?;
    let input = ActionInput(json!({ "query": args.query, "limit": args.limit }));
    let (_, result) = registry
        .execute(&ctx, search::NAME, input, &AlwaysDeny)
        .await;
    let output = result?;

    if global.json {
        print_json(&output.data);
        return Ok(());
    }

    let results = output.data["results"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if results.is_empty() {
        println!("No results for `{}`.", args.query);
        return Ok(());
    }
    for entry in &results {
        let path = entry["path"].as_str().unwrap_or_default();
        let line_start = entry["line_start"].as_u64();
        let location = match line_start {
            Some(line) => format!("{path}:{line}"),
            None => path.to_string(),
        };
        let section = entry["section"].as_str();
        match section {
            Some(section) => println!("{location} ({section})"),
            None => println!("{location}"),
        }
        println!("    {}", entry["excerpt"].as_str().unwrap_or_default());
    }
    println!("\n{} result(s)", results.len());
    Ok(())
}
