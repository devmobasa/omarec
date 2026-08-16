//! Symlink-safe staging and atomic no-replace promotion.

use std::ffi::OsStr;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rustix::fs::{Mode, OFlags, RenameFlags, fstat, fsync, open, openat, renameat_with};

const FFPROBE_TIMEOUT: Duration = Duration::from_secs(8);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MediaInfo {
    pub has_video: bool,
    pub duration_seconds: Option<u64>,
    pub format_name: String,
}

pub trait MediaProbe {
    fn probe(&self, path: &Path) -> Result<MediaInfo, OutputError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AcceptingProbe;

impl MediaProbe for AcceptingProbe {
    fn probe(&self, _path: &Path) -> Result<MediaInfo, OutputError> {
        Ok(MediaInfo {
            has_video: true,
            duration_seconds: Some(1),
            format_name: "mov,mp4,m4a,3gp,3g2,mj2".to_owned(),
        })
    }
}

#[derive(Clone, Debug)]
pub struct Ffprobe {
    binary: PathBuf,
}

impl Ffprobe {
    pub fn new(binary: impl AsRef<Path>) -> Self {
        Self {
            binary: binary.as_ref().to_path_buf(),
        }
    }

    pub fn arguments(path: &Path) -> Vec<String> {
        vec![
            "-hide_banner".to_owned(),
            "-loglevel".to_owned(),
            "error".to_owned(),
            "-print_format".to_owned(),
            "json".to_owned(),
            "-show_format".to_owned(),
            "-show_streams".to_owned(),
            "-analyzeduration".to_owned(),
            "2000000".to_owned(),
            "-probesize".to_owned(),
            "2000000".to_owned(),
            "--".to_owned(),
            path.as_os_str().to_string_lossy().into_owned(),
        ]
    }
}

impl MediaProbe for Ffprobe {
    fn probe(&self, path: &Path) -> Result<MediaInfo, OutputError> {
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('-'))
        {
            return Err(OutputError::DashPrefixedName);
        }
        let mut command = Command::new(&self.binary);
        command
            .args(Self::arguments(path))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(OutputError::ProbeSpawn)?;
        let output = wait_bounded(&mut child, FFPROBE_TIMEOUT)?;
        if !output.status.success() {
            return Err(OutputError::ProbeFailed(output.status.code()));
        }
        parse_ffprobe_json(
            std::str::from_utf8(&output.stdout).map_err(|_| OutputError::InvalidMedia)?,
        )
    }
}

pub fn parse_ffprobe_json(source: &str) -> Result<MediaInfo, OutputError> {
    let value: serde_json::Value =
        serde_json::from_str(source).map_err(OutputError::InvalidProbe)?;
    let streams = value
        .get("streams")
        .and_then(serde_json::Value::as_array)
        .ok_or(OutputError::InvalidMedia)?;
    let has_video = streams.iter().any(|stream| {
        stream.get("codec_type").and_then(serde_json::Value::as_str) == Some("video")
    });
    if !has_video {
        return Err(OutputError::InvalidMedia);
    }
    let format = value.get("format").ok_or(OutputError::InvalidMedia)?;
    let format_name = format
        .get("format_name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or(OutputError::InvalidMedia)?
        .to_owned();
    let duration_seconds = format
        .get("duration")
        .and_then(serde_json::Value::as_str)
        .map(parse_duration_seconds)
        .transpose()?;
    Ok(MediaInfo {
        has_video: true,
        duration_seconds,
        format_name,
    })
}

fn parse_duration_seconds(source: &str) -> Result<u64, OutputError> {
    let seconds: f64 = source.parse().map_err(|_| OutputError::InvalidMedia)?;
    if !seconds.is_finite() || !(0.0..=86_400.0).contains(&seconds) {
        return Err(OutputError::InvalidMedia);
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    Ok(seconds.floor() as u64)
}

fn wait_bounded(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<std::process::Output, OutputError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut stdout = Vec::new();
                let mut stderr = Vec::new();
                if let Some(mut pipe) = child.stdout.take() {
                    let _ = pipe.read_to_end(&mut stdout);
                }
                if let Some(mut pipe) = child.stderr.take() {
                    let _ = pipe.read_to_end(&mut stderr);
                }
                return Ok(std::process::Output {
                    status,
                    stdout,
                    stderr,
                });
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(OutputError::ProbeTimeout);
            }
            Err(error) => return Err(OutputError::ProbeSpawn(error)),
        }
    }
}

pub fn open_dir_nofollow(path: &Path) -> Result<OwnedFd, OutputError> {
    open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| OutputError::Open {
        path: path.to_path_buf(),
        source: error,
    })
}

