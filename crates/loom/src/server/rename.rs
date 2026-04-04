use tower_lsp::lsp_types::{
    Position, RenameParams, TextDocumentIdentifier, TextDocumentPositionParams, WorkspaceEdit,
};

use super::LoomServer;
use super::workspace_edit::rewrite_workspace_edit;

impl LoomServer {
    pub(crate) async fn handle_rename(
        &self,
        params: RenameParams,
    ) -> tower_lsp::jsonrpc::Result<Option<WorkspaceEdit>> {
        let TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        } = params.text_document_position;

        tracing::info!(
            "Rename request received for {} at line {}, character {}",
            uri,
            line,
            character
        );

        let Some((sender, vdoc_uri, _)) = self.resolve_delegate(&uri, line).await else {
            return Ok(None);
        };

        let edit: Option<WorkspaceEdit> = self
            .send_to_delegate(
                "textDocument/rename",
                sender,
                RenameParams {
                    text_document_position: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: vdoc_uri.clone(),
                        },
                        position: Position { line, character },
                    },
                    new_name: params.new_name,
                    work_done_progress_params: Default::default(),
                },
            )
            .await?;

        Ok(edit.map(|e| rewrite_workspace_edit(e, &vdoc_uri, &uri)))
    }
}
