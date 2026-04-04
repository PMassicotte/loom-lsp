use tower_lsp::lsp_types::{
    GotoDefinitionParams, GotoDefinitionResponse, Position, TextDocumentIdentifier,
    TextDocumentPositionParams,
};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let TextDocumentPositionParams {
            text_document: TextDocumentIdentifier { uri },
            position: Position { line, character },
        } = params.text_document_position_params;

        tracing::info!(
            "Definition request received for {} at line {}, character {}",
            uri,
            line,
            character
        );

        let Some((sender, vdoc_uri, _)) = self.resolve_delegate(&uri, line).await else {
            return Ok(None);
        };

        let resp: Option<GotoDefinitionResponse> = self
            .send_to_delegate(
                "textDocument/definition",
                sender,
                GotoDefinitionParams {
                    text_document_position_params: TextDocumentPositionParams {
                        text_document: TextDocumentIdentifier {
                            uri: vdoc_uri.clone(),
                        },
                        position: Position { line, character },
                    },
                    work_done_progress_params: Default::default(),
                    partial_result_params: Default::default(),
                },
            )
            .await?;

        Ok(resp.map(|r| rewrite_uris(r, &vdoc_uri, &uri)))
    }
}

fn rewrite_uris(
    resp: GotoDefinitionResponse,
    vdoc_uri: &tower_lsp::lsp_types::Url,
    host_uri: &tower_lsp::lsp_types::Url,
) -> GotoDefinitionResponse {
    match resp {
        GotoDefinitionResponse::Scalar(mut loc) => {
            if loc.uri == *vdoc_uri {
                loc.uri = host_uri.clone();
            }
            GotoDefinitionResponse::Scalar(loc)
        }
        GotoDefinitionResponse::Array(mut locs) => {
            for loc in &mut locs {
                if loc.uri == *vdoc_uri {
                    loc.uri = host_uri.clone();
                }
            }
            GotoDefinitionResponse::Array(locs)
        }
        GotoDefinitionResponse::Link(mut links) => {
            for link in &mut links {
                if link.target_uri == *vdoc_uri {
                    link.target_uri = host_uri.clone();
                }
            }
            GotoDefinitionResponse::Link(links)
        }
    }
}
