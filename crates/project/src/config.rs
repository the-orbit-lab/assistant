use std::collections::BTreeMap;
use std::path::Path;

use orbit_core::{OrbitError, Permission};
use serde::{Deserialize, Serialize};

pub const SUPPORTED_CONFIG_VERSION: u32 = 1;
pub const DEFAULT_OLLAMA_ENDPOINT: &str = "http://localhost:11434";
pub const DEFAULT_OLLAMA_MODEL: &str = "qwen2.5:latest";

/// Exclude patterns applied regardless of what the project configures.
/// Exclude always wins over include, and these are never overridable by
/// `context.include` — they exist so a misconfigured project can't
/// accidentally expose VCS internals, Orbit's own config, or common secret
/// file shapes to the model.
pub const MANDATORY_EXCLUDES: &[&str] = &[
    ".git/**",
    ".orbit/**",
    "target/**",
    "node_modules/**",
    ".env",
    ".env.*",
    "secrets/**",
    "**/*.key",
    "**/*.pem",
];

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectConfig {
    pub version: u32,
    pub project: ProjectMeta,
    #[serde(default)]
    pub model: ModelConfig,
    #[serde(default)]
    pub context: ContextConfig,
    #[serde(default)]
    pub commands: BTreeMap<String, CommandDef>,
    #[serde(default)]
    pub permissions: BTreeMap<String, Permission>,
    #[serde(default)]
    pub mcp: McpConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMeta {
    pub name: String,
    #[serde(rename = "type", default = "default_project_type")]
    pub project_type: String,
    #[serde(default)]
    pub description: String,
}

fn default_project_type() -> String {
    "software".to_string()
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ModelConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_model")]
    pub model: String,
    #[serde(default = "default_endpoint")]
    pub endpoint: String,
}

fn default_provider() -> String {
    "ollama".to_string()
}
fn default_model() -> String {
    DEFAULT_OLLAMA_MODEL.to_string()
}
fn default_endpoint() -> String {
    DEFAULT_OLLAMA_ENDPOINT.to_string()
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            model: default_model(),
            endpoint: default_endpoint(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContextConfig {
    #[serde(default = "default_include")]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
}

fn default_include() -> Vec<String> {
    vec![
        "README.md".to_string(),
        // Any other root-level instructions/overview doc (CLAUDE.md,
        // CONTRIBUTING.md, ...). Without this, a file like CLAUDE.md is
        // invisible to every action and to grounded answers even though
        // it plainly describes the project.
        "*.md".to_string(),
        "Cargo.toml".to_string(),
        "docs/**".to_string(),
        "src/**".to_string(),
        "tests/**".to_string(),
    ]
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            include: default_include(),
            exclude: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandDef {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
pub struct McpConfig {
    #[serde(default)]
    pub expose: Vec<String>,
    #[serde(default)]
    pub servers: BTreeMap<String, McpServerConfig>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum McpTransport {
    Stdio,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct McpServerConfig {
    pub transport: McpTransport,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl ProjectConfig {
    pub fn load(path: &Path) -> Result<Self, OrbitError> {
        let raw = std::fs::read_to_string(path).map_err(|e| OrbitError::io(path, e))?;
        Self::parse(&raw).map_err(|reason| OrbitError::ConfigInvalid {
            path: path.to_path_buf(),
            reason,
        })
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        let config: ProjectConfig =
            serde_norway::from_str(raw).map_err(|e| format!("YAML parse error: {e}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.version != SUPPORTED_CONFIG_VERSION {
            return Err(format!(
                "unsupported configuration version {} (expected {})",
                self.version, SUPPORTED_CONFIG_VERSION
            ));
        }
        if self.project.name.trim().is_empty() {
            return Err("project.name must not be empty".to_string());
        }
        if self.project.project_type.trim().is_empty() {
            return Err("project.type must not be empty".to_string());
        }
        if self.model.provider.trim().is_empty() {
            return Err("model.provider must not be empty".to_string());
        }
        if self.model.model.trim().is_empty() {
            return Err("model.model must not be empty".to_string());
        }
        for (name, def) in &self.commands {
            if name.trim().is_empty() {
                return Err("command names must not be empty".to_string());
            }
            if def.program.trim().is_empty() {
                return Err(format!("command `{name}` has an empty `program`"));
            }
        }
        for key in self.permissions.keys() {
            if key.trim().is_empty() {
                return Err("permission keys must not be empty".to_string());
            }
        }
        for name in &self.mcp.expose {
            if name.trim().is_empty() {
                return Err("mcp.expose entries must not be empty".to_string());
            }
        }
        for (name, server) in &self.mcp.servers {
            if name.trim().is_empty() {
                return Err("mcp.servers keys must not be empty".to_string());
            }
            if server.command.trim().is_empty() {
                return Err(format!("mcp server `{name}` has an empty `command`"));
            }
        }
        for pattern in self.context.include.iter().chain(&self.context.exclude) {
            if pattern.trim().is_empty() {
                return Err("context include/exclude patterns must not be empty".to_string());
            }
            if globset::Glob::new(pattern).is_err() {
                return Err(format!("invalid glob pattern `{pattern}`"));
            }
        }
        Ok(())
    }

    /// The effective permission for a named action: an explicit
    /// configuration entry always wins over the action's own default.
    pub fn effective_permission(&self, action_name: &str, default: Permission) -> Permission {
        self.permissions
            .get(action_name)
            .copied()
            .unwrap_or(default)
    }

    /// All exclude patterns that apply, combining the mandatory,
    /// non-overridable set with the project's own configuration.
    pub fn effective_excludes(&self) -> Vec<String> {
        let mut excludes: Vec<String> = MANDATORY_EXCLUDES.iter().map(|s| s.to_string()).collect();
        excludes.extend(self.context.exclude.iter().cloned());
        excludes
    }
}

/// Render `s` as a double-quoted YAML scalar so arbitrary user-supplied
/// text (a project name or description containing `:`, `#`, or quotes)
/// can never corrupt the generated document, and so an empty string is
/// written unambiguously rather than as a bare, null-parsing value.
fn yaml_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A documented starter configuration written by `orbit init`.
pub fn starter_yaml(name: &str, project_type: &str, description: &str) -> String {
    let name = yaml_quote(name);
    let project_type = yaml_quote(project_type);
    let description = yaml_quote(description);
    format!(
        r#"version: 1

project:
  name: {name}
  type: {project_type}
  description: {description}

model:
  # Orbit's first-class provider is a local Ollama instance.
  provider: ollama
  model: {model}
  endpoint: {endpoint}

context:
  # Files Orbit is allowed to read, search, and use as answer sources.
  include:
    - README.md
    - "*.md"       # any other root-level doc, e.g. CLAUDE.md
    - Cargo.toml
    - docs/**
    - src/**
    - tests/**

  # Exclude always wins over include. A mandatory set (.git, .orbit, secrets,
  # *.key, *.pem, .env*, target, node_modules) is enforced in addition to
  # this list and cannot be disabled from this file.
  exclude:
    - target/**
    - .git/**
    - .env
    - .env.*
    - secrets/**
    - "**/*.key"
    - "**/*.pem"

commands:
  build:
    program: cargo
    args:
      - build

  test:
    program: cargo
    args:
      - test

  lint:
    program: cargo
    args:
      - clippy
      - --workspace
      - --all-targets
      - --all-features
      - --
      - -D
      - warnings

  format:
    program: cargo
    args:
      - fmt
      - --check

permissions:
  project.information: allow
  project.list_files: allow
  project.read_file: allow
  project.search: allow
  command.list: allow
  command.run_configured: ask

mcp:
  # Native actions exported to external MCP hosts (e.g. Claude Code).
  # Nothing is exported unless listed here.
  expose:
    - project.information
    - project.search
    - project.read_file

  # External stdio MCP servers Orbit itself may consume, namespaced as
  # mcp.<server>.<tool>. Empty by default.
  servers: {{}}
"#,
        name = name,
        project_type = project_type,
        description = description,
        model = DEFAULT_OLLAMA_MODEL,
        endpoint = DEFAULT_OLLAMA_ENDPOINT,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starter_yaml_round_trips() {
        let yaml = starter_yaml("demo", "software", "A demo project");
        let config = ProjectConfig::parse(&yaml).expect("starter config must be valid");
        assert_eq!(config.project.name, "demo");
        assert_eq!(config.version, SUPPORTED_CONFIG_VERSION);
    }

    #[test]
    fn starter_yaml_handles_empty_description_as_empty_string_not_null() {
        let yaml = starter_yaml("demo", "software", "");
        let config = ProjectConfig::parse(&yaml).expect("starter config must be valid");
        assert_eq!(config.project.description, "");
    }

    #[test]
    fn starter_yaml_quotes_values_with_special_yaml_characters() {
        let yaml = starter_yaml("demo: weird", "software", "Has \"quotes\" and # a hash");
        let config = ProjectConfig::parse(&yaml).expect("starter config must be valid");
        assert_eq!(config.project.name, "demo: weird");
        assert_eq!(config.project.description, "Has \"quotes\" and # a hash");
    }

    #[test]
    fn rejects_unsupported_version() {
        let yaml = "version: 2\nproject:\n  name: demo\n";
        let err = ProjectConfig::parse(yaml).unwrap_err();
        assert!(err.contains("unsupported configuration version"));
    }

    #[test]
    fn rejects_invalid_permission_value() {
        let yaml = "version: 1\nproject:\n  name: demo\npermissions:\n  project.read_file: maybe\n";
        assert!(ProjectConfig::parse(yaml).is_err());
    }

    #[test]
    fn rejects_empty_command_program() {
        let yaml = "version: 1\nproject:\n  name: demo\ncommands:\n  build:\n    program: \"\"\n";
        let err = ProjectConfig::parse(yaml).unwrap_err();
        assert!(err.contains("empty `program`"));
    }

    #[test]
    fn rejects_unknown_top_level_field() {
        let yaml = "version: 1\nproject:\n  name: demo\nbogus: true\n";
        assert!(ProjectConfig::parse(yaml).is_err());
    }

    #[test]
    fn exclude_precedence_is_mandatory_plus_configured() {
        let yaml = "version: 1\nproject:\n  name: demo\ncontext:\n  exclude:\n    - custom/**\n";
        let config = ProjectConfig::parse(yaml).unwrap();
        let excludes = config.effective_excludes();
        assert!(excludes.contains(&".git/**".to_string()));
        assert!(excludes.contains(&"custom/**".to_string()));
    }

    #[test]
    fn effective_permission_falls_back_to_default() {
        let yaml = "version: 1\nproject:\n  name: demo\n";
        let config = ProjectConfig::parse(yaml).unwrap();
        assert_eq!(
            config.effective_permission("project.read_file", Permission::Ask),
            Permission::Ask
        );
    }

    #[test]
    fn effective_permission_prefers_explicit_config() {
        let yaml = "version: 1\nproject:\n  name: demo\npermissions:\n  project.read_file: deny\n";
        let config = ProjectConfig::parse(yaml).unwrap();
        assert_eq!(
            config.effective_permission("project.read_file", Permission::Allow),
            Permission::Deny
        );
    }
}
