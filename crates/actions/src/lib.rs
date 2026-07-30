//! The Action Runtime: a provider-independent, protocol-independent
//! registry of executable capabilities, plus the six native Orbit actions.
//!
//! Nothing in this crate knows about Ollama or MCP. The agent calls
//! [`ActionRegistry::execute`] directly; the MCP server wraps the same
//! registry behind a protocol adapter without adding execution logic of its
//! own.

pub mod native;
pub mod registry;

pub use registry::{Action, ActionContext, ActionRegistry};

/// Build a registry with every native action registered.
pub fn native_registry() -> Result<ActionRegistry, orbit_core::OrbitError> {
    let mut registry = ActionRegistry::new();
    native::register_all(&mut registry)?;
    Ok(registry)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;

    use orbit_project::ProjectConfig;

    use crate::registry::ActionContext;

    pub fn context(root: &Path, yaml: &str) -> ActionContext {
        let config = ProjectConfig::parse(yaml).expect("valid test config");
        ActionContext {
            root: root.to_path_buf(),
            config_path: root.join(".orbit/project.yaml"),
            config,
        }
    }

    pub fn default_context(root: &Path) -> ActionContext {
        context(
            root,
            "version: 1\nproject:\n  name: demo\ncontext:\n  include:\n    - \"**/*\"\n",
        )
    }
}
