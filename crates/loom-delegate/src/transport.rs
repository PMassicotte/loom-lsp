use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, oneshot};

use anyhow::Result;
use serde_json::Value;

#[derive(Debug)]
pub(crate) struct LspTransport {
    stdin: ChildStdin,
    child: tokio::process::Child,
    next_id: AtomicI64,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    notifications: mpsc::Receiver<Value>,
    _reader: tokio::task::JoinHandle<()>,
}

async fn recv_raw(stdout: &mut BufReader<ChildStdout>) -> Result<String> {
    let mut content_length = None;
    let mut buffer = String::new();

    loop {
        buffer.clear();
        let bytes_read = stdout.read_line(&mut buffer).await?;
        if bytes_read == 0 {
            return Err(anyhow::anyhow!("LSP server closed the connection"));
        }
        let line = buffer.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(rest) = line.strip_prefix("Content-Length: ") {
            content_length = Some(rest.parse::<usize>()?);
        }
    }

    let content_length =
        content_length.ok_or_else(|| anyhow::anyhow!("missing Content-Length header"))?;

    let mut content_buffer = vec![0; content_length];
    stdout.read_exact(&mut content_buffer).await?;

    Ok(String::from_utf8(content_buffer)?)
}

async fn reader_loop(
    mut stdout: BufReader<ChildStdout>,
    pending: Arc<Mutex<HashMap<i64, oneshot::Sender<Value>>>>,
    notifications: mpsc::Sender<Value>,
) {
    loop {
        let raw = match recv_raw(&mut stdout).await {
            Ok(raw) => raw,
            Err(_) => break,
        };
        let msg: Value = match serde_json::from_str(&raw) {
            Ok(msg) => msg,
            Err(_) => continue,
        };
        if let Some(id) = msg.get("id").and_then(|v| v.as_i64()) {
            let sender = pending.lock().unwrap().remove(&id);
            if let Some(sender) = sender {
                let _ = sender.send(msg);
            }
        } else {
            let _ = notifications.send(msg).await;
        }
    }
}

impl LspTransport {
    /// Spawns a new LSP server process using the given command, and sets up the transport for
    /// communication. The command should be a list of strings, where the first string is the
    /// executable and the rest are its arguments. For example, to spawn `pyright-langserver
    /// --stdio`, the command would be `["pyright-langserver".to_string(), "--stdio".to_string()]`.
    pub(crate) fn spawn(command: &[String]) -> Result<Self> {
        // Split the command into the executable and its arguments, and verify that the executable
        // exists in PATH.
        let (cmd, args) = command
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("command must not be empty"))?;

        which::which(cmd).map_err(|_| anyhow::anyhow!("LSP server not found in PATH: {cmd}"))?;

        let mut child = Command::new(cmd)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to open stdin"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("failed to open stdout"))?;

        let pending = Arc::new(Mutex::new(HashMap::new()));
        let (notif_tx, notif_rx) = mpsc::channel(64);

        let reader = tokio::spawn(reader_loop(
            BufReader::new(stdout),
            Arc::clone(&pending),
            notif_tx,
        ));

        Ok(Self {
            stdin,
            child,
            next_id: AtomicI64::new(0),
            pending,
            notifications: notif_rx,
            _reader: reader,
        })
    }

    async fn send_raw(&mut self, json: &str) -> Result<()> {
        let content_length = json.len();
        let message = format!("Content-Length: {content_length}\r\n\r\n{json}");
        self.stdin.write_all(message.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    pub(crate) async fn send_message(&mut self, msg: Value) -> Result<()> {
        let json = serde_json::to_string(&msg)?;
        self.send_raw(&json).await
    }

    pub(crate) async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send_message(request).await?;
        rx.await
            .map_err(|_| anyhow::anyhow!("request cancelled: LSP server closed"))
    }

    pub(crate) async fn next_notification(&mut self) -> Option<Value> {
        self.notifications.recv().await
    }

    pub(crate) fn kill(&mut self) {
        let _ = self.child.start_kill();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_spawn_cat() {
        let transport = LspTransport::spawn(&["cat".to_string()]);
        assert!(
            transport.is_ok(),
            "failed to spawn cat: {:?}",
            transport.err()
        );
    }

    #[tokio::test]
    async fn test_framing_round_trip() {
        let mut transport = LspTransport::spawn(&["cat".to_string()]).unwrap();

        // cat echoes back whatever we send, so send_request will receive the echoed request
        // as its response (sufficient to verify framing correctness).
        let response = transport
            .send_request("initialize", serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["method"], "initialize");
        assert_eq!(response["id"], 0);
    }

    #[tokio::test]
    #[ignore = "requires pyright-langserver in PATH"]
    async fn test_pyright_initialize() {
        let mut transport =
            LspTransport::spawn(&["pyright-langserver".to_string(), "--stdio".to_string()])
                .unwrap();

        let response = transport
            .send_request(
                "initialize",
                serde_json::json!({
                    "processId": null,
                    "rootUri": null,
                    "capabilities": {},
                }),
            )
            .await
            .unwrap();

        assert!(
            response
                .get("result")
                .and_then(|r| r.get("capabilities"))
                .is_some(),
            "expected capabilities in initialize response, got: {response}"
        );

        transport
            .send_request("shutdown", serde_json::Value::Null)
            .await
            .unwrap();

        transport
            .send_message(serde_json::json!({
                "jsonrpc": "2.0",
                "method": "exit",
            }))
            .await
            .unwrap();
    }

    #[test]
    fn test_spawn_missing_binary() {
        let err = LspTransport::spawn(&["this-binary-does-not-exist".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found in PATH"), "unexpected error: {err}");
    }
}

