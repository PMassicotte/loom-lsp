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
    let cli = Cli::parse();

    let config = if let Some(path) = cli.config {
        load_config_from(&path)?
    } else {
        load_config()?
    };

    let log_file = std::fs::File::create(std::env::temp_dir().join("loom.log"))?;

    let level = config
        .server
        .as_ref()
        .map(|s| s.log_level.as_str())
        .unwrap_or("info");

    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        tracing_subscriber::EnvFilter::new(format!("loom={level},loom_delegate={level}"))
    });

    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_env_filter(filter)
        .init();

    tracing::info!("Starting loom language server");

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = tower_lsp::LspService::new(|client| LoomServer {
        client,
        chunks: DashMap::new(),
        virtual_documents: DashMap::new(),
        registry: Arc::new(Mutex::new(DelegateRegistry::new(config.languages))),
        reverse_vdoc_index: Arc::new(DashMap::new()),
        completion_cache: Arc::new(DashMap::new()),
        diagnostics_store: Arc::new(DashMap::new()),
        parsers: DashMap::new(),
    });

    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
