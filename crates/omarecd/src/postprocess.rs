//! `omarchy_compat` postprocess: staging A stays until staging B validates.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

/// Copy/transform staging A into staging B with a bounded `FFmpeg` argv.
/// The source file is left in place until the caller validates and promotes B.
pub fn omarchy_compat(
    ffmpeg: &Path,
    source: &Path,
    destination: &Path,
    timeout: Duration,
) -> Result<(), PostprocessError> {
    if source
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('-'))
        || destination
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('-'))
    {
        return Err(PostprocessError::DashPrefixedName);
    }
    let mut child = Command::new(ffmpeg)
        .args(["-hide_banner", "-nostdin", "-loglevel", "error", "-i"])
        .arg(source)
        .args(["-c", "copy"])
        .arg(destination)
        .spawn()
        .map_err(PostprocessError::Spawn)?;
    let status = wait_bounded(&mut child, timeout)?;
    if !status.success() {
        let _ = child.kill();
        return Err(PostprocessError::Rejected(status.code()));
    }
    if !source.exists() {
        return Err(PostprocessError::SourceRemoved);
    }
    Ok(())
}

fn wait_bounded(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::ExitStatus, PostprocessError> {
    let started = std::time::Instant::now();
    loop {
        match child.try_wait().map_err(PostprocessError::Spawn)? {
            Some(status) => return Ok(status),
            None if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PostprocessError::Timeout);
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PostprocessError {
    #[error("failed to spawn ffmpeg: {0}")]
    Spawn(std::io::Error),
    #[error("ffmpeg timed out")]
    Timeout,
    #[error("ffmpeg failed (exit {0:?})")]
    Rejected(Option<i32>),
    #[error("postprocess removed the source staging file")]
    SourceRemoved,
    #[error("paths beginning with '-' are rejected")]
    DashPrefixedName,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn dash_prefixed_paths_are_rejected_before_spawn() {
        let error = omarchy_compat(
            Path::new("ffmpeg"),
            Path::new("/tmp/-a.mp4"),
            Path::new("/tmp/b.mp4"),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(error, PostprocessError::DashPrefixedName));
        let error = omarchy_compat(
            Path::new("ffmpeg"),
            Path::new("/tmp/a.mp4"),
            Path::new("/tmp/-b.mp4"),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(matches!(error, PostprocessError::DashPrefixedName));
    }
}
