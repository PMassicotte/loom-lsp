use loom_parse::parse_qmd;
use loom_vdoc::build_virtual_docs;
use tower_lsp::lsp_types::DidOpenTextDocumentParams;

use super::LoomServer;

impl LoomServer {
    pub(crate) async fn handle_did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri;
        let text = params.text_document.text;

        tracing::info!("Document opened: {} ({} bytes)", uri, text.len());

        let parsed_chunks = parse_qmd(&text).unwrap();
        let vdocs = build_virtual_docs(&parsed_chunks, text.split('\n').count() as u32, &uri);
        tracing::info!("built {} virtual docs for {}", vdocs.len(), uri);

        self.chunks.insert(uri.clone(), parsed_chunks);

        // Collect which languages need a new delegate spawned. Hold the registry lock only long
        // enough for this check not during the multi-second LSP initialize handshake.
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

        // Initialize all needed delegates concurrently, with no lock held.
        let init_futs = to_spawn
            .into_iter()
            .map(|(lang, cmd, root_uri)| async move {
                let cmd_str = cmd.join(" ");
                let mut delegate = match loom_delegate::DelegateServer::spawn(&cmd) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!("failed to spawn `{cmd_str}`: {e}");
                        return (lang, Err(()));
                    }
                };
                match delegate.initialize(root_uri).await {
                    Ok(()) => (lang, Ok(delegate)),
                    Err(e) => {
                        tracing::warn!("failed to initialize `{cmd_str}`: {e}");
                        (lang, Err(()))
                    }
                }
            });
        let init_results = futures::future::join_all(init_futs).await;

        // Insert results brief lock per insert.
        let mut handles = Vec::new();
        {
            let mut registry = self.registry.lock().await;
            for (lang, result) in init_results {
                match result {
                    Ok(delegate) => registry.insert_ready(lang, delegate),
                    Err(()) => registry.mark_failed(lang),
                }
            }
            for vdoc in &vdocs {
                if registry.is_failed(&vdoc.language) {
                    continue;
                }
                match registry.get_or_spawn(&vdoc.language).await {
                    Ok(handle) => handles.push((
                        handle,
                        vdoc.uri.clone(),
                        vdoc.language.clone(),
                        vdoc.content.clone(),
                    )),
                    Err(e) => tracing::warn!("failed to get delegate for {}: {e}", vdoc.language),
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

        self.virtual_documents.insert(uri, vdocs);
    }
}
