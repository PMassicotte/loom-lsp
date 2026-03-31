use loom_parse::parse_qmd;
use loom_vdoc::build_virtual_docs;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, DidChangeTextDocumentParams, Range};

use super::LoomServer;

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
}
