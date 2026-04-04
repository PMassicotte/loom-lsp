use tower_lsp::lsp_types::{
    Hover, HoverParams, Position, TextDocumentIdentifier, TextDocumentPositionParams,
};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_hover(
        &self,
        params: HoverParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
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

        self.send_to_delegate(
            "textDocument/hover",
            sender,
            HoverParams {
                text_document_position_params: TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: vdoc_uri },
                    position: Position { line, character },
                },
                work_done_progress_params: Default::default(),
            },
        )
        .await
    }
}
