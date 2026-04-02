use loom_parse::DocumentParser;
use loom_vdoc::build_virtual_docs;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, Range};

use crate::server::spawn_delegate::DelegateContext;

use super::LoomServer;
use super::spawn_delegate::spawn_delegate;

impl LoomServer {
    pub(crate) async fn handle_did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        tracing::info!("Document opened: {} ({} bytes)", uri, text.len());

        let (doc_parser, parsed_chunks) = match DocumentParser::new(&text) {
            Ok((parser, chunks)) => (parser, chunks),
            Err(e) => {
                tracing::error!("failed to parse {}: {e}", uri);
                self.client
                    .publish_diagnostics(
                        uri.clone(),
                        vec![Diagnostic {
                            range: Range::default(),
                            severity: Some(DiagnosticSeverity::WARNING),
                            source: Some("loom".into()),
                            message: format!("Loom failed to parse document: {e}"),
                            ..Default::default()
                        }],
                        None,
                    )
                    .await;
                self.chunks.insert(uri.clone(), Vec::new());
                self.virtual_documents.insert(uri, Vec::new());
                return;
            }
        };
        self.parsers.insert(uri.clone(), Mutex::new(doc_parser));
        let vdocs = build_virtual_docs(&parsed_chunks, text.split('\n').count() as u32, &uri);
        tracing::info!("built {} virtual docs for {}", vdocs.len(), uri);

        self.chunks.insert(uri.clone(), parsed_chunks);
        self.virtual_documents.insert(uri.clone(), vdocs.clone());
        for vdoc in &vdocs {
            self.reverse_vdoc_index
                .insert(vdoc.uri.clone(), (uri.clone(), vdoc.clone()));
        }

        // Collect which languages need a new delegate spawned.
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

        // Spawn each delegate init as an independent background task so fast delegates
        // (pyright ~200ms) don't wait for slow ones (Julia ~5s).
        for (lang, cmd, root_uri) in to_spawn {
            spawn_delegate(
                lang,
                cmd,
                root_uri,
                vdocs.clone(),
                DelegateContext {
                    registry: self.registry.clone(),
                    client: self.client.clone(),
                    reverse_vdoc_index: self.reverse_vdoc_index.clone(),
                    diagnostics_store: self.diagnostics_store.clone(),
                },
            );
        }

        // For languages that already have a running delegate, send didOpen immediately.
        let mut handles = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for vdoc in &vdocs {
                if registry.is_failed(&vdoc.language) {
                    continue;
                }
                if let Some(handle) = registry.get_if_alive(&vdoc.language).await {
                    handles.push((
                        handle,
                        vdoc.uri.clone(),
                        vdoc.language.clone(),
                        vdoc.content.clone(),
                    ))
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
    }
}
