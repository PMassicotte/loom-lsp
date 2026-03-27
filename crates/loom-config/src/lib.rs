use std::collections::HashMap;

use serde::Deserialize;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config() {
        let toml_str = r#"
[server]
log_level = "info"

[languages.python]
server_command = ["pyright-langserver", "--stdio"]
root_markers = ["pyproject.toml", "setup.py"]
preamble = "import pandas as pd\nimport numpy as np\n"
settings = { python = { analysis = { typeCheckingMode = "basic" } } }

[languages.r]
server_command = ["R", "--slave", "-e", "languageserver::run()"]
root_markers = [".Rproj", "DESCRIPTION"]

[languages.markdown]
server_command = ["marksman", "server"]

[languages.yaml]
server_command = ["yaml-language-server", "--stdio"]
"#;

        let config: Config = toml::from_str(toml_str).unwrap();
        assert_eq!(
            config.languages["python"].server_command,
            vec!["pyright-langserver", "--stdio"]
        );
    }
}
