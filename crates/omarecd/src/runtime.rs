//! Runtime directory and peer-credential checks that do not follow symlinks.

use std::fs;
use std::io::{self, ErrorKind};
use std::os::fd::AsFd;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("runtime path {0} is a symlink; refusing to follow it")]
    Symlink(std::path::PathBuf),
    #[error("runtime path {0} is not owned by the current user")]
    WrongOwner(std::path::PathBuf),
    #[error("failed to inspect {path}: {source}")]
    Inspect {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("peer credentials are unavailable: {0}")]
    PeerCredentials(#[from] rustix::io::Errno),
    #[error("client UID {peer} does not match daemon UID {daemon}")]
    PeerMismatch { peer: u32, daemon: u32 },
}

pub fn current_uid() -> io::Result<u32> {
    fs::metadata("/proc/self").map(|metadata| metadata.uid())
}

pub fn lstat(path: &Path) -> Result<fs::Metadata, RuntimeError> {
    fs::symlink_metadata(path).map_err(|source| RuntimeError::Inspect {
        path: path.to_path_buf(),
        source,
    })
}

pub fn ensure_private_dir(path: &Path) -> Result<(), RuntimeError> {
    match lstat(path) {
        Ok(metadata) => {
            reject_symlink(path, &metadata)?;
            reject_wrong_owner(path, &metadata)?;
            #[cfg(unix)]
            if metadata.mode() & 0o777 != 0o700 {
                fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
                    RuntimeError::Inspect {
                        path: path.to_path_buf(),
                        source,
                    }
                })?;
            }
            Ok(())
        }
        Err(RuntimeError::Inspect { source, .. }) if source.kind() == ErrorKind::NotFound => {
            fs::create_dir_all(path).map_err(|source| RuntimeError::Inspect {
                path: path.to_path_buf(),
                source,
            })?;
            #[cfg(unix)]
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
                RuntimeError::Inspect {
                    path: path.to_path_buf(),
                    source,
                }
            })?;
            let metadata = lstat(path)?;
            reject_symlink(path, &metadata)?;
            reject_wrong_owner(path, &metadata)?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

pub fn reject_symlink(path: &Path, metadata: &fs::Metadata) -> Result<(), RuntimeError> {
    if metadata.file_type().is_symlink() {
        return Err(RuntimeError::Symlink(path.to_path_buf()));
    }
    Ok(())
}

fn reject_wrong_owner(path: &Path, metadata: &fs::Metadata) -> Result<(), RuntimeError> {
    let uid = current_uid().map_err(|source| RuntimeError::Inspect {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.uid() != uid {
        return Err(RuntimeError::WrongOwner(path.to_path_buf()));
    }
    Ok(())
}

/// Verify the connected peer's effective UID with `SO_PEERCRED`.
///
/// `std::os::unix::net::UnixStream::peer_cred` is a nightly API
/// (`peer_credentials_unix_socket`). Use rustix's stable getsockopt instead,
/// which works on any `AsFd` including Tokio's Unix stream.
pub fn verify_peer(stream: impl AsFd) -> Result<(), RuntimeError> {
    #[cfg(target_os = "linux")]
    {
        let cred = rustix::net::sockopt::socket_peercred(stream)?;
        let daemon = current_uid().map_err(|source| RuntimeError::Inspect {
            path: Path::new("/proc/self").to_path_buf(),
            source,
        })?;
        if cred.uid.as_raw() != daemon {
            return Err(RuntimeError::PeerMismatch {
                peer: cred.uid.as_raw(),
                daemon,
            });
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = stream;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn scratch(name: &str) -> std::path::PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("omarec-{name}-{unique}"))
    }

    #[test]
    fn private_dir_is_0700_and_not_a_symlink() {
        let path = scratch("runtime");
        ensure_private_dir(&path).unwrap();
        let metadata = lstat(&path).unwrap();
        assert!(!metadata.file_type().is_symlink());
        assert_eq!(metadata.mode() & 0o777, 0o700);
        fs::remove_dir_all(&path).unwrap();
    }

    #[test]
    fn symlink_runtime_dir_is_rejected() {
        let real = scratch("real");
        fs::create_dir_all(&real).unwrap();
        let link = scratch("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let error = ensure_private_dir(&link).unwrap_err();
        assert!(matches!(error, RuntimeError::Symlink(_)));
        fs::remove_file(&link).unwrap();
        fs::remove_dir_all(&real).unwrap();
    }

    #[test]
    fn same_uid_peer_is_accepted_without_nightly_peer_cred() {
        let (left, right) = std::os::unix::net::UnixStream::pair().unwrap();
        verify_peer(&left).unwrap();
        verify_peer(&right).unwrap();
        let flags = rustix::io::fcntl_getfd(&left).unwrap();
        assert!(flags.contains(rustix::io::FdFlags::CLOEXEC));
    }
}
