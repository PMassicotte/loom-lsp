use clap::Parser;
use dashmap::DashMap;
use loom_config::{Config, load_config, load_config_from};
use std::path::PathBuf;

use std::sync::Arc;
use tower_lsp::lsp_types::{
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, InitializeParams, InitializeResult,
    ServerCapabilities, ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{LanguageServer, Server};

#[derive(Debug)]
struct LoomServer {
    documents: Arc<DashMap<Url, String>>,
    config: Config,
}

#[tower_lsp::async_trait]
impl LanguageServer for LoomServer {
    async fn initialize(
        &self,
        _params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        tracing::info!("Initialize request received");

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "loom".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                ..ServerCapabilities::default()
            },
        })
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        tracing::info!("Document opened: {} ({} bytes)", uri, text.len());

        self.documents.insert(uri, text);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;

        tracing::info!("Document closed: {}", uri);

        self.documents.remove(&uri);
    }
}

#[derive(Parser)]
struct Cli {
    #[arg(long)]
    stdio: bool,

    #[arg(long)]
    config: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_file = std::fs::File::create("/tmp/loom.log")?;

    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_env_filter("loom=debug")
        .init();

    tracing::info!("Starting loom language server");

    let cli = Cli::parse();

    // Create the standard input and output streams for the LSP server.
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    // Load the configuration, either from the specified path or by discovering it automatically.
    let config = if let Some(path) = cli.config {
        load_config_from(&path)? // load from explicit path
    } else {
        load_config()? // discover automatically
    };

    // Create the LSP service and the socket to listen for incoming messages.
    let server = LoomServer {
        documents: Arc::new(DashMap::new()),
        config,
    };

    let (service, socket) = tower_lsp::LspService::new(|_client| server);

    // Start the server and block until it finishes.
    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
