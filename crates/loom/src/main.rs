mod registry;

use clap::Parser;
use dashmap::DashMap;
use loom_config::{load_config, load_config_from};
use loom_delegate::TransportSender;
use loom_parse::{CodeChunk, language_at_position, parse_qmd};
use loom_vdoc::{VirtualDocument, build_virtual_docs};
use registry::DelegateRegistry;
use std::path::PathBuf;

use std::sync::Arc;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::{
    CompletionOptions, CompletionParams, CompletionResponse, DidChangeTextDocumentParams,
    DidCloseTextDocumentParams, DidOpenTextDocumentParams, InitializeParams, InitializeResult,
    Position, ServerCapabilities, ServerInfo, TextDocumentIdentifier, TextDocumentPositionParams,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};
use tower_lsp::{LanguageServer, Server};

#[derive(Debug)]
struct LoomServer {
    documents: DashMap<Url, String>,
    chunks: DashMap<Url, Vec<CodeChunk>>,
    virtual_documents: DashMap<Url, Vec<VirtualDocument>>,
    registry: Mutex<DelegateRegistry>,
    /// Caches the most recent raw completion result per language, populated by background tasks
    /// that survive tower-lsp task cancellation (e.g. when a slow LSP like Julia takes > 300ms). I
    /// guess there could be a better way to correlate in-flight requests and responses, but this
    /// is simple and seems to work well in practice.
    completion_cache: Arc<DashMap<String, serde_json::Value>>,
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

        // Collect which languages need a new delegate spawned. Hold the registry lock only long
        // enough for this check — not during the multi-second LSP initialize handshake.
        let to_spawn: Vec<(String, Vec<String>, Option<tower_lsp::lsp_types::Url>)> = {
            let registry = self.registry.lock().await;
            vdocs
                .iter()
                .filter_map(|vdoc| {
                    registry
                        .spawn_params(&vdoc.language)
                        .map(|(cmd, root_uri)| (vdoc.language.clone(), cmd, root_uri))
                })
                .collect()
        };

        // Initialize all needed delegates concurrently, with no lock held.
        let init_futs = to_spawn.into_iter().map(|(lang, cmd, root_uri)| async move {
            let cmd_str = cmd.join(" ");
            let mut delegate = match loom_delegate::DelegateServer::spawn(&cmd) {
                Ok(d) => d,
                Err(e) => {
                    tracing::warn!("failed to spawn `{cmd_str}`: {e}");
                    return (lang, Err(()));
                }
            };
            match delegate.initialize(root_uri).await {
                Ok(()) => (lang, Ok(delegate)),
                Err(e) => {
                    tracing::warn!("failed to initialize `{cmd_str}`: {e}");
                    (lang, Err(()))
                }
            }
        });
        let init_results = futures::future::join_all(init_futs).await;

        // Insert results — brief lock per insert.
        let mut handles = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for (lang, result) in init_results {
                match result {
                    Ok(delegate) => registry.insert_ready(lang, delegate),
                    Err(()) => registry.mark_failed(lang),
                }
            }
            for vdoc in &vdocs {
                if registry.is_failed(&vdoc.language) {
                    continue;
                }
                match registry.get_or_spawn(&vdoc.language).await {
                    Ok(handle) => handles.push((
                        handle,
                        vdoc.uri.clone(),
                        vdoc.language.clone(),
                        vdoc.content.clone(),
                    )),
                    Err(e) => tracing::warn!("failed to get delegate for {}: {e}", vdoc.language),
                }
            }
        }
        for (handle, vdoc_uri, language, content) in handles {
            if let Err(e) = handle
                .lock()
                .await
                .open_document(vdoc_uri, &language, &content)
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
        self.chunks.remove(&uri);

        if let Some((_, vdocs)) = self.virtual_documents.remove(&uri) {
            let mut handles = Vec::new();
            {
                let mut registry = self.registry.lock().await;
                for vdoc in &vdocs {
                    if registry.is_failed(&vdoc.language) {
                        continue;
                    }
                    match registry.get_or_spawn(&vdoc.language).await {
                        Ok(handle) => handles.push((handle, vdoc.uri.clone())),
                        Err(e) => {
                            tracing::warn!("failed to get delegate for {}: {e}", vdoc.language)
                        }
                    }
                }
            }
            for (handle, vdoc_uri) in handles {
                if let Err(e) = handle.lock().await.close_document(vdoc_uri).await {
                    tracing::warn!("failed to close virtual doc on delegate: {e}");
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

        let mut handles = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for vdoc in &vdocs {
                if registry.is_failed(&vdoc.language) {
                    continue;
                }
                match registry.get_or_spawn(&vdoc.language).await {
                    Ok(handle) => {
                        handles.push((handle, vdoc.uri.clone(), vdoc.version, vdoc.content.clone()))
                    }
                    Err(e) => tracing::warn!("failed to get delegate for {}: {e}", vdoc.language),
                }
            }
        }
        for (handle, vdoc_uri, version, content) in handles {
            if let Err(e) = handle
                .lock()
                .await
                .update_document(vdoc_uri, version, &content)
                .await
            {
                tracing::warn!("failed to update virtual doc on delegate: {e}");
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

        let vdoc_uri = match self.virtual_documents.get(uri) {
            Some(vdocs) => match vdocs.iter().find(|v| v.language == language) {
                Some(vdoc) => vdoc.uri.clone(),
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

        // Get a cloneable sender — hold the per-delegate lock only long enough to clone it so
        // that other tasks (e.g. did_change) can acquire the lock while we await the response.
        let sender: TransportSender = {
            let mut registry = self.registry.lock().await;
            let handle = registry
                .get_or_spawn(&language)
                .await
                .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;
            handle.lock().await.sender()
        };

        // Check the cache first — a previous background task may have stored the result after
        // its tower-lsp task was cancelled (e.g. slow Julia LSP completing after the timeout).
        if let Some((_, cached)) = self.completion_cache.remove(&language) {
            tracing::info!("completion: serving cached result for {language}");
            let response: Option<CompletionResponse> =
                serde_json::from_value(cached).ok().flatten();
            return Ok(response);
        }

        // No locks held from this point. The did_change handler already keeps the delegate
        // document current — sending another didChange here would duplicate version numbers
        // and corrupt the delegate LSP's document state.
        let completion_params = CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: vdoc_uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };
        let params_value = serde_json::to_value(completion_params)
            .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;

        // Spawn a detached task so the LSP request survives tower-lsp task cancellation.
        // neovim sends $/cancelRequest unpredictably (200-300ms); waiting in the tower-lsp
        // handler is a race we can't win reliably. Instead: always return null immediately,
        // store the result in cache, and serve it on the next completion trigger.
        let cache = Arc::clone(&self.completion_cache);
        let lang = language.clone();
        tokio::spawn(async move {
            if let Ok(raw) = sender
                .send_request("textDocument/completion", params_value)
                .await
            {
                let result = raw["result"].clone();
                tracing::debug!("completion background result for {lang}: {result}");
                cache.insert(lang, result);
            }
        });

        tracing::info!(
            "completion: {language} request in flight, returning null (result will be cached)"
        );
        Ok(None)
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

    let server = LoomServer {
        documents: DashMap::new(),
        chunks: DashMap::new(),
        virtual_documents: DashMap::new(),
        registry: Mutex::new(DelegateRegistry::new(config.languages)),
        completion_cache: Arc::new(DashMap::new()),
    };

    let (service, socket) = tower_lsp::LspService::new(|_client| server);

    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
