mod registry;
mod server;

use clap::Parser;
use dashmap::DashMap;
use loom_config::{load_config, load_config_from};
use registry::DelegateRegistry;
use server::LoomServer;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::Server;

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    stdio: bool,

    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_file = std::fs::File::create(std::env::temp_dir().join("loom.log"))?;

    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_env_filter("loom=debug,loom_delegate=debug")
        .init();

    tracing::info!("Starting loom language server");

    let cli = Cli::parse();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let config = if let Some(path) = cli.config {
        load_config_from(&path)?
    } else {
        load_config()?
    };

    let (service, socket) = tower_lsp::LspService::new(|client| LoomServer {
        client,
        chunks: DashMap::new(),
        virtual_documents: DashMap::new(),
        registry: Mutex::new(DelegateRegistry::new(config.languages)),
        completion_cache: Arc::new(DashMap::new()),
        diagnostics_store: DashMap::new(),
    });

    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
