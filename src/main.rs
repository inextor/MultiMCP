mod config;
mod exec;
mod register;
mod schema;
mod server;

use anyhow::{Context, Result};
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};

use crate::config::{load_server_file, missing_file_hint, resolve_config_path};
use crate::server::MultiMcp;

#[derive(Parser)]
#[command(
    name = "multimcp",
    about = "Serve one JSON-defined MCP server over stdio"
)]
struct Cli {
    /// Server name: serves <config_dir>/MultiMCP/<NAME>.json
    name: String,
    /// Disable the built-in register_command tool (locked-down mode)
    #[arg(long)]
    disable_register: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = resolve_config_path(&cli.name)?;
    if !path.is_file() {
        eprintln!("multimcp: {}", missing_file_hint(&path));
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
