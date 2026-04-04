use serde_json::json;
use tower_lsp::lsp_types::DidSaveTextDocumentParams;

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri;

        tracing::info!("Document saved: {}", uri);

        let Some(vdocs) = self.virtual_documents.get(&uri).map(|v| v.clone()) else {
            return;
        };

        let mut handles = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for vdoc in &vdocs {
                if let Some(handle) = registry.get_if_alive(&vdoc.language).await {
                    handles.push((
                        handle,
                        vdoc.uri.clone(),
                        vdoc.language.clone(),
                        vdoc.content.clone(),
                    ));
                }
            }
        }

        for (handle, vdoc_uri, language, content) in handles {
            let sender = handle.lock().await.sender();

            // Close and reopen the virtual doc so the delegate does a fresh analysis
            // from the current content, instead of firing cached diagnostics.
            let close = json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didClose",
                "params": { "textDocument": { "uri": vdoc_uri } }
            });
            let open = json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": {
                    "textDocument": {
                        "uri": vdoc_uri,
                        "languageId": language,
                        "version": 0,
                        "text": content
                    }
                }
            });

            if let Err(e) = sender.send_message(close).await {
                tracing::warn!("failed to send didClose to {language} delegate: {e}");
                continue;
            }
            if let Err(e) = sender.send_message(open).await {
                tracing::warn!("failed to send didOpen to {language} delegate: {e}");
            } else {
                tracing::info!("refreshed {language} delegate on save");
            }
        }
    }
}
