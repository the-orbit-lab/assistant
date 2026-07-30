use orbit_core::OrbitError;
use orbit_project::config::starter_yaml;

use crate::args::{GlobalArgs, InitArgs};

pub fn run(global: &GlobalArgs, args: InitArgs) -> Result<(), OrbitError> {
    let target_dir = match &global.project {
        Some(dir) => dir.clone(),
        None => std::env::current_dir().map_err(|e| OrbitError::io(".", e))?,
    };
    let orbit_dir = target_dir.join(".orbit");
    let config_path = orbit_dir.join("project.yaml");

    if config_path.exists() && !args.force {
        return Err(OrbitError::ConfigAlreadyExists { path: config_path });
    }

    std::fs::create_dir_all(&orbit_dir).map_err(|e| OrbitError::io(&orbit_dir, e))?;

    let name = args.name.unwrap_or_else(|| {
        target_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".to_string())
    });

    let yaml = starter_yaml(&name, &args.r#type, &args.description);
    std::fs::write(&config_path, yaml).map_err(|e| OrbitError::io(&config_path, e))?;

    println!("Created {}", config_path.display());
    println!("Edit it to adjust context, commands, and permissions, then run `orbit doctor`.");
    Ok(())
}
