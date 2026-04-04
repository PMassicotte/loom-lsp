use loom_parse::language_at_position;
use tower_lsp::lsp_types::{DocumentRangeFormattingParams, TextEdit};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> tower_lsp::jsonrpc::Result<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        let start_line = params.range.start.line;
        let end_line = params.range.end.line;

        tracing::debug!(
            "RangeFormatting request for {} lines {}..{}",
            uri,
            start_line,
            end_line
        );

        // Only forward if the range falls within a single chunk.
        let same_language = {
            let Some(chunks) = self.chunks.get(uri) else {
                tracing::debug!("RangeFormatting for {} but no chunks found; ignoring", uri);
                return Ok(None);
            };

            let start_lang = language_at_position(&chunks, start_line);

            let end_lang = language_at_position(&chunks, end_line);

            start_lang.is_some() && start_lang == end_lang
        };

        if !same_language {
            tracing::debug!(
                "RangeFormatting for {} lines {}..{}: range spans multiple languages; skipping",
                uri,
                start_line,
                end_line
            );
            return Ok(None);
        }

        let Some((sender, vdoc_uri, _)) = self.resolve_delegate(uri, start_line).await else {
            tracing::debug!(
                "RangeFormatting for {} line {}: no delegate resolved; skipping",
                uri,
                start_line
            );
            return Ok(None);
        };

        self.send_to_delegate(
            "textDocument/rangeFormatting",
            sender,
            DocumentRangeFormattingParams {
                text_document: tower_lsp::lsp_types::TextDocumentIdentifier { uri: vdoc_uri },
                range: params.range,
                options: params.options,
                work_done_progress_params: Default::default(),
            },
        )
        .await
    }
}
