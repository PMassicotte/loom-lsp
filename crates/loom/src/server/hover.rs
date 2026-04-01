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

        let Some((sender, vdoc_uri, _)) = self.resolve_delegate(&uri, line).await else {
            return Ok(None);
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
