mod config;
mod exec;
mod register;
mod schema;
mod server;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rmcp::{ServiceExt, transport::stdio};

use crate::config::{init_server_file, load_server_file, missing_file_hint, resolve_config_path};
use crate::server::MultiMcp;

#[derive(Parser)]
#[command(
    name = "multimcp",
    about = "Serve one JSON-defined MCP server over stdio"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Server name: serves <config_dir>/MultiMCP/<NAME>.json
    name: Option<String>,
    /// Disable the built-in register_command tool (locked-down mode)
    #[arg(long)]
    disable_register: bool,
}

#[derive(Subcommand)]
enum Command {
    /// Create <config_dir>/MultiMCP/<NAME>.json with example content
    Init {
        /// Server name to create
        name: String,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    if cli.command.is_some() && cli.name.is_some() {
        anyhow::bail!("pass either a server NAME or a subcommand, not both");
    }
    if let Some(Command::Init { name }) = cli.command {
        let path = resolve_config_path(&name)?;
        init_server_file(&path, &name)?;
        println!(
            "created {} — run `multimcp {name}` to serve it",
            path.display()
        );
        return Ok(());
    }
    let Some(name) = cli.name else {
        anyhow::bail!("provide a server NAME to serve, or `multimcp init <NAME>` to create one");
    };
    let path = resolve_config_path(&name)?;
    if !path.is_file() {
        eprintln!("multimcp: {}", missing_file_hint(&path, &name));
        std::process::exit(2);
    }
    let def = load_server_file(&path)?;
    eprintln!(
        "multimcp: serving {:?} ({} tool{}) from {}",
        def.name,
        def.commands.len(),
        if def.commands.len() == 1 { "" } else { "s" },
        path.display()
    );
    let service = MultiMcp::new(def, path, !cli.disable_register);
    let running = service.serve(stdio()).await.context("serve failed")?;
    running.waiting().await.context("server error")?;
    Ok(())
}
