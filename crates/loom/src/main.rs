use tower_lsp::LanguageServer;
use tower_lsp::lsp_types::{InitializeParams, InitializeResult, ServerCapabilities, ServerInfo};

#[derive(Debug)]
struct LoomServer;

#[tower_lsp::async_trait]
impl LanguageServer for LoomServer {
    async fn initialize(
        &self,
        _params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "loom".to_string(),
                version: Some("0.1.0".to_string()),
            }),
            capabilities: ServerCapabilities::default(),
        })
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }
}

fn main() {
    println!("Hello from my Nix-powered CLI!");
}
