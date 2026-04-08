use loom_parse::language_at_position;
use tower_lsp::lsp_types::{DocumentRangeFormattingParams, TextEdit};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TextEdit>>> {
        let uri = params.text_document.uri.clone();
        let start_line = params.range.start.line;
        let end_line = params.range.end.line;

        // Only forward if the range falls within a single chunk.
        let same_language = {
            let Some(chunks) = self.chunks.get(&uri) else {
                return Ok(None);
            };
            let start_lang = language_at_position(&chunks, start_line);
            let end_lang = language_at_position(&chunks, end_line);
            start_lang.is_some() && start_lang == end_lang
        };

        if !same_language {
            return Ok(None);
        }

        let result = self
            .forward_request("textDocument/rangeFormatting", params, &uri, start_line)
            .await?;
        Ok(serde_json::from_value(result).unwrap_or(None))
    }
}
