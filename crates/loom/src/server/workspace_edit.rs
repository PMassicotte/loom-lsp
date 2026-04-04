use std::collections::HashMap;
use tower_lsp::lsp_types::{DocumentChangeOperation, DocumentChanges, Url, WorkspaceEdit};

/// Rewrites all occurrences of `vdoc_uri` in a `WorkspaceEdit` to `host_uri`,
/// so that edits targeting a virtual document are applied to the host file.
pub(super) fn rewrite_workspace_edit(
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
