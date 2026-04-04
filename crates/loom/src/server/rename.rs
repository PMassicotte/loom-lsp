use std::collections::HashMap;
use tower_lsp::lsp_types::{
    DocumentChangeOperation, DocumentChanges, Position, RenameParams, TextDocumentIdentifier,
    TextDocumentPositionParams, Url, WorkspaceEdit,
};

use super::LoomServer;

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

fn rewrite_workspace_edit(
    mut edit: WorkspaceEdit,
    vdoc_uri: &Url,
    host_uri: &Url,
) -> WorkspaceEdit {
    if let Some(changes) = edit.changes.take() {
        edit.changes = Some(
            changes
                .into_iter()
                .map(|(uri, edits)| {
                    let uri = if uri == *vdoc_uri {
                        host_uri.clone()
                    } else {
                        uri
                    };
                    (uri, edits)
                })
                .collect::<HashMap<_, _>>(),
        );
    }
    if let Some(doc_changes) = edit.document_changes.take() {
        edit.document_changes = Some(match doc_changes {
            DocumentChanges::Edits(mut edits) => {
                for e in &mut edits {
                    if e.text_document.uri == *vdoc_uri {
                        e.text_document.uri = host_uri.clone();
                    }
                }
                DocumentChanges::Edits(edits)
            }
            DocumentChanges::Operations(mut ops) => {
                for op in &mut ops {
                    if let DocumentChangeOperation::Edit(text_edit) = op
                        && text_edit.text_document.uri == *vdoc_uri
                    {
                        text_edit.text_document.uri = host_uri.clone();
                    }
                }
                DocumentChanges::Operations(ops)
            }
        });
    }
    edit
}
