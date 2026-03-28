use clap::Parser;
use dashmap::DashMap;
use loom_config::{Config, load_config, load_config_from};
use loom_delegate::DelegateServer;
use loom_parse::parse_qmd;
use loom_vdoc::{VirtualDocument, build_virtual_docs};
use std::path::PathBuf;

use tokio::sync::Mutex;
use tower_lsp::lsp_types::{
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams,
    InitializeParams, InitializeResult, ServerCapabilities, ServerInfo, TextDocumentSyncCapability,
    TextDocumentSyncKind, Url,
};
use tower_lsp::{LanguageServer, Server};

#[derive(Debug)]
struct LoomServer {
    documents: DashMap<Url, String>,
    virtual_documents: DashMap<Url, Vec<VirtualDocument>>,
    config: Config,
    registry: DashMap<String, Mutex<DelegateServer>>,
}

#[tower_lsp::async_trait]
impl LanguageServer for LoomServer {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        tracing::info!("Initialize request received");

        for (language, lang_config) in &self.config.languages {
            match DelegateServer::spawn(&lang_config.server_command) {
                Ok(mut delegate) => {
                    if let Err(e) = delegate.initialize(params.root_uri.clone()).await {
                        tracing::warn!("failed to initialize delegate for {language}: {e}");
                    } else {
                        tracing::info!("delegate ready for {language}");

                        // Add the delegate to the registry so we can forward documents to it
                        // later.
                        self.registry.insert(language.clone(), Mutex::new(delegate));
                    }
                }
                Err(e) => tracing::warn!("failed to spawn delegate for {language}: {e}"),
            }
        }

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
        for entry in self.registry.iter() {
            if let Err(e) = entry.value().lock().await.shutdown().await {
                tracing::warn!("failed to shutdown delegate for {}: {e}", entry.key());
            }
        }
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        tracing::info!("Document opened: {} ({} bytes)", uri, text.len());

        let chunks = parse_qmd(&text).unwrap();
        let vdocs = build_virtual_docs(&chunks, text.lines().count() as u32, &uri);
        tracing::info!("built {} virtual docs for {}", vdocs.len(), uri);

        self.documents.insert(uri.clone(), text);

        // TODO(8.5): forward each virtual doc to the matching delegate
        for vdoc in &vdocs {
            if let Some(delegate) = self.registry.get(&vdoc.language)
                && let Err(e) = delegate
                    .lock()
                    .await
                    .open_document(vdoc.uri.clone(), &vdoc.language, &vdoc.content)
                    .await
            {
                tracing::warn!("failed to open virtual doc on delegate: {e}");
            }
        }

        self.virtual_documents.insert(uri, vdocs);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;

        tracing::info!("Document closed: {}", uri);

        self.documents.remove(&uri);

        if let Some((_, vdocs)) = self.virtual_documents.remove(&uri) {
            for vdoc in vdocs {
                if let Some(delegate) = self.registry.get(&vdoc.language)
                    && let Err(e) = delegate.lock().await.close_document(vdoc.uri).await
                {
                    tracing::warn!("failed to close virtual doc on delegate: {e}");
                }
            }
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.content_changes[0].text.clone();

        tracing::info!("Document changed: {} ({} bytes)", uri, text.len());

        let chunks = parse_qmd(&text).unwrap();
        let vdocs = build_virtual_docs(&chunks, text.lines().count() as u32, &uri);
        tracing::info!("built {} virtual docs for {}", vdocs.len(), uri);

        self.documents.insert(uri.clone(), text);

        for vdoc in &vdocs {
            if let Some(delegate) = self.registry.get(&vdoc.language)
                && let Err(e) = delegate
                    .lock()
                    .await
                    .update_document(vdoc.uri.clone(), vdoc.version, &vdoc.content)
                    .await
            {
                tracing::warn!("failed to update virtual doc on delegate: {e}");
            }
        }

        self.virtual_documents.insert(uri, vdocs);
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
        documents: DashMap::new(),
        virtual_documents: DashMap::new(),
        config,
        registry: DashMap::new(),
    };

    let (service, socket) = tower_lsp::LspService::new(|_client| server);

    // Start the server and block until it finishes.
    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
