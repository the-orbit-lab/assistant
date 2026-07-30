use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "orbit",
    version,
    about = "Orbit: a local-first AI engineering assistant"
)]
pub struct Cli {
    #[command(flatten)]
    pub global: GlobalArgs,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Args, Clone)]
pub struct GlobalArgs {
    /// A filesystem path, or (when a workspace is active) a registered
    /// project name or alias. A value starting with `./`, `../`, `/`, or a
    /// platform path prefix is always treated as a path.
    #[arg(long, global = true)]
    pub project: Option<String>,

    /// Use this workspace directory instead of searching upward from the
    /// current directory for `.orbit/workspace.yaml`.
    #[arg(long, global = true)]
    pub workspace: Option<PathBuf>,

    /// Use this exact `project.yaml` path instead of the usual
    /// `<root>/.orbit/project.yaml` layout.
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Print machine-readable JSON instead of formatted text.
    #[arg(long, global = true)]
    pub json: bool,

    /// Override the configured model name for this invocation.
    #[arg(long, global = true)]
    pub model: Option<String>,

    /// Override the configured Ollama endpoint for this invocation.
    #[arg(long, global = true, value_name = "URL")]
    pub ollama_endpoint: Option<String>,

    /// Approve every `ask`-permission action for this invocation without
    /// prompting. Required in non-interactive contexts (no TTY) to run
    /// anything gated by `ask`.
    #[arg(long, global = true)]
    pub yes: bool,

    /// Log project/agent internals to stderr: resolved root and config,
    /// discovered-file counts, tools offered to the model, tool calls it
    /// made, and action results. Never logs file contents, secrets, or
    /// environment variables. `RUST_LOG` overrides this with full
    /// `tracing` filter syntax (e.g. `RUST_LOG=orbit_agent=trace`).
    #[arg(long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Create `.orbit/project.yaml`.
    Init(InitArgs),
    /// Show project metadata, provider, commands, and permissions.
    Project,
    /// List every file the project configuration allows Orbit to see.
    Files,
    /// Run deterministic local search, without the model.
    Search(SearchArgs),
    /// Ask the configured model a question, grounded in project context.
    Ask(AskArgs),
    /// List configured commands and the permission each requires.
    Commands,
    /// Run a single named configured command.
    Run(RunArgs),
    /// Check local setup: config, Ollama connectivity, model availability.
    Doctor,
    /// Start an interactive multi-turn session.
    Chat,
    /// MCP server/client operations.
    #[command(subcommand)]
    Mcp(McpCommand),
    /// Show workspace information, or `orbit workspace init` to create one.
    Workspace(WorkspaceArgs),
    /// List every project registered in the active workspace.
    Projects,
}

#[derive(Subcommand)]
pub enum McpCommand {
    /// Serve this project's (or, with --workspace, workspace's) exposed
    /// actions over MCP stdio.
    Serve,
}

#[derive(Args)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub action: Option<WorkspaceAction>,
}

#[derive(Subcommand)]
pub enum WorkspaceAction {
    /// Create `.orbit/workspace.yaml`, registering immediate child
    /// directories that already contain `.orbit/project.yaml`.
    Init(WorkspaceInitArgs),
}

#[derive(Args)]
pub struct WorkspaceInitArgs {
    /// Overwrite an existing `.orbit/workspace.yaml`.
    #[arg(long)]
    pub force: bool,
    /// Workspace name written into the generated configuration. Defaults
    /// to the target directory's name.
    #[arg(long)]
    pub name: Option<String>,
    /// Workspace description written into the generated configuration.
    #[arg(long, default_value = "")]
    pub description: String,
}

#[derive(Args)]
pub struct InitArgs {
    /// Overwrite an existing `.orbit/project.yaml`.
    #[arg(long)]
    pub force: bool,
    /// Project name written into the starter configuration. Defaults to
    /// the target directory's name.
    #[arg(long)]
    pub name: Option<String>,
    /// Project type written into the starter configuration.
    #[arg(long, default_value = "software")]
    pub r#type: String,
    /// Project description written into the starter configuration.
    #[arg(long, default_value = "")]
    pub description: String,
}

#[derive(Args)]
pub struct SearchArgs {
    pub query: String,
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
    /// Search these registered projects instead of the current
    /// single-project context. Comma-separated registered names or
    /// aliases, e.g. `--projects docs,obc`.
    #[arg(long, value_delimiter = ',')]
    pub projects: Vec<String>,
}

#[derive(Args)]
pub struct AskArgs {
    pub question: String,
    /// Ask across these registered projects instead of the current
    /// single-project context, or instead of scanning the question text
    /// for project names. Comma-separated registered names or aliases.
    #[arg(long, value_delimiter = ',')]
    pub projects: Vec<String>,
}

#[derive(Args)]
pub struct RunArgs {
    pub name: String,
}
