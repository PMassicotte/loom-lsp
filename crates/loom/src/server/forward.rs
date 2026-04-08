use loom_delegate::TransportSender;
use loom_parse::language_at_position;
use tower_lsp::lsp_types::Url;

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn resolve_delegate(
        &self,
        uri: &Url,
        line: u32,
    ) -> Option<(TransportSender, Url, String)> {
        let language = {
            let chunks = self.chunks.get(uri)?;
            language_at_position(&chunks, line)?.to_string()
        };
        let vdoc_uri = {
            let vdocs = self.virtual_documents.get(uri)?;
            vdocs.iter().find(|v| v.language == language)?.uri.clone()
        };
        let sender = {
            let mut registry = self.registry.lock().await;
            let handle = registry.get_if_alive(&language).await?;
            handle.lock().await.sender()
        };
        Some((sender, vdoc_uri, language))
    }

    /// Generic LSP request forwarder:
    /// - Rewrites host URI → vdoc URI in outgoing params
    /// - Rewrites vdoc URI → host URI in incoming response
    /// - Returns the raw `result` field; callers deserialize into their expected type.
    ///
    /// Returns `Value::Null` when no delegate is reachable.
    pub(crate) async fn forward_request<P: serde::Serialize>(
        &self,
        method: &str,
        params: P,
        uri: &Url,
        line: u32,
    ) -> tower_lsp::jsonrpc::Result<serde_json::Value> {
        let Some((sender, vdoc_uri, _)) = self.resolve_delegate(uri, line).await else {
            return Ok(serde_json::Value::Null);
        };

        let raw = serde_json::to_value(params)
            .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;

        let raw = rewrite_uris_in_json(raw, uri.as_str(), vdoc_uri.as_str());

        // Send the request to the delegate and rewrite URIs in the response back to the host URI.
        match sender.send_request(method, raw).await {
            Ok(resp) => {
                let result = resp
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                Ok(rewrite_uris_in_json(
                    result,
                    vdoc_uri.as_str(),
                    uri.as_str(),
                ))
            }
            Err(e) => {
                tracing::error!("{method} request failed: {e}");
                Ok(serde_json::Value::Null)
            }
        }
    }
}

/// Recursively replaces every JSON string exactly equal to `from` with `to`.
/// Both object keys and string values are rewritten — keys matter because
/// `WorkspaceEdit.changes` uses URIs as keys, not values.
fn rewrite_uris_in_json(value: serde_json::Value, from: &str, to: &str) -> serde_json::Value {
    match value {
        serde_json::Value::String(s) if s == from => serde_json::Value::String(to.to_string()),
        serde_json::Value::Array(arr) => serde_json::Value::Array(
            arr.into_iter()
                .map(|v| rewrite_uris_in_json(v, from, to))
                .collect(),
        ),
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .map(|(k, v)| {
                    let k = if k == from { to.to_string() } else { k };
                    (k, rewrite_uris_in_json(v, from, to))
                })
                .collect(),
        ),
        other => other,
    }
}
