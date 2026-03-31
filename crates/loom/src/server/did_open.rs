use loom_parse::parse_qmd;
use loom_vdoc::build_virtual_docs;
use tower_lsp::lsp_types::{Diagnostic, DiagnosticSeverity, DidOpenTextDocumentParams, Range};

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        tracing::info!("Document opened: {} ({} bytes)", uri, text.len());

        let parsed_chunks = match parse_qmd(&text) {
            Ok(chunks) => chunks,
            Err(e) => {
                tracing::error!("failed to parse {}: {e}", uri);
                self.client
                    .publish_diagnostics(
                        uri.clone(),
                        vec![Diagnostic {
                            range: Range::default(),
                            severity: Some(DiagnosticSeverity::WARNING),
                            source: Some("loom".into()),
                            message: format!("Loom failed to parse document: {e}"),
                            ..Default::default()
                        }],
                        None,
                    )
                    .await;
                self.chunks.insert(uri.clone(), Vec::new());
                self.virtual_documents.insert(uri, Vec::new());
                return;
            }
        };
        let vdocs = build_virtual_docs(&parsed_chunks, text.split('\n').count() as u32, &uri);
        tracing::info!("built {} virtual docs for {}", vdocs.len(), uri);

        self.chunks.insert(uri.clone(), parsed_chunks);
        self.virtual_documents.insert(uri, vdocs.clone());

        // Collect which languages need a new delegate spawned.
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

        // Spawn each delegate init as an independent background task so fast delegates
        // (pyright ~200ms) don't wait for slow ones (Julia ~5s).
        for (lang, cmd, root_uri) in to_spawn {
            let registry = self.registry.clone();
            let client = self.client.clone();
            let vdocs_map = self.virtual_documents.clone();
            let diagnostics_store = self.diagnostics_store.clone();
            let vdocs = vdocs.clone();

            tokio::spawn(async move {
                let cmd_str = cmd.join(" ");
                let mut delegate = match loom_delegate::DelegateServer::spawn(&cmd) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!("failed to spawn `{cmd_str}`: {e}");
                        registry.lock().await.mark_failed(lang);
                        return;
                    }
                };
                if let Err(e) = delegate.initialize(root_uri).await {
                    tracing::warn!("failed to initialize `{cmd_str}`: {e}");
                    registry.lock().await.mark_failed(lang);
                    return;
                }

                // Take notification rx before inserting into registry.
                let rx = delegate.take_notification_rx();

                // Insert into registry and send didOpen for matching vdocs.
                {
                    let mut reg = registry.lock().await;
                    reg.insert_ready(lang.clone(), delegate);

                    // Open virtual docs for this language on the new delegate.
                    if let Some(handle) = reg.get_if_alive(&lang).await {
                        for vdoc in &vdocs {
                            if vdoc.language == lang
                                && let Err(e) = handle
                                    .lock()
                                    .await
                                    .open_document(vdoc.uri.clone(), &vdoc.language, &vdoc.content)
                                    .await
                            {
                                tracing::warn!("failed to open virtual doc on delegate: {e}");
                            }
                        }
                    }
                }

                // Spawn notification listener for diagnostics.
                if let Some(mut rx) = rx {
                    tokio::spawn(async move {
                        while let Some(notif) = rx.recv().await {
                            if notif.method != "textDocument/publishDiagnostics" {
                                continue;
                            }

                            let params: tower_lsp::lsp_types::PublishDiagnosticsParams =
                                match serde_json::from_value(notif.params) {
                                    Ok(p) => p,
                                    Err(e) => {
                                        tracing::warn!("bad diagnostics params: {e}");
                                        continue;
                                    }
                                };

                            // Reverse-map: find which .qmd owns this virtual doc URI
                            let found = vdocs_map.iter().find_map(|entry| {
                                let host = entry.key().clone();
                                entry
                                    .value()
                                    .iter()
                                    .find(|vdoc| vdoc.uri == params.uri)
                                    .map(|vdoc| (host, vdoc.clone()))
                            });

                            let (host_uri, vdoc) = match found {
                                Some(pair) => pair,
                                None => {
                                    tracing::debug!("no host doc for {}", params.uri);
                                    continue;
                                }
                            };

                            // Filter out diagnostics on padding lines
                            let filtered: Vec<tower_lsp::lsp_types::Diagnostic> = params
                                .diagnostics
                                .into_iter()
                                .filter(|d| vdoc.is_live(d.range.start.line))
                                .collect();

                            tracing::info!(
                                "publishDiagnostics: {} -> {} lang={} ({} filtered)",
                                params.uri,
                                host_uri,
                                vdoc.language,
                                filtered.len()
                            );

                            diagnostics_store
                                .entry(host_uri.clone())
                                .or_default()
                                .insert(vdoc.language.clone(), filtered);

                            let all: Vec<tower_lsp::lsp_types::Diagnostic> = diagnostics_store
                                .get(&host_uri)
                                .map(|entry| entry.values().flatten().cloned().collect())
                                .unwrap_or_default();

                            tracing::info!("publishing merged: {} ({} total)", host_uri, all.len());
                            client.publish_diagnostics(host_uri, all, None).await;
                        }
                    });
                }
            });
        }

        // For languages that already have a running delegate, send didOpen immediately.
        let mut handles = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for vdoc in &vdocs {
                if registry.is_failed(&vdoc.language) {
                    continue;
                }
                if let Some(handle) = registry.get_if_alive(&vdoc.language).await {
                    handles.push((
                        handle,
                        vdoc.uri.clone(),
                        vdoc.language.clone(),
                        vdoc.content.clone(),
                    ))
                }
            }
        }
        for (handle, vdoc_uri, language, content) in handles {
            if let Err(e) = handle
                .lock()
                .await
                .open_document(vdoc_uri, &language, &content)
                .await
            {
                tracing::warn!("failed to open virtual doc on delegate: {e}");
            }
        }
    }
}
