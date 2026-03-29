/// The job of `DelegateServer` is to manage the lifecycle of a single LSP server process, and to
/// provide a simple API for sending LSP messages to it. It is not responsible for managing
/// multiple servers or for routing messages to the correct server based on the file being edited.
/// It is using `LspTransport` to handle the actual communication with the server process, and it
/// is responsible for maintaining the state of the server (e.g. whether it is starting, ready, or
/// shutting down) and for storing the server's capabilities once it has been initialized.
use anyhow::Result;

use crate::transport::LspTransport;
pub use crate::transport::TransportSender;

#[derive(Debug)]
pub struct DelegateServer {
    transport: LspTransport,
    state: DelegateState,
    server_capabilities: Option<lsp_types::ServerCapabilities>,
}

#[derive(Debug, PartialEq)]
pub(crate) enum DelegateState {
    Starting,
    Ready,
    ShuttingDown,
}

impl DelegateServer {
    /// Returns a cheaply cloneable sender handle. Callers can clone this and then release the
    /// `DelegateServer` lock before awaiting long LSP responses (e.g. completions).
    pub fn sender(&self) -> TransportSender {
        self.transport.sender()
    }

    /// Returns false if the LSP server process has exited (broken pipe / EOF on stdout).
    pub fn is_alive(&self) -> bool {
        self.transport.is_alive()
    }

    pub fn spawn(command: &[String]) -> Result<Self> {
        let transport = LspTransport::spawn(command)?;

        Ok(Self {
            transport,
            state: DelegateState::Starting,
            server_capabilities: None,
        })
    }

    pub async fn initialize(&mut self, root_uri: Option<lsp_types::Url>) -> Result<()> {
        let params = lsp_types::InitializeParams {
            process_id: None,
            root_uri,
            capabilities: lsp_types::ClientCapabilities::default(),
            ..Default::default()
        };

        let response = self
            .transport
            .send_request("initialize", serde_json::to_value(params)?)
            .await?;

        let result: lsp_types::InitializeResult =
            serde_json::from_value(response["result"].clone())
                .map_err(|e| anyhow::anyhow!("failed to parse initialize result: {e}"))?;

        self.server_capabilities = Some(result.capabilities);

        self.transport
            .send_message(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "initialized",
                "params": {},
            }))
            .await?;

        self.state = DelegateState::Ready;
        Ok(())
    }

    pub async fn open_document(
        &self,
        uri: lsp_types::Url,
        language_id: &str,
        content: &str,
    ) -> Result<()> {
        let params = lsp_types::DidOpenTextDocumentParams {
            text_document: lsp_types::TextDocumentItem {
                uri,
                language_id: language_id.to_string(),
                version: 0,
                text: content.to_string(),
            },
        };

        self.transport
            .send_message(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didOpen",
                "params": serde_json::to_value(params)?,
            }))
            .await
    }

    pub async fn update_document(
        &self,
        uri: lsp_types::Url,
        version: i32,
        content: &str,
    ) -> Result<()> {
        let params = lsp_types::DidChangeTextDocumentParams {
            text_document: lsp_types::VersionedTextDocumentIdentifier { uri, version },
            content_changes: vec![lsp_types::TextDocumentContentChangeEvent {
                range: None,
                range_length: None,
                text: content.to_string(),
            }],
        };
        self.transport
            .send_message(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didChange",
                "params": serde_json::to_value(params)?,
            }))
            .await
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        self.state = DelegateState::ShuttingDown;

        self.transport
            .send_request("shutdown", serde_json::Value::Null)
            .await?;

        self.transport
            .send_message(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "exit",
            }))
            .await?;

        Ok(())
    }

    pub async fn completion(
        &self,
        uri: lsp_types::Url,
        line: u32,
        character: u32,
    ) -> Result<serde_json::Value> {
        let params = lsp_types::CompletionParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                position: lsp_types::Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: None,
        };

        let response = self
            .transport
            .send_request("textDocument/completion", serde_json::to_value(params)?)
            .await?;

        Ok(response["result"].clone())
    }

    pub async fn close_document(&self, uri: lsp_types::Url) -> Result<()> {
        let params = lsp_types::DidCloseTextDocumentParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
        };
        self.transport
            .send_message(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "textDocument/didClose",
                "params": serde_json::to_value(params)?,
            }))
            .await
    }
}

impl Drop for DelegateServer {
    fn drop(&mut self) {
        if self.state != DelegateState::ShuttingDown {
            self.transport.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires pyright-langserver in PATH"]
    async fn test_delegate_initialize() {
        let mut server =
            DelegateServer::spawn(&["pyright-langserver".to_string(), "--stdio".to_string()])
                .unwrap();

        server.initialize(None).await.unwrap();

        assert_eq!(server.state, DelegateState::Ready);
        assert!(server.server_capabilities.is_some());
    }

    #[tokio::test]
    #[ignore = "requires pyright-langserver in PATH"]
    async fn test_completions_round_trip() {
        let mut server =
            DelegateServer::spawn(&["pyright-langserver".to_string(), "--stdio".to_string()])
                .unwrap();

        server.initialize(None).await.unwrap();

        let uri = lsp_types::Url::parse("file:///tmp/virtual.py").unwrap();

        // Pad with blank lines so the code lands on line 2, matching the completion position.
        let content = "\n\nimport os\nos.path.";
        server
            .open_document(uri.clone(), "python", content)
            .await
            .unwrap();

        let result = server.completion(uri, 3, 8).await.unwrap();

        let labels: Vec<&str> = result["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|item| item["label"].as_str())
            .collect();

        assert!(
            labels.iter().any(|l| *l == "join"),
            "expected 'join' in completions, got: {labels:?}"
        );
        assert!(
            labels.iter().any(|l| *l == "exists"),
            "expected 'exists' in completions, got: {labels:?}"
        );
    }
}
