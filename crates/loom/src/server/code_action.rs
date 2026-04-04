use super::workspace_edit::rewrite_workspace_edit;
use tower_lsp::lsp_types::{
    CodeActionOrCommand, CodeActionParams, CodeActionResponse, TextDocumentIdentifier,
};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_code_action(
        &self,
        params: CodeActionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CodeActionResponse>> {
        let uri = params.text_document.uri.clone();
        let line = params.range.start.line;

        let Some((sender, vdoc_uri, language)) = self.resolve_delegate(&uri, line).await else {
            return Ok(None);
        };

        // WARNING: LanguageServer.jl has a bug where its codeAction handler returns an invalid
        // type, causing the JSONRPC layer to throw and crash the entire server process.
        if language == "julia" {
            return Ok(None);
        }

        let response: Option<CodeActionResponse> = self
            .send_to_delegate(
                "textDocument/codeAction",
                sender,
                CodeActionParams {
                    text_document: TextDocumentIdentifier {
                        uri: vdoc_uri.clone(),
                    },
                    range: params.range,
                    context: params.context,
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
            )
            .await?;

        Ok(response.map(|actions| {
            actions
                .into_iter()
                .map(|action| match action {
                    CodeActionOrCommand::CodeAction(mut ca) => {
                        ca.edit = ca.edit.map(|e| rewrite_workspace_edit(e, &vdoc_uri, &uri));
                        CodeActionOrCommand::CodeAction(ca)
                    }
                    other => other,
                })
                .collect()
        }))
    }
}
