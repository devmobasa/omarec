use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use omarec_core::Capabilities;
use tokio::process::Command;
use tokio::time::timeout;

use crate::parse::{
    MAX_PROBE_BYTES, ParseError, ProbeContext, ProbeDocuments, assemble_capabilities,
    decode_bounded,
};

#[derive(Clone, Debug)]
pub struct ProbeRunner {
    recorder_binary: PathBuf,
    cli_binary: PathBuf,
    timeout: Duration,
    max_bytes: usize,
}

impl ProbeRunner {
    pub fn new(recorder_binary: impl Into<PathBuf>) -> Self {
        Self {
            recorder_binary: recorder_binary.into(),
            cli_binary: PathBuf::from("gsr-cli"),
            timeout: Duration::from_secs(5),
            max_bytes: MAX_PROBE_BYTES,
        }
    }

    #[must_use]
    pub fn with_cli_binary(mut self, cli_binary: impl Into<PathBuf>) -> Self {
        self.cli_binary = cli_binary.into();
        self
    }

    pub async fn probe(&self) -> Result<Capabilities, ProbeError> {
        let info = self
            .run(&["--info"], true)
            .await?
            .ok_or(ProbeError::RequiredMissing("--info"))?;
        let capture = self
            .run(&["--list-capture-options"], true)
            .await?
            .ok_or(ProbeError::RequiredMissing("--list-capture-options"))?;
        let version = self.run(&["--version"], false).await?;
        let help = match self.run(&["--help"], false).await? {
            Some(document) => Some(document),
            None => self.run(&["-h"], false).await?,
        };
        let cameras = self.run(&["--list-v4l2-devices"], false).await?;
        let audio = self.run(&["--list-audio-devices"], false).await?;
        let applications = self.run(&["--list-application-audio"], false).await?;
        let monitors = self.run(&["--list-monitors"], false).await?;
        let gsr_cli_available = command_exists(&self.cli_binary).await;
        let probed_unix_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|duration| u64::try_from(duration.as_millis()).ok());

        assemble_capabilities(
            &ProbeDocuments {
                version: version.as_deref(),
                info: &info,
                capture_options: &capture,
                monitors: monitors.as_deref(),
                cameras: cameras.as_deref(),
                audio: audio.as_deref(),
                applications: applications.as_deref(),
                help: help.as_deref(),
            },
            &ProbeContext {
                gsr_cli_available,
                generation: 1,
                probed_unix_ms,
            },
        )
        .map_err(ProbeError::Parse)
    }

    async fn run(&self, arguments: &[&str], required: bool) -> Result<Option<String>, ProbeError> {
        let output = match timeout(
            self.timeout,
            Command::new(&self.recorder_binary).args(arguments).output(),
        )
        .await
        {
            Ok(Ok(output)) => output,
            Ok(Err(error)) if required => return Err(ProbeError::Spawn(error)),
            Err(_) if required => return Err(ProbeError::Timeout(self.timeout)),
            Ok(Err(_)) | Err(_) => return Ok(None),
        };
        if output.stdout.len() > self.max_bytes || output.stderr.len() > self.max_bytes {
            return Err(ProbeError::Parse(ParseError::Oversized {
                limit: self.max_bytes,
            }));
        }
        let document = coalesce_output(&output.stdout, &output.stderr, self.max_bytes);
        if required && !output.status.success() {
            return Err(ProbeError::Rejected {
                arguments: arguments.iter().map(ToString::to_string).collect(),
                code: output.status.code(),
                stderr: decode_bounded(&output.stderr, self.max_bytes)
                    .unwrap_or_default()
                    .trim()
                    .to_owned(),
            });
        }
        Ok(document)
    }
}

fn coalesce_output(stdout: &[u8], stderr: &[u8], limit: usize) -> Option<String> {
    for bytes in [stdout, stderr] {
        if let Ok(document) = decode_bounded(bytes, limit)
            && !document.trim().is_empty()
        {
            return Some(document);
        }
    }
    None
}

async fn command_exists(path: &std::path::Path) -> bool {
    match Command::new(path).arg("--help").output().await {
        Ok(output) => {
            output.status.success() || !output.stdout.is_empty() || !output.stderr.is_empty()
        }
        Err(_) => false,
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("failed to execute GPU Screen Recorder probe: {0}")]
    Spawn(std::io::Error),
    #[error("GPU Screen Recorder probe timed out after {0:?}")]
    Timeout(Duration),
    #[error("GPU Screen Recorder {arguments:?} failed (exit {code:?}): {stderr}")]
    Rejected {
        arguments: Vec<String>,
        code: Option<i32>,
        stderr: String,
    },
    #[error("required GPU Screen Recorder probe {0} returned no document")]
    RequiredMissing(&'static str),
    #[error(transparent)]
    Parse(#[from] ParseError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn write_executable(path: &std::path::Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn scratch() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("omarec-probe-{unique}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn help_on_stdout_is_kept_when_the_process_exits_nonzero() {
        let document = coalesce_output(
            b"usage: gpu-screen-recorder [-ipc <socket_path>]\n",
            b"",
            MAX_PROBE_BYTES,
        )
        .unwrap();
        assert!(document.contains("-ipc"));
    }

    #[tokio::test]
    async fn probe_treats_help_exit_one_as_ipc_available() {
        let dir = scratch();
        let recorder = dir.join("gpu-screen-recorder");
        let cli = dir.join("gsr-cli");
        write_executable(
            &recorder,
            r#"#!/bin/sh
case "$1" in
  --info)
    printf '%s\n' \
      'section=system_info' \
      'gsr_version|6.0.0' \
      'section=gpu_info' \
      'vendor|amd' \
      'section=video_codecs' \
      'h264' \
      'section=capture_options' \
      'DP-1|1920x1080'
    ;;
  --list-capture-options)
    printf 'DP-1|1920x1080\n'
    ;;
  --version)
    printf '6.0.0\n'
    ;;
  --help|-h)
    printf 'usage: gpu-screen-recorder [-ipc <socket_path>] [-write-first-frame-ts yes|no] combine sources with |\n'
    exit 1
    ;;
  *)
    exit 0
    ;;
esac
"#,
        );
        write_executable(&cli, "#!/bin/sh\nprintf 'ok\n'\n");
        let capabilities = ProbeRunner::new(&recorder)
            .with_cli_binary(&cli)
            .probe()
            .await
            .unwrap();
        assert!(capabilities.ipc_available);
        assert!(capabilities.native_multi_source_available);
    }
}
