use std::collections::HashMap;

use anyhow::Result;
use loom_config::LanguageConfig;
use loom_delegate::DelegateServer;
use tower_lsp::lsp_types::Url;

/// Manages the lifecycle of delegate LSP servers, one per language. Delegates are spawned lazily
/// on first use so that we don't start servers for languages that never appear in the workspace.
pub struct DelegateRegistry {
    delegates: HashMap<String, DelegateServer>,
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

    /// Returns a mutable reference to the delegate for `language`, spawning and initializing it
    /// first if it has not been started yet.
    pub async fn get_or_spawn(&mut self, language: &str) -> Result<&mut DelegateServer> {
        if self.failed.contains(language) {
            return Err(anyhow::anyhow!("delegate for {language} previously failed to start"));
        }

        if !self.delegates.contains_key(language) {
            let config = self
                .configs
                .get(language)
                .ok_or_else(|| anyhow::anyhow!("no config for language: {language}"))?;

            let mut delegate = DelegateServer::spawn(&config.server_command)?;
            if let Err(e) = delegate.initialize(self.root_uri.clone()).await {
                self.failed.insert(language.to_string());
                return Err(e);
            }
            tracing::info!("delegate ready for {language}");
            self.delegates.insert(language.to_string(), delegate);
        }

        Ok(self.delegates.get_mut(language).unwrap())
    }

    pub async fn shutdown_all(&mut self) {
        for (language, delegate) in &mut self.delegates {
            if let Err(e) = delegate.shutdown().await {
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
