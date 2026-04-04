use loom_delegate::TransportSender;
use loom_parse::language_at_position;
use serde::de::DeserializeOwned;
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

    /// Serialize `params` and send `method` to the given delegate sender,
    /// returning the deserialized response. URI rewriting (if needed) is left
    /// to the caller.
    pub(crate) async fn send_to_delegate<P, R>(
        &self,
        method: &str,
        sender: TransportSender,
        params: P,
    ) -> tower_lsp::jsonrpc::Result<Option<R>>
    where
        P: serde::Serialize,
        R: DeserializeOwned,
    {
        let params_value = serde_json::to_value(params)
            .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;

        match sender.send_request(method, params_value).await {
            Ok(raw) => {
                let result = raw
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                Ok(serde_json::from_value::<Option<R>>(result).unwrap_or(None))
            }
            Err(e) => {
                tracing::error!("{method} request failed: {e}");
                Ok(None)
            }
        }
    }
}
