pub mod command_list;
pub mod command_run;
pub mod information;
pub mod list_files;
pub mod read_file;
pub mod search;

use std::sync::Arc;

use orbit_core::OrbitError;

use crate::registry::ActionRegistry;

/// Register every native Orbit action. `project.information` is registered
/// last so it can report a snapshot of every other action's descriptor.
pub fn register_all(registry: &mut ActionRegistry) -> Result<(), OrbitError> {
    registry.register(Arc::new(list_files::ListFilesAction))?;
    registry.register(Arc::new(read_file::ReadFileAction))?;
    registry.register(Arc::new(search::SearchAction))?;
    registry.register(Arc::new(command_list::CommandListAction))?;
    registry.register(Arc::new(command_run::RunConfiguredCommandAction))?;

    let known_actions = registry.descriptors();
    registry.register(Arc::new(information::InformationAction::new(known_actions)))?;
    Ok(())
}
