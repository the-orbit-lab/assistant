use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::permission::Permission;
use crate::source::SourceReference;

/// The static description of an action: what the Action Registry lists, what
/// gets exported through MCP, and what the model sees as a callable tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionDescriptor {
    /// Stable, namespaced name, e.g. `project.read_file`. Doubles as the key
    /// looked up in `.orbit/project.yaml`'s `permissions` map.
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// The permission applied when the project configuration does not list
    /// this action explicitly. Actions declare this conservatively; it is
    /// never a way around configuration — an explicit `permissions` entry
    /// always wins.
    pub default_permission: Permission,
}

/// Raw, validated-by-the-action input.
#[derive(Debug, Clone)]
pub struct ActionInput(pub Value);

impl ActionInput {
    pub fn empty() -> Self {
        ActionInput(Value::Object(Default::default()))
    }
}

/// The structured result of a successful action execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionOutput {
    pub data: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<SourceReference>,
}

impl ActionOutput {
    pub fn new(data: Value) -> Self {
        Self {
            data,
            sources: Vec::new(),
        }
    }

    pub fn with_sources(mut self, sources: Vec<SourceReference>) -> Self {
        self.sources = sources;
        self
    }

    /// Compact JSON rendering handed back to the model as a tool-result
    /// message body.
    pub fn to_model_text(&self) -> String {
        serde_json::to_string(&self.data).unwrap_or_else(|_| "{}".to_string())
    }
}
