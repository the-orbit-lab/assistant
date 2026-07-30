use std::path::PathBuf;

/// Structured, user-facing errors for every Orbit layer.
///
/// Every variant renders a message that can be shown directly to the user
/// without additional context, per the project's "understandable errors"
/// requirement.
#[derive(Debug, thiserror::Error)]
pub enum OrbitError {
    #[error(
        "Project configuration was not found under {searched_from}.\nRun `orbit init` to create `.orbit/project.yaml`."
    )]
    ConfigNotFound { searched_from: PathBuf },

    #[error("Project configuration at {path} is invalid: {reason}")]
    ConfigInvalid { path: PathBuf, reason: String },

    #[error(
        "Refusing to use the project configuration at {parent} because a more specific one exists at {child}. Run Orbit from within the {child} project or remove the ambiguous configuration."
    )]
    AmbiguousProjectRoot { parent: PathBuf, child: PathBuf },

    #[error("{path} already exists. Re-run with --force to overwrite it.")]
    ConfigAlreadyExists { path: PathBuf },

    #[error("The requested path `{path}` is outside the configured project root.")]
    PathOutsideProject { path: PathBuf },

    #[error("The path `{path}` is excluded by the project configuration.")]
    PathExcluded { path: PathBuf },

    #[error("The path `{path}` does not exist.")]
    PathNotFound { path: PathBuf },

    #[error("The path `{path}` is a symlink that escapes the project root.")]
    SymlinkEscape { path: PathBuf },

    #[error(
        "The file `{path}` is {size} bytes, which exceeds the configured limit of {limit} bytes."
    )]
    FileTooLarge {
        path: PathBuf,
        size: u64,
        limit: u64,
    },

    #[error("The file `{path}` is not valid UTF-8 text and cannot be read as text context.")]
    NotUtf8Text { path: PathBuf },

    #[error("Unknown action `{name}`.")]
    UnknownAction { name: String },

    #[error("Action `{name}` is already registered.")]
    DuplicateAction { name: String },

    #[error("Input for action `{name}` is invalid: {reason}")]
    InvalidActionInput { name: String, reason: String },

    #[error(
        "Action `{name}` requires permission `{permission}`, which is denied by project configuration."
    )]
    PermissionDenied { name: String, permission: String },

    #[error(
        "Action `{name}` requires confirmation and none was given. Re-run interactively or pass an explicit approval flag."
    )]
    ConfirmationRequired { name: String },

    #[error("Action `{name}` was not confirmed by the user.")]
    ConfirmationDenied { name: String },

    #[error("Command `{name}` is not configured for this project.")]
    CommandNotConfigured { name: String },

    #[error("Command execution for `{name}` failed to start: {reason}")]
    CommandExecutionFailed { name: String, reason: String },

    #[error("{0}")]
    Provider(#[from] ProviderError),

    #[error(
        "The agent reached the maximum of {limit} tool-call iterations without a final answer."
    )]
    AgentIterationLimitReached { limit: u32 },

    #[error("The model requested unknown tool `{name}`.")]
    UnknownToolCall { name: String },

    #[error("The model produced a malformed tool call: {reason}")]
    MalformedToolCall { reason: String },

    #[error("MCP error: {0}")]
    Mcp(String),

    #[error(
        "No `.orbit/workspace.yaml` was found searching upward from {searched_from}.\nRun `orbit workspace init` to create one."
    )]
    WorkspaceNotFound { searched_from: PathBuf },

    #[error("Unknown project `{name}`. Available projects: {}", available.join(", "))]
    UnknownProject {
        name: String,
        available: Vec<String>,
    },

    #[error(
        "`{name}` matches more than one registered project or alias; use the exact registered name."
    )]
    AmbiguousProject { name: String },

    #[error(
        "No project is selected. Available projects: {}. Use --project <name> or run this from inside a registered project.",
        available.join(", ")
    )]
    NoActiveProject { available: Vec<String> },

    #[error("Project `{name}` is unavailable: {reason}")]
    ProjectUnavailable { name: String, reason: String },

    #[error(
        "Workspace project `{name}` at `{path}` resolves outside the workspace root; refusing to load the workspace."
    )]
    WorkspaceProjectEscapesRoot { name: String, path: PathBuf },

    #[error("I/O error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Failed to parse YAML configuration: {0}")]
    Yaml(String),
}

impl OrbitError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        OrbitError::Io {
            path: path.into(),
            source,
        }
    }
}

/// Errors from a model provider, kept independent of any specific provider
/// implementation so the agent layer never has to match on provider-specific
/// types (e.g. Ollama HTTP status codes).
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("could not reach the model provider at {endpoint}: {reason}")]
    ConnectionFailed { endpoint: String, reason: String },

    #[error("the model provider timed out after {timeout_secs}s")]
    Timeout { timeout_secs: u64 },

    #[error("model `{model}` is not available on this provider. {hint}")]
    ModelUnavailable { model: String, hint: String },

    #[error("the model provider returned a malformed response: {reason}")]
    InvalidResponse { reason: String },

    #[error("the model provider rejected the request (rate limited or overloaded): {reason}")]
    RateLimited { reason: String },

    #[error("the model provider rejected the request as unauthorized: {reason}")]
    Unauthorized { reason: String },

    #[error("the configured model does not appear to support tool calling: {reason}")]
    ToolCallingUnsupported { reason: String },

    #[error("the model provider request failed: {reason}")]
    Other { reason: String },
}

pub type Result<T> = std::result::Result<T, OrbitError>;
