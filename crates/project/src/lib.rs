//! Project configuration, discovery, security, and local search.
//!
//! This crate owns everything that touches the filesystem on the project's
//! behalf: loading and validating `.orbit/project.yaml`, locating the
//! project root, enforcing the path/exclude security boundary, and running
//! deterministic local search. It has no knowledge of actions, providers,
//! or MCP.

pub mod config;
pub mod discovery;
pub mod lexical;
pub mod query;
pub mod read;
pub mod root;
pub mod search;
pub mod security;

pub use config::{
    CommandDef, ContextConfig, McpConfig, McpServerConfig, McpTransport, ModelConfig,
    ProjectConfig, ProjectMeta,
};
pub use discovery::{DEFAULT_MAX_TEXT_FILE_BYTES, DiscoveredFile, discover_files};
pub use lexical::{content_terms, is_reference_word, is_stopword, stem, tokenize};
pub use query::{AnalyzedQuery, analyze, analyze_with_context};
pub use read::{read_allowed_file, read_allowed_file_truncated};
pub use root::{discover_project_root, project_paths_at};
pub use search::{LexicalIndex, SearchOptions, SearchResult, search_files};
