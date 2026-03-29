mod registry;

use clap::Parser;
use dashmap::DashMap;
use loom_config::{load_config, load_config_from};
use loom_parse::{CodeChunk, language_at_position, parse_qmd};
use loom_vdoc::{VirtualDocument, build_virtual_docs};
use registry::DelegateRegistry;
use std::path::PathBuf;

use tokio::sync::Mutex;
use tower_lsp::lsp_types::{
    CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, InitializeParams, InitializeResult,
    ServerCapabilities, ServerInfo, TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{LanguageServer, Server};

#[derive(Debug)]
struct LoomServer {
    documents: DashMap<Url, String>,
    chunks: DashMap<Url, Vec<CodeChunk>>,
    virtual_documents: DashMap<Url, Vec<VirtualDocument>>,
    registry: Mutex<DelegateRegistry>,
}

#[tower_lsp::async_trait]
impl LanguageServer for LoomServer {
    async fn initialize(
        &self,
        params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        tracing::info!("Initialize request received");

        self.registry.lock().await.set_root_uri(params.root_uri);

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "loom".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::FULL,
                )),
                completion_provider: Some(CompletionOptions::default()),
                ..ServerCapabilities::default()
            },
        })
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        self.registry.lock().await.shutdown_all().await;
        Ok(())
    }

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        tracing::info!("Document opened: {} ({} bytes)", uri, text.len());

        let parsed_chunks = parse_qmd(&text).unwrap();
        let vdocs = build_virtual_docs(&parsed_chunks, text.split('\n').count() as u32, &uri);
        tracing::info!("built {} virtual docs for {}", vdocs.len(), uri);

        self.documents.insert(uri.clone(), text);
        self.chunks.insert(uri.clone(), parsed_chunks);

        let mut registry = self.registry.lock().await;
        for vdoc in &vdocs {
            match registry.get_or_spawn(&vdoc.language).await {
                Ok(delegate) => {
                    if let Err(e) = delegate
                        .open_document(vdoc.uri.clone(), &vdoc.language, &vdoc.content)
                        .await
                    {
                        tracing::warn!("failed to open virtual doc on delegate: {e}");
                    }
                }
                Err(e) => tracing::warn!("failed to spawn delegate for {}: {e}", vdoc.language),
            }
        }

        self.virtual_documents.insert(uri, vdocs);
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;

        tracing::info!("Document closed: {}", uri);

        self.documents.remove(&uri);
        self.chunks.remove(&uri);

        if let Some((_, vdocs)) = self.virtual_documents.remove(&uri) {
            let mut registry = self.registry.lock().await;
            for vdoc in vdocs {
                match registry.get_or_spawn(&vdoc.language).await {
                    Ok(delegate) => {
                        if let Err(e) = delegate.close_document(vdoc.uri).await {
                            tracing::warn!("failed to close virtual doc on delegate: {e}");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("failed to get delegate for {}: {e}", vdoc.language)
                    }
                }
            }
        }
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.content_changes[0].text.clone();

        tracing::info!("Document changed: {} ({} bytes)", uri, text.len());

        let parsed_chunks = parse_qmd(&text).unwrap();
        let mut vdocs = build_virtual_docs(&parsed_chunks, text.split('\n').count() as u32, &uri);

        // Increment version for each virtual doc from the previous state.
        if let Some(old_vdocs) = self.virtual_documents.get(&uri) {
            for vdoc in &mut vdocs {
                if let Some(old) = old_vdocs.iter().find(|v| v.language == vdoc.language) {
                    vdoc.version = old.version + 1;
                }
            }
        }

        tracing::info!("built {} virtual docs for {}", vdocs.len(), uri);

        self.documents.insert(uri.clone(), text);
        self.chunks.insert(uri.clone(), parsed_chunks);
        self.virtual_documents.insert(uri.clone(), vdocs.clone());

        let mut registry = self.registry.lock().await;
        for vdoc in &vdocs {
            match registry.get_or_spawn(&vdoc.language).await {
                Ok(delegate) => {
                    if let Err(e) = delegate
                        .update_document(vdoc.uri.clone(), vdoc.version, &vdoc.content)
                        .await
                    {
                        tracing::warn!("failed to update virtual doc on delegate: {e}");
                    }
                }
                Err(e) => tracing::warn!("failed to get delegate for {}: {e}", vdoc.language),
            }
        }
    }

    async fn completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let line = params.text_document_position.position.line;
        let character = params.text_document_position.position.character;

        tracing::info!("Completion request at {}:{}:{}", uri, line, character);

        let chunks = match self.chunks.get(uri) {
            Some(c) => c,
            None => {
                tracing::info!("completion: no chunks for {uri}");
                return Ok(None);
            }
        };

        let language = match language_at_position(&chunks, line) {
            Some(l) => l.to_string(),
            None => {
                tracing::info!("completion: line {line} is not in a code chunk");
                return Ok(None);
            }
        };

        let (vdoc_uri, vdoc_content, vdoc_version) = match self.virtual_documents.get(uri) {
            Some(vdocs) => match vdocs.iter().find(|v| v.language == language) {
                Some(vdoc) => (vdoc.uri.clone(), vdoc.content.clone(), vdoc.version),
                None => {
                    tracing::info!("completion: no virtual doc for language {language}");
                    return Ok(None);
                }
            },
            None => {
                tracing::info!("completion: no virtual documents for {uri}");
                return Ok(None);
            }
        };

        tracing::info!(
            "forwarding completion to delegate: language={language} uri={vdoc_uri} line={line} char={character}"
        );

        let mut registry = self.registry.lock().await;
        let delegate = registry
            .get_or_spawn(&language)
            .await
            .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;

        if let Err(e) = delegate
            .update_document(vdoc_uri.clone(), vdoc_version + 1, &vdoc_content)
            .await
        {
            tracing::warn!("completion: failed to sync doc before completion: {e}");
        }

        let result = delegate
            .completion(vdoc_uri, line, character)
            .await
            .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;

        let response: Option<CompletionResponse> =
            serde_json::from_value(result).map_err(|e: serde_json::Error| {
                tower_lsp::jsonrpc::Error::invalid_params(e.to_string())
            })?;

        Ok(response)
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

    let server = LoomServer {
        documents: DashMap::new(),
        chunks: DashMap::new(),
        virtual_documents: DashMap::new(),
        registry: Mutex::new(DelegateRegistry::new(config.languages)),
    };

    let (service, socket) = tower_lsp::LspService::new(|_client| server);

    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
