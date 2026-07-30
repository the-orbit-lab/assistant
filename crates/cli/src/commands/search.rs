use orbit_actions::native::search;
use orbit_core::{ActionInput, AlwaysDeny, OrbitError};
use serde_json::json;

use crate::args::{GlobalArgs, SearchArgs};
use crate::output::print_json;
use crate::resolve::{resolve_project, resolve_workspace};
use crate::runtime::build_context;

pub async fn run(global: &GlobalArgs, args: SearchArgs) -> Result<(), OrbitError> {
    if !args.projects.is_empty() {
        return run_multi_project(global, args).await;
    }

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

async fn run_multi_project(global: &GlobalArgs, args: SearchArgs) -> Result<(), OrbitError> {
    let project_registry = resolve_workspace(global)?;
    // Validate every requested project up front so a typo fails clearly
    // instead of silently searching a subset.
    project_registry.resolve_projects(&args.projects)?;

    let registry =
        orbit_workspace::build_registry(project_registry.clone(), std::sync::Arc::new(AlwaysDeny))?;
    let ctx = project_registry.workspace_action_context();
    let input = ActionInput(json!({
        "projects": args.projects,
        "query": args.query,
        "limit_per_project": args.limit.min(orbit_workspace::budget::MAX_RESULTS_PER_PROJECT),
    }));
    let (_, result) = registry
        .execute(
            &ctx,
            orbit_workspace::native::search::NAME,
            input,
            &AlwaysDeny,
        )
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
        println!(
            "No results for `{}` in {}.",
            args.query,
            args.projects.join(", ")
        );
    }
    let mut last_project: Option<String> = None;
    for entry in &results {
        let project = entry["project"].as_str().unwrap_or_default();
        if last_project.as_deref() != Some(project) {
            println!("\nProject: {project}");
            last_project = Some(project.to_string());
        }
        println!("Path: {}", entry["path"].as_str().unwrap_or_default());
        match (entry["line_start"].as_u64(), entry["line_end"].as_u64()) {
            (Some(start), Some(end)) if start == end => println!("Lines: {start}"),
            (Some(start), Some(end)) => println!("Lines: {start}-{end}"),
            _ => {}
        }
        if let Some(section) = entry["section"].as_str() {
            println!("Section: {section}");
        }
        println!("    {}", entry["excerpt"].as_str().unwrap_or_default());
    }

    for unavailable in output.data["unavailable_projects"]
        .as_array()
        .cloned()
        .unwrap_or_default()
    {
        eprintln!(
            "Warning: project `{}` was skipped: {}",
            unavailable["project"].as_str().unwrap_or_default(),
            unavailable["error"].as_str().unwrap_or_default()
        );
    }

    println!(
        "\n{} result(s) across {} project(s)",
        results.len(),
        args.projects.len()
    );
    Ok(())
}
