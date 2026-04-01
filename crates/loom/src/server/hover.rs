use loom_parse::language_at_position;
use tower_lsp::lsp_types::{
    Hover, HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams,
};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_hover(
        &self,
        params: HoverParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        // Destructure the params to extract the URI, line, and character.
        let TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        } = params.text_document_position_params;

        tracing::info!(
            "Hover request received for {} at line {}, character {}",
            uri,
            line,
            character
        );

        let language = {
            let chunks = match self.chunks.get(&uri) {
                Some(c) => c,
                None => return Ok(None),
            };

            match language_at_position(&chunks, line) {
                Some(l) => l.to_string(),
                None => return Ok(None),
            }
        };

        let vdoc_uri = match self.virtual_documents.get(&uri) {
            Some(vdocs) => match vdocs.iter().find(|v| v.language == language) {
                Some(vdoc) => vdoc.uri.clone(),
                None => return Ok(None),
            },
            None => return Ok(None),
        };

        let sender = {
            let mut registry = self.registry.lock().await;
            match registry.get_if_alive(&language).await {
                Some(handle) => handle.lock().await.sender(),
                None => {
                    tracing::info!("hover: delegate for {language} not ready yet");
                    return Ok(None);
                }
            }
        };

        let params_value = serde_json::to_value(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: vdoc_uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
        })
        .map_err(|e| {
            tracing::error!("Failed to serialize hover params: {e}");
            tower_lsp::jsonrpc::Error::invalid_params(e.to_string())
        })?;

        let response = sender
            .send_request("textDocument/hover", params_value)
            .await;

        match response {
            Ok(raw) => {
                let result = raw
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                Ok(serde_json::from_value::<Option<Hover>>(result).unwrap_or(None))
            }
            Err(e) => {
                tracing::error!("Failed to get hover response: {e}");
                Ok(None)
            }
        }
    }
}
