use orbit_actions::native::command_run;
use orbit_core::{ActionInput, OrbitError};
use serde_json::json;

use crate::args::{GlobalArgs, RunArgs};
use crate::confirm::build_confirmation_provider;
use crate::output::print_json;
use crate::resolve::resolve_project_with_mode;
use crate::runtime::build_context;

pub async fn run(global: &GlobalArgs, args: RunArgs) -> Result<(), OrbitError> {
    // Commands never fall back to a workspace's default project: at a
    // workspace root with no explicit --project, this must ask for one
    // rather than silently running somewhere the caller didn't choose.
    let loaded = resolve_project_with_mode(global, true)?;
    let ctx = build_context(loaded);
    let registry = orbit_actions::native_registry()?;
    let confirmation = build_confirmation_provider(global.yes);

    let input = ActionInput(json!({ "name": args.name }));
    let (_, result) = registry
        .execute(&ctx, command_run::NAME, input, confirmation.as_ref())
        .await;
    let output = result?;

    if global.json {
        print_json(&output.data);
    } else {
        if let Some(stdout) = output.data["stdout"].as_str().filter(|s| !s.is_empty()) {
            print!("{stdout}");
        }
        if let Some(stderr) = output.data["stderr"].as_str().filter(|s| !s.is_empty()) {
            eprint!("{stderr}");
        }
        let success = output.data["success"].as_bool().unwrap_or(false);
        println!(
            "\n`{}` exited {} ({}ms)",
            args.name,
            if success {
                "successfully"
            } else {
                "with a failure"
            },
            output.data["duration_ms"].as_u64().unwrap_or_default()
        );
    }

    if !output.data["success"].as_bool().unwrap_or(false) {
        std::process::exit(1);
    }
    Ok(())
}
