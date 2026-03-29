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
    chunks: DashMap<Url, Vec<CodeChunk>>,
    virtual_documents: DashMap<Url, Vec<VirtualDocument>>,
    registry: Mutex<DelegateRegistry>,
    /// Caches the most recent completion result per language. Fast LSPs (pyright) populate this
    /// via direct await; slow LSPs (Julia) populate it via background tasks. Used as fallback
    /// when the direct request times out.
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

        self.chunks.insert(uri.clone(), parsed_chunks);

        // Collect which languages need a new delegate spawned. Hold the registry lock only long
        // enough for this check not during the multi-second LSP initialize handshake.
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
        let init_futs = to_spawn
            .into_iter()
            .map(|(lang, cmd, root_uri)| async move {
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

        // Insert results brief lock per insert.
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

        self.chunks.remove(&uri);

        if let Some((_, vdocs)) = self.virtual_documents.remove(&uri) {
            let mut handles = Vec::new();
            {
                let mut registry = self.registry.lock().await;
                for vdoc in &vdocs {
                    if let Some(handle) = registry.get_if_alive(&vdoc.language).await {
                        handles.push((handle, vdoc.uri.clone()));
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

        self.chunks.insert(uri.clone(), parsed_chunks);
        self.virtual_documents.insert(uri.clone(), vdocs.clone());

        let mut handles = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for vdoc in &vdocs {
                // Only send to delegates that are already alive, did_change should not trigger
                // a slow re-spawn. A dead delegate will be re-spawned on the next completion.
                if let Some(handle) = registry.get_if_alive(&vdoc.language).await {
                    handles.push((handle, vdoc.uri.clone(), vdoc.version, vdoc.content.clone()));
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

        // Hold each DashMap ref only briefly — releasing the shard lock before any await prevents
        // did_change's synchronous insert from blocking Tokio threads.
        let language = {
            let chunks = match self.chunks.get(uri) {
                Some(c) => c,
                None => return Ok(None),
            };
            match language_at_position(&chunks, line) {
                Some(l) => l.to_string(),
                None => return Ok(None),
            }
        };
        let vdoc_uri = match self.virtual_documents.get(uri) {
            Some(vdocs) => match vdocs.iter().find(|v| v.language == language) {
                Some(vdoc) => vdoc.uri.clone(),
                None => return Ok(None),
            },
            None => return Ok(None),
        };

        tracing::info!(
            "forwarding completion to delegate: language={language} line={line} char={character}"
        );

        // Never spawn delegates inside completion, did_open handles that.
        let sender: TransportSender = {
            let mut registry = self.registry.lock().await;
            match registry.get_if_alive(&language).await {
                Some(handle) => handle.lock().await.sender(),
                None => {
                    tracing::info!("completion: delegate for {language} not ready yet");
                    return Ok(None);
                }
            }
        };

        let params_value = serde_json::to_value(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: vdoc_uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;

        // Always send the request in a background task so it survives neovim's
        // $/cancelRequest, which cancels the tower-lsp handler future. The background
        // task updates the cache when the LSP responds, regardless of what happens here.
        let (fresh_tx, fresh_rx) = tokio::sync::oneshot::channel();
        let cache = Arc::clone(&self.completion_cache);
        let lang = language.clone();
        tokio::spawn(async move {
            if let Ok(raw) = sender
                .send_request("textDocument/completion", params_value)
                .await
            {
                let result = raw["result"].clone();
                if !result.is_null() {
                    cache.insert(lang, result.clone());
                    let _ = fresh_tx.send(result);
                }
            }
        });

        // Wait briefly for the background task's result. Fast LSPs (pyright ~50ms) respond
        // in time and we return fresh completions. Slow LSPs (Julia ~2s) time out and we
        // fall back to the stale cache, which the background task will eventually update.
        match tokio::time::timeout(std::time::Duration::from_millis(200), fresh_rx).await {
            Ok(Ok(result)) => {
                tracing::info!("completion: fresh result for {language}");
                return Ok(serde_json::from_value(result).ok().flatten());
            }
            _ => {}
        }

        // Stale cache fallback. Strip textEdit since cursor positions may be stale.
        if let Some(cached) = self.completion_cache.get(&language) {
            let mut value = cached.clone();
            strip_text_edits(&mut value);
            tracing::info!("completion: stale cache for {language}");
            return Ok(serde_json::from_value(value).ok().flatten());
        }

        tracing::info!("completion: no result for {language}");
        Ok(None)
    }
}

/// Strips `textEdit` from every completion item, promoting `textEdit.newText` to `insertText`
/// when not already set. Stale position ranges from cached responses corrupt insertions when
/// served at a different cursor position.
fn strip_text_edits(value: &mut serde_json::Value) {
    let items = if let Some(arr) = value.as_array_mut() {
        arr
    } else if let Some(arr) = value.get_mut("items").and_then(|v| v.as_array_mut()) {
        arr
    } else {
        return;
    };
    for item in items.iter_mut() {
        let new_text = item
            .get("textEdit")
            .and_then(|te| {
                te.get("newText")
                    .or_else(|| te.get("insert").and_then(|r| r.get("newText")))
            })
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(obj) = item.as_object_mut() {
            obj.remove("textEdit");
            obj.remove("additionalTextEdits");
            if let Some(text) = new_text {
                obj.entry("insertText")
                    .or_insert_with(|| serde_json::Value::String(text));
            }
        }
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
        chunks: DashMap::new(),
        virtual_documents: DashMap::new(),
        registry: Mutex::new(DelegateRegistry::new(config.languages)),
        completion_cache: Arc::new(DashMap::new()),
    };

    let (service, socket) = tower_lsp::LspService::new(|_client| server);

    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
