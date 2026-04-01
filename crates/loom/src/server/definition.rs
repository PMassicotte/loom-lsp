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

        let params_value = serde_json::to_value(GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier {
                    uri: vdoc_uri.clone(),
                },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        })
        .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;

        let response = sender
            .send_request("textDocument/definition", params_value)
            .await;

        match response {
            Ok(raw) => {
                let result = raw
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let resp: Option<GotoDefinitionResponse> =
                    serde_json::from_value(result).unwrap_or(None);
                Ok(resp.map(|r| rewrite_uris(r, &vdoc_uri, &uri)))
            }
            Err(e) => {
                tracing::error!("definition request failed: {e}");
                Ok(None)
            }
        }
    }
}

/// Rewrite virtual doc URIs back to the host URI so the editor jumps
/// to the .qmd file instead of the non-existent virtual doc on disk.
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
