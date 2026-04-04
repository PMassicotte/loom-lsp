use std::collections::HashSet;

use crate::server::spawn_delegate::DelegateContext;
use loom_parse::{CodeChunk, DocumentParser};
use loom_vdoc::build_virtual_docs;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::DidChangeTextDocumentParams;

use super::LoomServer;
use super::spawn_delegate::spawn_delegate;

impl LoomServer {
    pub(crate) async fn handle_did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;

        let Some(change) = params.content_changes.into_iter().next() else {
            tracing::warn!("did_change for {} had empty contentChanges; ignoring", uri);
            return;
        };

        let text = change.text;

        tracing::info!("Document changed: {} ({} bytes)", uri, text.len());

        let old_chunks: Vec<CodeChunk> =
            self.chunks.get(&uri).map(|c| c.clone()).unwrap_or_default();

        let parsed_chunks = if let Some(entry) = self.parsers.get(&uri) {
            match entry.value().lock().await.update(&text) {
                Ok(chunks) => chunks,
                Err(e) => {
                    tracing::error!("incremental parse failed for {}: {e}", uri);
                    self.publish_parse_error(
                        uri.clone(),
                        format!("Loom failed to parse document: {e}"),
                    )
                    .await;
                    return;
                }
            }
        } else {
            // No parser yet (e.g. change arrived before open); create one.
            match DocumentParser::new(&text) {
                Ok((parser, chunks)) => {
                    self.parsers.insert(uri.clone(), Mutex::new(parser));
                    chunks
                }
                Err(e) => {
                    tracing::error!("failed to parse {}: {e}", uri);
                    self.publish_parse_error(
                        uri.clone(),
                        format!("Loom failed to parse document: {e}"),
                    )
                    .await;
                    return;
                }
            }
        };

        let mut vdocs = build_virtual_docs(&parsed_chunks, text.split('\n').count() as u32, &uri);

        let changed_languages: HashSet<String> = {
            let mut changed = HashSet::new();
            for vdoc in &vdocs {
                let old_content: Vec<&CodeChunk> = old_chunks
                    .iter()
                    .filter(|c| c.language == vdoc.language)
                    .collect();
                let new_content: Vec<&CodeChunk> = parsed_chunks
                    .iter()
                    .filter(|c| c.language == vdoc.language)
                    .collect();
                if old_content != new_content {
                    changed.insert(vdoc.language.clone());
                }
            }
            changed
        };

        // Increment version only for languages that actually changed.
        if let Some(old_vdocs) = self.virtual_documents.get(&uri) {
            for vdoc in &mut vdocs {
                if let Some(old) = old_vdocs.iter().find(|v| v.language == vdoc.language) {
                    if changed_languages.contains(&vdoc.language) {
                        vdoc.version = old.version + 1;
                    } else {
                        vdoc.version = old.version;
                    }
                }
            }
        }

        tracing::debug!("built {} virtual docs for {}", vdocs.len(), uri);

        // Remove stale reverse index entries before inserting new ones.
        if let Some(old_vdocs) = self.virtual_documents.get(&uri) {
            for old_vdoc in old_vdocs.iter() {
                self.reverse_vdoc_index.remove(&old_vdoc.uri);
            }
        }

        self.chunks.insert(uri.clone(), parsed_chunks);
        self.virtual_documents.insert(uri.clone(), vdocs.clone());
        for vdoc in &vdocs {
            self.reverse_vdoc_index
                .insert(vdoc.uri.clone(), (uri.clone(), vdoc.clone()));
        }

        // Spawn delegates for languages that are missing or dead.
        let to_spawn: Vec<(String, Vec<String>, Option<tower_lsp::lsp_types::Url>)> = {
            let registry = self.registry.lock().await;
            vdocs
                .iter()
                .filter_map(|vdoc| {
                    registry
                        .spawn_params(&vdoc.language)
                        .map(|(cmd, root_uri)| (vdoc.language.clone(), cmd, root_uri))
                })
                .collect()
        };

        for (lang, cmd, root_uri) in to_spawn {
            spawn_delegate(
                lang,
                cmd,
                root_uri,
                vdocs.clone(),
                DelegateContext {
                    registry: self.registry.clone(),
                    client: self.client.clone(),
                    reverse_vdoc_index: self.reverse_vdoc_index.clone(),
                    diagnostics_store: self.diagnostics_store.clone(),
                },
            );
        }

        // Update already-running delegates.
        let mut handles = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for vdoc in &vdocs {
                if !changed_languages.contains(&vdoc.language) {
                    tracing::debug!("skipping delegate {} (unchanged)", vdoc.language);
                    continue;
                }
                if let Some(handle) = registry.get_if_alive(&vdoc.language).await {
                    handles.push((
                        handle,
                        vdoc.language.clone(),
                        vdoc.uri.clone(),
                        vdoc.version,
                        vdoc.content.clone(),
                    ));
                }
            }
        }

        for (handle, lang, vdoc_uri, version, content) in handles {
            if let Err(e) = handle
                .lock()
                .await
                .update_document(vdoc_uri.clone(), version, &content)
                .await
            {
                tracing::warn!("failed to update {} delegate for {}: {e}", lang, vdoc_uri);
            }
        }
    }
}