pub fn identity(fd: impl AsFd) -> Result<FileIdentity, OutputError> {
    let stat = fstat(fd).map_err(OutputError::Stat)?;
    Ok(FileIdentity {
        device: stat.st_dev,
        inode: stat.st_ino,
    })
}

pub fn create_staging_exclusive(dir: impl AsFd, name: &OsStr) -> Result<OwnedFd, OutputError> {
    openat(
        dir,
        name,
        OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC,
        Mode::from(0o600),
    )
    .map_err(|error| OutputError::CreateStaging { source: error })
}

pub fn open_nofollow(dir: impl AsFd, name: &OsStr) -> Result<OwnedFd, OutputError> {
    openat(
        dir,
        name,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|error| OutputError::Open {
        path: PathBuf::from(name),
        source: error,
    })
}

/// Promote `staging` to `final_path` with Linux `renameat2(RENAME_NOREPLACE)`.
///
/// Identities are re-checked immediately before the rename so a replacement
/// between validation and mutation cannot be promoted by pathname alone.
pub fn promote(
    dest_dir: &Path,
    expected_staging: &Path,
    saved_path: &Path,
    final_path: &Path,
    probe: &impl MediaProbe,
) -> Result<(), OutputError> {
    if expected_staging
        .file_name()
        .is_some_and(|name| name.to_string_lossy().starts_with('-'))
        || final_path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with('-'))
    {
        return Err(OutputError::DashPrefixedName);
    }
    let dir = open_dir_nofollow(dest_dir)?;
    let staging_name = basename(expected_staging)?;
    let saved_name = basename(saved_path)?;
    let final_name = basename(final_path)?;
    if parent_or_dot(expected_staging) != dest_dir || parent_or_dot(final_path) != dest_dir {
        return Err(OutputError::WrongDirectory);
    }
    if parent_or_dot(saved_path) != dest_dir || saved_name != staging_name {
        return Err(OutputError::ForgedSavedPath);
    }

    // Keep the validated inode open across probing. If the path is replaced,
    // the filesystem cannot recycle this inode before the identity re-check.
    let validated_staging = open_nofollow(&dir, staging_name)?;
    let expected = identity(&validated_staging)?;
    let saved = open_nofollow(&dir, saved_name)?;
    if identity(&saved)? != expected {
        return Err(OutputError::IdentityMismatch);
    }
    drop(saved);

    let info = probe.probe(expected_staging)?;
    if !info.has_video {
        return Err(OutputError::InvalidMedia);
    }

    let current_staging = open_nofollow(&dir, staging_name)?;
    if identity(&current_staging)? != expected {
        return Err(OutputError::IdentityMismatch);
    }
    drop(current_staging);
    drop(validated_staging);

    renameat_with(&dir, staging_name, &dir, final_name, RenameFlags::NOREPLACE).map_err(
        |error| {
            if error == rustix::io::Errno::EXIST {
                OutputError::AlreadyExists
            } else {
                OutputError::Promote { source: error }
            }
        },
    )?;

    let completed = open_nofollow(&dir, final_name)?;
    fsync(&completed).map_err(OutputError::Fsync)?;
    fsync(&dir).map_err(OutputError::Fsync)?;
    Ok(())
}

fn basename(path: &Path) -> Result<&OsStr, OutputError> {
    path.file_name().ok_or(OutputError::MissingFileName)
}

