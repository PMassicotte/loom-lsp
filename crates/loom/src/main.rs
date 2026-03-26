use tower_lsp::lsp_types::{InitializeParams, InitializeResult, ServerCapabilities, ServerInfo};
use tower_lsp::{LanguageServer, Server};

#[derive(Debug)]
struct LoomServer;

#[tower_lsp::async_trait]
impl LanguageServer for LoomServer {
    async fn initialize(
        &self,
        _params: InitializeParams,
    ) -> tower_lsp::jsonrpc::Result<InitializeResult> {
        tracing::info!("Initialize request received");

        Ok(InitializeResult {
            server_info: Some(ServerInfo {
                name: "loom".to_string(),
                version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            capabilities: ServerCapabilities::default(),
        })
    }

    async fn shutdown(&self) -> tower_lsp::jsonrpc::Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_file = std::fs::File::create("/tmp/loom.log")?;

    tracing_subscriber::fmt()
        .with_writer(log_file)
        .with_env_filter("loom=debug")
        .init();

    tracing::info!("Starting loom language server");

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();
    let (service, socket) = tower_lsp::LspService::new(|_client| LoomServer);
    Server::new(stdin, stdout, socket).serve(service).await;

    Ok(())
}
