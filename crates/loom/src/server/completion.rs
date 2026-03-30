use std::sync::Arc;

use loom_delegate::TransportSender;
use loom_parse::language_at_position;
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

        // Hold each DashMap ref only briefly — releasing the shard lock before any await prevents
        // did_change's synchronous insert from blocking Tokio threads.
        let language = {
            let chunks = match self.chunks.get(uri) {
                Some(c) => c,
                None => return Ok(None),
            };
            match language_at_position(&chunks, line) {
                Some(l) => l.to_string(),
                None => return Ok(None),
            }
        };
        let vdoc_uri = match self.virtual_documents.get(uri) {
            Some(vdocs) => match vdocs.iter().find(|v| v.language == language) {
                Some(vdoc) => vdoc.uri.clone(),
                None => return Ok(None),
            },
            None => return Ok(None),
        };

        tracing::info!(
            "forwarding completion to delegate: language={language} line={line} char={character}"
        );

        // Never spawn delegates inside completion, did_open handles that.
        let sender: TransportSender = {
            let mut registry = self.registry.lock().await;
            match registry.get_if_alive(&language).await {
                Some(handle) => handle.lock().await.sender(),
                None => {
                    tracing::info!("completion: delegate for {language} not ready yet");
                    return Ok(None);
                }
            }
        };

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

        // Always send the request in a background task so it survives neovim's
        // $/cancelRequest, which cancels the tower-lsp handler future. The background
        // task updates the cache when the LSP responds, regardless of what happens here.
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

        // Wait briefly for the background task's result. Fast LSPs (pyright ~50ms) respond
        // in time and we return fresh completions. Slow LSPs (Julia ~2s) time out and we
        // fall back to the stale cache, which the background task will eventually update.
        if let Ok(Ok(result)) =
            tokio::time::timeout(std::time::Duration::from_millis(200), fresh_rx).await
        {
            tracing::info!("completion: fresh result for {language}");
            return Ok(serde_json::from_value(result).ok().flatten());
        }

        // Stale cache fallback. Strip textEdit since cursor positions may be stale.
        if let Some(cached) = self.completion_cache.get(&language) {
            let mut value = cached.clone();
            strip_text_edits(&mut value);
            tracing::info!("completion: stale cache for {language}");
            return Ok(serde_json::from_value(value).ok().flatten());
        }

        tracing::info!("completion: no result for {language}");
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
