use tower_lsp::lsp_types::{RenameParams, WorkspaceEdit};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_rename(
        &self,
        params: RenameParams,
    ) -> tower_lsp::jsonrpc::Result<Option<WorkspaceEdit>> {
        let uri = params.text_document_position.text_document.uri.clone();
        let line = params.text_document_position.position.line;
        let result = self
            .forward_request("textDocument/rename", params, &uri, line)
            .await?;
        Ok(serde_json::from_value(result).unwrap_or(None))
    }
}
