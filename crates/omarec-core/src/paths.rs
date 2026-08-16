use std::env;
use std::fs;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppPaths {
    pub runtime_root: PathBuf,
    pub control_socket: PathBuf,
    pub sessions_runtime: PathBuf,
    pub state_root: PathBuf,
    pub sessions_state: PathBuf,
    pub config_file: PathBuf,
}

impl AppPaths {
    pub fn discover() -> Result<Self, PathError> {
        let runtime_base = env::var_os("XDG_RUNTIME_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .ok_or(PathError::MissingRuntimeDirectory)?;

        let home = env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let state_base = env::var_os("XDG_STATE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".local/state")))
            .ok_or(PathError::MissingHomeDirectory)?;
        let config_base = env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| home.as_ref().map(|path| path.join(".config")))
            .ok_or(PathError::MissingHomeDirectory)?;

        let runtime_root = runtime_base.join("omarec");
        let state_root = state_base.join("omarec");
        Ok(Self {
            control_socket: runtime_root.join("control.sock"),
            sessions_runtime: runtime_root.join("sessions"),
            sessions_state: state_root.join("sessions"),
            config_file: config_base.join("omarec/config.toml"),
            runtime_root,
            state_root,
        })
    }

    pub fn ensure_directories(&self) -> Result<(), PathError> {
        create_private_dir(&self.runtime_root)?;
        create_private_dir(&self.sessions_runtime)?;
        fs::create_dir_all(&self.sessions_state).map_err(|source| PathError::Create {
            path: self.sessions_state.clone(),
            source,
        })?;
        Ok(())
    }

    pub fn session_runtime(&self, session_id: &impl ToString) -> PathBuf {
        self.sessions_runtime.join(session_id.to_string())
    }
}

fn create_private_dir(path: &Path) -> Result<(), PathError> {
    fs::create_dir_all(path).map_err(|source| PathError::Create {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
        PathError::Permissions {
            path: path.to_path_buf(),
            source,
        }
    })?;
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum PathError {
    #[error("XDG_RUNTIME_DIR is required; refusing an insecure /tmp fallback")]
    MissingRuntimeDirectory,
    #[error("HOME, XDG_STATE_HOME, or XDG_CONFIG_HOME is required")]
    MissingHomeDirectory,
    #[error("failed to create {path}: {source}")]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to set private permissions on {path}: {source}")]
    Permissions {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}
