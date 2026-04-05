use tower_lsp::lsp_types::{
    Position, SignatureHelp, SignatureHelpParams, TextDocumentIdentifier,
    TextDocumentPositionParams,
};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> tower_lsp::jsonrpc::Result<Option<SignatureHelp>> {
        let TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        } = params.text_document_position_params;

        tracing::debug!(
            "Signature help request received for {} at line {}, character {}",
            uri,
            line,
            character
        );

        let Some((sender, vdoc_uri, _)) = self.resolve_delegate(&uri, line).await else {
            tracing::debug!("signature_help: no delegate for {} at line {}", uri, line);
            return Ok(None);
        };

        self.send_to_delegate(
            "textDocument/signatureHelp",
            sender,
            SignatureHelpParams {
                context: None,
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
