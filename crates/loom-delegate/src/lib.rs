use std::sync::atomic::AtomicI64;

use tokio::io::BufReader;
use tokio::process::{ChildStdin, ChildStdout, Command};

use anyhow::Result;

#[derive(Debug)]
pub struct LspTransport {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
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

    #[test]
    fn test_spawn_missing_binary() {
        let err = LspTransport::spawn(&["this-binary-does-not-exist".to_string()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found in PATH"), "unexpected error: {err}");
    }
}
