use tower_lsp::lsp_types::{CodeActionParams, CodeActionResponse};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_code_action(
        &self,
        params: CodeActionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri.clone();
        let line = params.range.start.line;
        let result = self
            .forward_request("textDocument/codeAction", params, &uri, line)
            .await?;
        Ok(serde_json::from_value(result).unwrap_or(None))
    }
}
