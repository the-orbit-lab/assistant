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
    match dispatch(cli).await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("Error: {err}");
            match err {
                OrbitError::ConfigNotFound { .. } | OrbitError::AmbiguousProjectRoot { .. } => {
                    std::process::ExitCode::from(2)
                }
                _ => std::process::ExitCode::FAILURE,
            }
        }
    }
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
    }
}
