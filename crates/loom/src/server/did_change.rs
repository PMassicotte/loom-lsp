use crate::server::spawn_delegate::DelegateContext;
use loom_parse::parse_qmd;
use loom_vdoc::build_virtual_docs;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, Range};

use super::LoomServer;
use super::spawn_delegate::spawn_delegate;

impl LoomServer {
    pub(crate) async fn handle_did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.content_changes[0].text.clone();

        tracing::info!("Document changed: {} ({} bytes)", uri, text.len());

        let parsed_chunks = match parse_qmd(&text) {
            Ok(chunks) => chunks,
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
                return;
            }
        };
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

        // Remove stale reverse index entries before inserting new ones.
        if let Some(old_vdocs) = self.virtual_documents.get(&uri) {
            for old_vdoc in old_vdocs.iter() {
                self.reverse_vdoc_index.remove(&old_vdoc.uri);
            }
        }

        self.chunks.insert(uri.clone(), parsed_chunks);
        self.virtual_documents.insert(uri.clone(), vdocs.clone());
        for vdoc in &vdocs {
            self.reverse_vdoc_index
                .insert(vdoc.uri.clone(), (uri.clone(), vdoc.clone()));
        }

        // Spawn delegates for languages that are missing or dead.
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

        // Update already-running delegates.
        let mut handles = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for vdoc in &vdocs {
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
}
