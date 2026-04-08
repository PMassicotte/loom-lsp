use tower_lsp::lsp_types::{SignatureHelp, SignatureHelpParams};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> tower_lsp::jsonrpc::Result<Option<SignatureHelp>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let line = params.text_document_position_params.position.line;
        let result = self
            .forward_request("textDocument/signatureHelp", params, &uri, line)
            .await?;
        Ok(serde_json::from_value(result).unwrap_or(None))
    }
}
