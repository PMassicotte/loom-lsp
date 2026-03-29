use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use loom_config::LanguageConfig;
use loom_delegate::DelegateServer;
use tokio::sync::Mutex;
use tower_lsp::lsp_types::Url;

/// Manages the lifecycle of delegate LSP servers, one per language. Delegates are spawned lazily
/// on first use so that we don't start servers for languages that never appear in the workspace.
pub struct DelegateRegistry {
    delegates: HashMap<String, Arc<Mutex<DelegateServer>>>,
    failed: std::collections::HashSet<String>,
    configs: HashMap<String, LanguageConfig>,
    root_uri: Option<Url>,
}

impl DelegateRegistry {
    pub fn new(configs: HashMap<String, LanguageConfig>) -> Self {
        Self {
            delegates: HashMap::new(),
            failed: std::collections::HashSet::new(),
            configs,
            root_uri: None,
        }
    }

    pub fn is_failed(&self, language: &str) -> bool {
        self.failed.contains(language)
    }

    pub fn set_root_uri(&mut self, root_uri: Option<Url>) {
        self.root_uri = root_uri;
    }

    /// Returns `Some((command, root_uri))` if `language` needs to be spawned (not yet running and
    /// not failed), or `None` if it already has a delegate or is in the failed set. Used by
    /// callers that want to initialize delegates outside the registry lock.
    pub fn spawn_params(&self, language: &str) -> Option<(Vec<String>, Option<Url>)> {
        if self.failed.contains(language) || self.delegates.contains_key(language) {
            return None;
        }
        self.configs
            .get(language)
            .map(|cfg| (cfg.server_command.clone(), self.root_uri.clone()))
    }

    /// Inserts an already-initialized delegate. Call this after initializing outside the lock.
    pub fn insert_ready(&mut self, language: String, delegate: DelegateServer) {
        tracing::info!("delegate ready for {language}");
        self.delegates
            .insert(language, Arc::new(Mutex::new(delegate)));
    }

    /// Marks a language as permanently failed so future requests are skipped.
    pub fn mark_failed(&mut self, language: String) {
        self.failed.insert(language);
    }

    /// Returns the existing delegate handle if present, without spawning. Returns None if not
    /// yet spawned, failed, or dead (dead delegates are evicted so get_or_spawn can re-spawn).
    pub async fn get_if_alive(&mut self, language: &str) -> Option<Arc<Mutex<DelegateServer>>> {
        let handle = self.delegates.get(language)?;
        if handle.lock().await.is_alive() {
            Some(Arc::clone(handle))
        } else {
            tracing::warn!("delegate for {language} has died, evicting");
            self.delegates.remove(language);
            None
        }
    }

    /// Returns a handle to the delegate for `language`, spawning and initializing it first if it
    /// has not been started yet. The registry lock can be released after this call; use the
    /// returned `Arc<Mutex<DelegateServer>>` to perform operations on the delegate.
    pub async fn get_or_spawn(&mut self, language: &str) -> Result<Arc<Mutex<DelegateServer>>> {
        if self.failed.contains(language) {
            return Err(anyhow::anyhow!(
                "delegate for {language} previously failed to start"
            ));
        }

        // Evict a dead delegate (get_if_alive already does this, but get_or_spawn must also
        // handle the case where it's called without a prior get_if_alive check).
        if let Some(handle) = self.delegates.get(language)
            && !handle.lock().await.is_alive()
        {
            tracing::warn!("delegate for {language} has died, re-spawning");
            self.delegates.remove(language);
        }

        if !self.delegates.contains_key(language) {
            let config = self
                .configs
                .get(language)
                .ok_or_else(|| anyhow::anyhow!("no config for language: {language}"))?;

            let cmd = config.server_command.join(" ");

            let lang_root = self
                .root_uri
                .as_ref()
                .and_then(|root| config.find_root(root.to_file_path().ok()?.as_path()))
                .or_else(|| self.root_uri.as_ref().and_then(|u| u.to_file_path().ok()))
                .unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                });

            let mut delegate = DelegateServer::spawn(&config.server_command)
                .map_err(|e| anyhow::anyhow!("failed to spawn `{cmd}`: {e}"))?;

            if let Err(e) = delegate
                .initialize(Url::from_file_path(&lang_root).ok())
                .await
            {
                self.failed.insert(language.to_string());
                return Err(anyhow::anyhow!("failed to initialize `{cmd}`: {e}"));
            }

            tracing::info!("delegate ready for {language}");
            self.delegates
                .insert(language.to_string(), Arc::new(Mutex::new(delegate)));
        }

        Ok(Arc::clone(self.delegates.get(language).unwrap()))
    }

    pub async fn shutdown_all(&mut self) {
        for (language, delegate) in &self.delegates {
            if let Err(e) = delegate.lock().await.shutdown().await {
                tracing::warn!("failed to shutdown delegate for {language}: {e}");
            }
        }
    }
}

impl std::fmt::Debug for DelegateRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegateRegistry")
            .field("languages", &self.delegates.keys().collect::<Vec<_>>())
            .finish()
    }
}
