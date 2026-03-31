use tower_lsp::lsp_types::DidCloseTextDocumentParams;

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;

        tracing::info!("Document closed: {}", uri);

        self.chunks.remove(&uri);
        self.diagnostics_store.remove(&uri);
        self.client
            .publish_diagnostics(uri.clone(), vec![], None)
            .await;

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
}
