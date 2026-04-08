use tower_lsp::lsp_types::{GotoDefinitionParams, GotoDefinitionResponse};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let line = params.text_document_position_params.position.line;
        let result = self
            .forward_request("textDocument/definition", params, &uri, line)
            .await?;
        Ok(serde_json::from_value(result).unwrap_or(None))
    }
}
