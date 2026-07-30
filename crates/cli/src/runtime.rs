use orbit_actions::ActionContext;

use crate::resolve::Loaded;

pub fn build_context(loaded: Loaded) -> ActionContext {
    ActionContext {
        root: loaded.paths.root,
        config_path: loaded.paths.config_path,
        config: loaded.config,
    }
}
