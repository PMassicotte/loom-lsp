use serde::Deserialize;
use std::collections::HashMap;

/// Configuration for the loom language server.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    pub server: Option<ServerConfig>,
    pub languages: HashMap<String, LanguageConfig>,
}

/// Configuration for the language server itself.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ServerConfig {
    pub log_level: String,
}

/// Configuration for a specific programming language.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct LanguageConfig {
    pub server_command: Vec<String>,
    pub root_markers: Option<Vec<String>>,
    pub preamble: Option<String>,
    pub settings: Option<toml::Value>,
}

impl LanguageConfig {
    pub fn find_root(&self, start: &std::path::Path) -> Option<std::path::PathBuf> {
        let markers = self.root_markers.as_ref()?;
        let mut current = start;
        loop {
            for marker in markers {
                if current.join(marker).exists() {
                    return Some(current.to_path_buf());
                }
            }
            current = current.parent()?;
        }
    }
}

/// Errors that can occur while loading the configuration.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    Io(#[from] std::io::Error),

    #[error("Failed to parse config file: {0}")]
    Toml(#[from] toml::de::Error),
}

/// Parse the configuration from a string.
fn parse_config(content: &str) -> Result<Config, ConfigError> {
    Ok(toml::from_str(content)?)
}

pub fn load_config_from(path: &std::path::Path) -> Result<Config, ConfigError> {
    let content = std::fs::read_to_string(path)?;
    parse_config(&content)
}

/// Load the configuration from the default locations.
pub fn load_config() -> Result<Config, ConfigError> {
    // Define the paths to look for config files, in order of precedence. Start with the user
    // config directory, then the current/project directory.
    let mut config_paths = Vec::new();

    if let Some(config_dir) = dirs::config_dir() {
        config_paths.push(config_dir.join("loom").join("loom.toml"));
    }

    config_paths.push(std::env::current_dir()?.join(".loom.toml"));

    // Load and merge configs from all found paths, with later paths taking precedence.
    let mut result = Config::default();

    for path in &config_paths {
        if path.exists() {
            let config_str = std::fs::read_to_string(path)?;
            let config = parse_config(&config_str)?;

            result = merge_configs(result, config);

            tracing::info!("Loaded config from {}", path.display());
        }
    }

    if result.languages.is_empty() && result.server.is_none() {
        tracing::info!("No config file found, using default config");
    }

    Ok(result)
}

pub fn merge_configs(base: Config, overlay: Config) -> Config {
    let mut languages = base.languages;
    languages.extend(overlay.languages);
    Config {
        server: overlay.server.or(base.server),
        languages,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! fixture {
        ($name:expr) => {
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../tests/fixtures/",
                $name
            ))
        };
    }

    #[test]
    fn test_parse_config() {
        let config = parse_config(fixture!("full_config.toml")).unwrap();
        assert_eq!(
            config.languages["python"].server_command,
            vec!["pyright-langserver", "--stdio"]
        );
    }

    #[test]
    fn test_merge_configs() {
        let base = parse_config(fixture!("full_config.toml")).unwrap();

        let overlay = parse_config(
            r#"
[languages.python]
server_command = ["pylsp"]
"#,
        )
        .unwrap();

        let merged = merge_configs(base, overlay);

        // overlay's python wins
        assert_eq!(merged.languages["python"].server_command, vec!["pylsp"]);
        // base's r is preserved
        assert_eq!(
            merged.languages["r"].server_command,
            vec!["R", "--slave", "-e", "languageserver::run()"]
        );
        // base's server is preserved when overlay has none
        assert_eq!(merged.server.unwrap().log_level, "info");
    }

    #[test]
    // This test ensures that when both global and project configs specify a setting, the project
    // config takes precedence.
    fn test_project_config_takes_precedence() {
        let global = parse_config(
            r#"
  [languages.python]                                                                
  server_command = ["pyright-langserver", "--stdio"]
  "#,
        )
        .unwrap();

        let project = parse_config(
            r#"
  [languages.python]
  server_command = ["pylsp"]                                                        
  "#,
        )
        .unwrap();

        let merged = merge_configs(global, project);

        assert_eq!(merged.languages["python"].server_command, vec!["pylsp"]);
    }

    #[test]
    fn test_find_root_for_python() {
        let config = LanguageConfig {
            server_command: vec!["dummy".to_string()],
            root_markers: Some(vec![".git".to_string(), "pyproject.toml".to_string()]),
            preamble: None,
            settings: None,
        };

        let temp_dir = tempfile::tempdir().unwrap();
        let project_dir = temp_dir.path().join("my_project");
        std::fs::create_dir(&project_dir).unwrap();
        std::fs::write(project_dir.join(".git"), "").unwrap();

        let found_root = config.find_root(&project_dir.join("src").join("main.py"));
        assert_eq!(found_root.unwrap(), project_dir);
    }

    #[test]
    fn test_find_root_no_markers() {
        let config = LanguageConfig {
            server_command: vec!["dummy".to_string()],
            root_markers: Some(vec![".git".to_string(), "pyproject.toml".to_string()]),
            preamble: None,
            settings: None,
        };

        let temp_dir = tempfile::tempdir().unwrap();
        let found_root = config.find_root(&temp_dir.path().join("src").join("main.py"));
        assert!(found_root.is_none());
    }
}
