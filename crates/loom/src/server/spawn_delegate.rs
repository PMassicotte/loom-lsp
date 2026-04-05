use std::collections::HashMap;
use std::sync::Arc;

use dashmap::DashMap;
use loom_vdoc::VirtualDocument;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::{self, Url};

use crate::registry::DelegateRegistry;

pub(crate) struct DelegateContext {
    pub registry: Arc<Mutex<DelegateRegistry>>,
    pub client: tower_lsp::Client,
    pub reverse_vdoc_index: Arc<DashMap<Url, (Url, VirtualDocument)>>,
    pub diagnostics_store: Arc<DashMap<Url, HashMap<String, Vec<lsp_types::Diagnostic>>>>,
}

/// Spawns a delegate LSP server for `lang` in a background task. The delegate is initialized,
/// registered, and then receives `didOpen` for each matching virtual document. A notification
/// listener is also spawned to fan-in diagnostics from the delegate.
pub(crate) fn spawn_delegate(
    lang: String,
    cmd: Vec<String>,
    root_uri: Option<Url>,
    vdocs: Vec<VirtualDocument>,
    ctx: DelegateContext,
) {
    tokio::spawn(async move {
        let cmd_str = cmd.join(" ");

        let mut delegate = match loom_delegate::DelegateServer::spawn(&cmd) {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("failed to spawn `{cmd_str}`: {e}");

                if ctx.registry.lock().await.mark_failed(lang.clone()) {
                    ctx.client.show_message(
                        lsp_types::MessageType::ERROR,
                        format!("{lang} LSP failed 3 times and will not be retried; check your config"),
                    ).await;
                }

                return;
            }
        };

        if let Err(e) = delegate.initialize(root_uri).await {
            tracing::warn!("failed to initialize `{cmd_str}`: {e}");

            if ctx.registry.lock().await.mark_failed(lang.clone()) {
                ctx.client
                    .show_message(
                        lsp_types::MessageType::ERROR,
                        format!(
                            "{lang} LSP failed 3 times and will not be retried; check your config"
                        ),
                    )
                    .await;
            }

            return;
        }

        // Take notification rx before inserting into registry.
        let rx = delegate.take_notification_rx();

        // Insert into registry and send didOpen for matching vdocs.
        {
            let mut reg = ctx.registry.lock().await;
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

                    // O(1) reverse lookup: virtual_uri -> (host_uri, VirtualDocument)
                    let (host_uri, vdoc) =
                        match ctx.reverse_vdoc_index.get(&params.uri).map(|e| e.clone()) {
                            Some(pair) => pair,
                            None => {
                                tracing::debug!("no host doc for {} (stale vdoc?)", params.uri);
                                continue;
                            }
                        };

                    // Filter out diagnostics on padding lines
                    let filtered: Vec<tower_lsp::lsp_types::Diagnostic> = params
                        .diagnostics
                        .into_iter()
                        .filter(|d| vdoc.is_live(d.range.start.line))
                        .collect();

                    tracing::debug!(
                        "publishDiagnostics: {} -> {} lang={} ({} diagnostics)",
                        params.uri,
                        host_uri,
                        vdoc.language,
                        filtered.len()
                    );

                    ctx.diagnostics_store
                        .entry(host_uri.clone())
                        .or_default()
                        .insert(vdoc.language.clone(), filtered);

                    let all: Vec<tower_lsp::lsp_types::Diagnostic> = ctx
                        .diagnostics_store
                        .get(&host_uri)
                        .map(|entry| entry.values().flatten().cloned().collect())
                        .unwrap_or_default();

                    ctx.client.publish_diagnostics(host_uri, all, None).await;
                }
            });
        }
    });
}
