use std::path::PathBuf;

use orbit_core::OrbitError;
use orbit_workspace::config::starter_yaml;

use crate::args::{GlobalArgs, WorkspaceInitArgs};

/// `orbit workspace init`: registers immediate child directories that
/// already contain `.orbit/project.yaml`. Never descends further, and
/// never registers a directory that isn't already an Orbit project --
/// initializing a workspace must not silently turn an unrelated sibling
/// repository into a registered project.
pub fn run(global: &GlobalArgs, args: WorkspaceInitArgs) -> Result<(), OrbitError> {
    let target_dir = match &global.workspace {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().map_err(|e| OrbitError::io(".", e))?,
    };
    let orbit_dir = target_dir.join(".orbit");
    let config_path = orbit_dir.join("workspace.yaml");

    if config_path.exists() && !args.force {
        return Err(OrbitError::ConfigAlreadyExists { path: config_path });
    }

    let (registered, skipped) = scan_children(&target_dir)?;

    std::fs::create_dir_all(&orbit_dir).map_err(|e| OrbitError::io(&orbit_dir, e))?;
    let name = args.name.unwrap_or_else(|| {
        target_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "workspace".to_string())
    });
    let yaml = starter_yaml(&name, &args.description, &registered);
    std::fs::write(&config_path, yaml).map_err(|e| OrbitError::io(&config_path, e))?;

    println!("Created {}", config_path.display());
    if registered.is_empty() {
        println!("No child directories with `.orbit/project.yaml` were found to register.");
    } else {
        println!("Registered projects: {}", registered.join(", "));
    }
    if !skipped.is_empty() {
        println!(
            "Skipped (no `.orbit/project.yaml`, not registered automatically): {}",
            skipped.join(", ")
        );
    }
    println!("Edit it to add aliases, descriptions, relationships, and a default project.");
    Ok(())
}

/// Immediate children only, alphabetically, so results are deterministic.
/// Hidden directories (`.git`, `.orbit`, ...) are never candidates.
fn scan_children(target_dir: &std::path::Path) -> Result<(Vec<String>, Vec<String>), OrbitError> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(target_dir)
        .map_err(|e| OrbitError::io(target_dir, e))?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            !path
                .file_name()
                .map(|n| n.to_string_lossy().starts_with('.'))
                .unwrap_or(true)
        })
        .collect();
    dirs.sort();

    let mut registered = Vec::new();
    let mut skipped = Vec::new();
    for dir in dirs {
        let name = dir.file_name().unwrap().to_string_lossy().to_string();
        if dir.join(".orbit").join("project.yaml").is_file() {
            registered.push(name);
        } else {
            skipped.push(name);
        }
    }
    Ok((registered, skipped))
}
