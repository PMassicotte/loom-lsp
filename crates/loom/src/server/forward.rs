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
}