fn parent_or_dot(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error("failed to open {path}: {source}")]
    Open {
        path: PathBuf,
        #[source]
        source: rustix::io::Errno,
    },
    #[error("failed to inspect file identity: {0}")]
    Stat(rustix::io::Errno),
    #[error("failed to create exclusive staging file: {source}")]
    CreateStaging { source: rustix::io::Errno },
    #[error("saved path is not the session-owned staging file")]
    ForgedSavedPath,
    #[error("file identity changed between validation and promotion")]
    IdentityMismatch,
    #[error("destination already exists; v1 does not overwrite")]
    AlreadyExists,
    #[error("promotion failed: {source}")]
    Promote { source: rustix::io::Errno },
    #[error("failed to fsync: {0}")]
    Fsync(rustix::io::Errno),
    #[error("validated media is missing a video stream")]
    InvalidMedia,
    #[error("output names must live in the destination directory")]
    WrongDirectory,
    #[error("output path is missing a file name")]
    MissingFileName,
    #[error("paths beginning with '-' are rejected")]
    DashPrefixedName,
    #[error("ffprobe output is not valid JSON: {0}")]
    InvalidProbe(#[from] serde_json::Error),
    #[error("ffprobe timed out")]
    ProbeTimeout,
    #[error("failed to spawn ffprobe: {0}")]
    ProbeSpawn(std::io::Error),
    #[error("ffprobe failed (exit {0:?})")]
    ProbeFailed(Option<i32>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct ReplacingProbe {
        path: PathBuf,
    }

    impl MediaProbe for ReplacingProbe {
        fn probe(&self, _path: &Path) -> Result<MediaInfo, OutputError> {
            fs::remove_file(&self.path).unwrap();
            fs::write(&self.path, b"during-probe").unwrap();
            Ok(MediaInfo {
                has_video: true,
                duration_seconds: Some(1),
                format_name: "mp4".to_owned(),
            })
        }
    }

    fn unique_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("omarec-out-{nanos}"));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn exclusive_staging_then_atomic_promote() {
        let dir = unique_dir();
        let staging = dir.join(".omarec-part.mp4");
        let final_path = dir.join("out.mp4");
        let dirfd = open_dir_nofollow(&dir).unwrap();
        let staging_fd = create_staging_exclusive(&dirfd, staging.file_name().unwrap()).unwrap();
        rustix::io::write(&staging_fd, b"media").unwrap();
        drop(staging_fd);
        promote(&dir, &staging, &staging, &final_path, &AcceptingProbe).unwrap();
        assert!(final_path.is_file());
        assert!(!staging.exists());
    }

    #[test]
    fn existing_final_name_is_not_overwritten() {
        let dir = unique_dir();
        let staging = dir.join(".omarec-part.mp4");
        let final_path = dir.join("out.mp4");
        fs::write(&staging, b"new").unwrap();
        fs::write(&final_path, b"old").unwrap();
        let error = promote(&dir, &staging, &staging, &final_path, &AcceptingProbe).unwrap_err();
        assert!(matches!(error, OutputError::AlreadyExists));
        assert_eq!(fs::read(&final_path).unwrap(), b"old");
        assert_eq!(fs::read(&staging).unwrap(), b"new");
    }

    #[test]
    fn symlink_destination_directory_is_rejected() {
        let real = unique_dir();
        let parent = real.parent().unwrap().join(format!(
            "omarec-link-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        symlink(&real, &parent).unwrap();
        let error = open_dir_nofollow(&parent).unwrap_err();
        assert!(matches!(error, OutputError::Open { .. }));
    }

    #[test]
    fn forged_saved_path_is_rejected() {
        let dir = unique_dir();
        let staging = dir.join(".omarec-part.mp4");
        let other = dir.join("other.mp4");
        let final_path = dir.join("out.mp4");
        fs::write(&staging, b"owned").unwrap();
        fs::write(&other, b"forged").unwrap();
        let error = promote(&dir, &staging, &other, &final_path, &AcceptingProbe).unwrap_err();
        assert!(matches!(error, OutputError::ForgedSavedPath));
        assert!(staging.exists());
        assert!(!final_path.exists());
    }

    #[test]
    fn replacement_between_validation_and_promote_is_detected() {
        let dir = unique_dir();
        let staging = dir.join(".omarec-part.mp4");
        let final_path = dir.join("out.mp4");
        fs::write(&staging, b"first").unwrap();
        let error = promote(
            &dir,
            &staging,
            &staging,
            &final_path,
            &ReplacingProbe {
                path: staging.clone(),
            },
        )
        .unwrap_err();
        assert!(matches!(error, OutputError::IdentityMismatch));
        assert_eq!(fs::read(&staging).unwrap(), b"during-probe");
        assert!(!final_path.exists());
    }

    #[test]
    fn dash_prefixed_names_are_rejected() {
        let dir = unique_dir();
        let staging = dir.join("-evil.mp4");
        fs::write(&staging, b"x").unwrap();
        let error = promote(
            &dir,
            &staging,
            &staging,
            &dir.join("out.mp4"),
            &AcceptingProbe,
        )
        .unwrap_err();
        assert!(matches!(error, OutputError::DashPrefixedName));
    }

    #[test]
    fn ffprobe_argv_places_end_of_options_before_the_path() {
        let args = Ffprobe::arguments(Path::new("clip.mp4"));
        assert_eq!(args.last().map(String::as_str), Some("clip.mp4"));
        let separator = args.iter().position(|argument| argument == "--").unwrap();
        assert_eq!(separator, args.len() - 2);
        assert!(
            !args[..separator]
                .iter()
                .any(|argument| argument == "clip.mp4")
        );
    }

    #[test]
    fn ffprobe_json_requires_a_video_stream() {
        let info =
            parse_ffprobe_json(include_str!("../../../tests/fixtures/ffprobe/video.json")).unwrap();
        assert!(info.has_video);
        assert_eq!(info.duration_seconds, Some(1));
        assert!(info.format_name.contains("mp4"));
        let error = parse_ffprobe_json(include_str!(
            "../../../tests/fixtures/ffprobe/audio-only.json"
        ))
        .unwrap_err();
        assert!(matches!(error, OutputError::InvalidMedia));
    }
}
