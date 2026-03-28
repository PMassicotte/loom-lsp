use std::sync::atomic::AtomicI64;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout, Command};

use anyhow::Result;

#[derive(Debug)]
pub struct LspTransport {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,

    #[allow(dead_code)]
    next_id: AtomicI64,
}

impl LspTransport {
    pub fn spawn(command: &[String]) -> Result<Self> {
        // This will contains the command and its arguments, so we need to split it into the
        // command and its arguments. For example ["pyright-langserver", "--stdio"] will be split
        // into "pyright-langserver" and ["--stdio"].
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

        Ok(Self {
            stdin,
            stdout: BufReader::new(stdout),
            next_id: AtomicI64::new(0),
        })
    }

    async fn send_raw(&mut self, json: &str) -> Result<()> {
        let content_length = json.len();
        let message = format!("Content-Length: {content_length}\r\n\r\n{json}");
        self.stdin.write_all(message.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    pub async fn send_message(&mut self, msg: serde_json::Value) -> Result<()> {
        let json = serde_json::to_string(&msg)?;
        self.send_raw(&json).await
    }

    // This method reads the raw LSP message from the stdout of the child process. It first reads
    // the headers to determine the content length, and then reads the content based on that
    // length.
    async fn recv_raw(&mut self) -> Result<String> {
        let mut content_length = None;
        let mut buffer = String::new();

        loop {
            buffer.clear();

            let bytes_read = self.stdout.read_line(&mut buffer).await?;
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
        self.stdout.read_exact(&mut content_buffer).await?;

        Ok(String::from_utf8(content_buffer)?)
    }

    pub async fn recv_message(&mut self) -> Result<serde_json::Value> {
        let raw = self.recv_raw().await?;
        let msg = serde_json::from_str(&raw)?;
        Ok(msg)
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

        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize"
        });

        transport.send_message(msg.clone()).await.unwrap();

        let received = transport.recv_message().await.unwrap();
        assert_eq!(received, msg);
    }

    #[test]
    fn test_spawn_missing_binary() {
        let err = LspTransport::spawn(&["this-binary-does-not-exist".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found in PATH"), "unexpected error: {err}");
    }
}
