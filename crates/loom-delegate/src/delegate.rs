/// The job of `DelegateServer` is to manage the lifecycle of a single LSP server process, and to
/// provide a simple API for sending LSP messages to it. It is not responsible for managing
/// multiple servers or for routing messages to the correct server based on the file being edited.
/// It is using `LspTransport` to handle the actual communication with the server process, and it
/// is responsible for maintaining the state of the server (e.g. whether it is starting, ready, or
/// shutting down) and for storing the server's capabilities once it has been initialized.
use anyhow::Result;

use crate::transport::LspTransport;

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
        &mut self,
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
        &mut self,
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

    pub async fn close_document(&mut self, uri: lsp_types::Url) -> Result<()> {
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
}
