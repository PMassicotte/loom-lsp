use tower_lsp::lsp_types::{Hover, HoverParams};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_hover(
        &self,
        params: HoverParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Hover>> {
        let uri = params
            .text_document_position_params
            .text_document
            .uri
            .clone();
        let line = params.text_document_position_params.position.line;
        let result = self
            .forward_request("textDocument/hover", params, &uri, line)
            .await?;
        Ok(serde_json::from_value(result).unwrap_or(None))
    }
}
