use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::process::Command;
use tokio::time::timeout;

#[derive(Clone, Debug)]
pub struct GsrCli {
    binary: PathBuf,
    timeout: Duration,
}

impl GsrCli {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            timeout: Duration::from_secs(20),
        }
    }

    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub async fn status(&self, socket: &Path) -> Result<GsrStatus, ControlError> {
        let output = self.run(socket, &["status"]).await?;
        match output.stdout.trim() {
            "running" => Ok(GsrStatus::Running),
            "not running" => Ok(GsrStatus::NotRunning),
            other => Err(ControlError::UnexpectedOutput(other.to_owned())),
        }
    }

    /// Stops this exact GSR instance. On success, stdout is the saved file path.
    pub async fn stop(&self, socket: &Path) -> Result<PathBuf, ControlError> {
        let output = self.run(socket, &["stop"]).await?;
        let path = output.stdout.trim();
        if path.is_empty() {
            return Err(ControlError::UnexpectedOutput(
                "stop succeeded without a saved path".to_owned(),
            ));
        }
        Ok(PathBuf::from(path))
    }

    pub async fn set_paused(&self, socket: &Path, paused: bool) -> Result<(), ControlError> {
        self.run(
            socket,
            &["set-paused", if paused { "true" } else { "false" }],
        )
        .await?;
        Ok(())
    }

    async fn run(&self, socket: &Path, arguments: &[&str]) -> Result<CommandOutput, ControlError> {
        let mut command = Command::new(&self.binary);
        command.arg("-ipc").arg(socket).args(arguments);
        let output = timeout(self.timeout, command.output())
            .await
            .map_err(|_| ControlError::Timeout(self.timeout))?
            .map_err(ControlError::Spawn)?;
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            return Err(ControlError::Rejected {
                code: output.status.code(),
                stderr: stderr.trim().to_owned(),
            });
        }
        Ok(CommandOutput { stdout })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GsrStatus {
    Running,
    NotRunning,
}

#[derive(Debug)]
struct CommandOutput {
    stdout: String,
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("failed to execute gsr-cli: {0}")]
    Spawn(std::io::Error),
    #[error("gsr-cli timed out after {0:?}")]
    Timeout(Duration),
    #[error("gsr-cli rejected the command (exit {code:?}): {stderr}")]
    Rejected { code: Option<i32>, stderr: String },
    #[error("unexpected gsr-cli output: {0}")]
    UnexpectedOutput(String),
}
