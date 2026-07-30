mod args;
mod commands;
mod confirm;
mod output;
mod resolve;
mod runtime;

use clap::Parser;
use orbit_core::OrbitError;

use args::{Cli, Command, McpCommand};

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    init_tracing(cli.global.verbose);
    match dispatch(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            match err {
                OrbitError::ConfigNotFound { .. }
                | OrbitError::AmbiguousProjectRoot { .. }
                | OrbitError::WorkspaceNotFound { .. }
                | OrbitError::NoActiveProject { .. } => std::process::ExitCode::from(2),
                _ => std::process::ExitCode::FAILURE,
            }
        }
    }
}

/// Always logs to stderr (stdout carries `mcp serve`'s JSON-RPC traffic
/// and `--json` output, neither of which may be polluted by log lines).
/// `RUST_LOG` takes precedence, using normal `tracing` filter syntax;
/// `--verbose` is a coarse default for "show me what Orbit is doing"
/// without needing to know that syntax. Logs are debug-level summaries
/// only -- action names, paths, counts, durations -- never file contents,
/// secrets, or environment variable values.
fn init_tracing(verbose: bool) {
    let filter = std::env::var("RUST_LOG")
        .ok()
        .and_then(|v| tracing_subscriber::EnvFilter::try_new(v).ok())
        .unwrap_or_else(|| {
            let default = if verbose {
                "warn,orbit_cli=debug,orbit_agent=debug,orbit_actions=debug,orbit_mcp_client=debug"
            } else {
                "warn"
            };
            tracing_subscriber::EnvFilter::new(default)
        });

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .init();
}

async fn dispatch(cli: Cli) -> Result<(), OrbitError> {
    match cli.command {
        Command::Init(args) => commands::init::run(&cli.global, args),
        Command::Project => commands::project::run(&cli.global).await,
        Command::Files => commands::files::run(&cli.global).await,
        Command::Search(args) => commands::search::run(&cli.global, args).await,
        Command::Ask(args) => commands::ask::run(&cli.global, args).await,
        Command::Commands => commands::list_commands::run(&cli.global).await,
        Command::Run(args) => commands::run::run(&cli.global, args).await,
        Command::Doctor => commands::doctor::run(&cli.global).await,
        Command::Chat => commands::chat::run(&cli.global).await,
        Command::Mcp(McpCommand::Serve) => commands::mcp_serve::run(&cli.global).await,
        Command::Workspace(args) => commands::workspace::run(&cli.global, args).await,
        Command::Projects => commands::projects::run(&cli.global).await,
    }
}
