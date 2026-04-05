use std::sync::Arc;

use tower_lsp::lsp_types::{
    CompletionParams, CompletionResponse, Position, TextDocumentIdentifier,
    TextDocumentPositionParams,
};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_completion(
        &self,
        params: CompletionParams,
    ) -> tower_lsp::jsonrpc::Result<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let line = params.text_document_position.position.line;
        let character = params.text_document_position.position.character;

        tracing::info!("Completion request at {}:{}:{}", uri, line, character);

        let Some((sender, vdoc_uri, language)) = self.resolve_delegate(uri, line).await else {
            return Ok(None);
        };

        tracing::info!(
            "forwarding completion to delegate: language={language} line={line} char={character}"
        );

        let params_value = serde_json::to_value(CompletionParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: vdoc_uri },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        })
        .map_err(|e| tower_lsp::jsonrpc::Error::invalid_params(e.to_string()))?;

        // Always send the request in a background task so it survives $/cancelRequest, which
        // cancels the tower-lsp handler future. The background task updates the cache when the
        // LSP responds, regardless of what happens here.
        let (fresh_tx, fresh_rx) = tokio::sync::oneshot::channel();

        let cache = Arc::clone(&self.completion_cache);

        let lang = language.clone();

        tokio::spawn(async move {
            if let Ok(raw) = sender
                .send_request("textDocument/completion", params_value)
                .await
            {
                let result = raw["result"].clone();

                if !result.is_null() {
                    cache.insert(lang, result.clone());
                    let _ = fresh_tx.send(result);
                }
            }
        });

        // Stale-while-revalidate: if we have a cached result return it immediately and
        // let the background task warm the cache for the next request. This avoids any
        // arbitrary timeout and works equally well for fast and slow LSPs.
        if let Some(cached) = self.completion_cache.get(&language) {
            let mut value = cached.clone();
            strip_text_edits(&mut value);
            tracing::debug!("completion: stale cache for {language}");
            return Ok(serde_json::from_value(value).ok().flatten());
        }

        // No cache yet (first request for this language), wait for the fresh result.
        if let Ok(result) = fresh_rx.await {
            tracing::debug!("completion: fresh result for {language}");
            return Ok(serde_json::from_value(result).ok().flatten());
        }

        tracing::debug!("completion: no result for {language}");
        Ok(None)
    }
}

/// Strips `textEdit` from every completion item, promoting `textEdit.newText` to `insertText`
/// when not already set. Stale position ranges from cached responses corrupt insertions when
/// served at a different cursor position.
fn strip_text_edits(value: &mut serde_json::Value) {
    let items = if let Some(arr) = value.as_array_mut() {
        arr
    } else if let Some(arr) = value.get_mut("items").and_then(|v| v.as_array_mut()) {
        arr
    } else {
        return;
    };
    for item in items.iter_mut() {
        let new_text = item
            .get("textEdit")
            .and_then(|te| {
                te.get("newText")
                    .or_else(|| te.get("insert").and_then(|r| r.get("newText")))
            })
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let Some(obj) = item.as_object_mut() {
            obj.remove("textEdit");
            obj.remove("additionalTextEdits");
            if let Some(text) = new_text {
                obj.entry("insertText")
                    .or_insert_with(|| serde_json::Value::String(text));
            }
        }
    }
}
