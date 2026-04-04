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

        let params_value = serde_json::to_value(RenameParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: vdoc_uri.clone(),
                },
                position: Position { line, character },
            },
            new_name: params.new_name,
            work_done_progress_params: Default::default(),
        })
        .map_err(|e| {
            tracing::error!("Failed to serialize rename params: {e}");
            tower_lsp::jsonrpc::Error::invalid_params(e.to_string())
        })?;

        let response = sender
            .send_request("textDocument/rename", params_value)
            .await;

        match response {
            Ok(raw) => {
                let result = raw
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);

                let edit = serde_json::from_value::<Option<WorkspaceEdit>>(result).unwrap_or(None);

                Ok(edit.map(|e| rewrite_workspace_edit(e, &vdoc_uri, &uri)))
            }
            Err(e) => {
                tracing::error!("Failed to get rename response: {e}");
                Ok(None)
            }
        }
    }
}

/// Rewrites a `WorkspaceEdit` from a delegate to replace any references to the virtual document
/// URI with the host document URI. This is necessary because the delegate operates on the virtual document, but the edits need to be applied to the host document.
fn rewrite_workspace_edit(
    mut edit: WorkspaceEdit,
    vdoc_uri: &Url,
    host_uri: &Url,
) -> WorkspaceEdit {
    // First case: `changes` field, which is a simple map of URI to edits.
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

    // Second case: `document_changes` field, which can contain either edits or operations.
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
