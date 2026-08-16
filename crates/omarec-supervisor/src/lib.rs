//! Per-session systemd user-service supervision.
//!
//! GSR IPC is the normal control path. systemd owns the cgroup and is the recovery/fallback path.

use std::path::PathBuf;
use std::time::Duration;

use omarec_core::{PlannedCommand, SessionId};
use tokio::process::Command;
use tokio::time::timeout;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum UnitState {
    Active,
    #[default]
    Inactive,
    Failed,
}

#[derive(Clone, Debug)]
pub struct SystemdSupervisor {
    systemd_run_binary: PathBuf,
    systemctl_binary: PathBuf,
    journalctl_binary: PathBuf,
    timeout: Duration,
}

impl SystemdSupervisor {
    pub fn new(
        systemd_run_binary: impl Into<PathBuf>,
        systemctl_binary: impl Into<PathBuf>,
    ) -> Self {
        Self {
            systemd_run_binary: systemd_run_binary.into(),
            systemctl_binary: systemctl_binary.into(),
            journalctl_binary: PathBuf::from("journalctl"),
            timeout: Duration::from_secs(10),
        }
    }

    #[must_use]
    pub fn with_journalctl(mut self, journalctl_binary: impl Into<PathBuf>) -> Self {
        self.journalctl_binary = journalctl_binary.into();
        self
    }

    pub fn unit_name(session_id: SessionId) -> String {
        format!("omarec-session-{session_id}.service")
    }

    pub fn parse_unit_state(is_active: bool, is_failed: bool) -> UnitState {
        if is_active {
            UnitState::Active
        } else if is_failed {
            UnitState::Failed
        } else {
            UnitState::Inactive
        }
    }

    pub fn is_active_argv(unit: &str) -> Vec<String> {
        vec![
            "--user".to_owned(),
            "is-active".to_owned(),
            "--quiet".to_owned(),
            unit.to_owned(),
        ]
    }

    pub fn is_failed_argv(unit: &str) -> Vec<String> {
        vec![
            "--user".to_owned(),
            "is-failed".to_owned(),
            "--quiet".to_owned(),
            unit.to_owned(),
        ]
    }

    pub fn reset_failed_argv(unit: &str) -> Vec<String> {
        vec![
            "--user".to_owned(),
            "reset-failed".to_owned(),
            unit.to_owned(),
        ]
    }

    pub fn journal_argv(unit: &str, lines: u32) -> Vec<String> {
        vec![
            "--user".to_owned(),
            "-u".to_owned(),
            unit.to_owned(),
            "-n".to_owned(),
            lines.to_string(),
            "--no-pager".to_owned(),
            "-o".to_owned(),
            "cat".to_owned(),
        ]
    }

    pub fn plan(&self, session_id: SessionId, child: &PlannedCommand) -> PlannedCommand {
        let mut arguments = vec![
            "--user".to_owned(),
            format!("--unit={}", Self::unit_name(session_id)),
            "--quiet".to_owned(),
            "--property=Type=exec".to_owned(),
            "--property=Restart=no".to_owned(),
            "--property=KillSignal=SIGINT".to_owned(),
            "--property=TimeoutStopSec=20s".to_owned(),
            "--property=SendSIGKILL=yes".to_owned(),
            "--property=StandardOutput=journal".to_owned(),
            "--property=StandardError=journal".to_owned(),
            "--".to_owned(),
            child.program.display().to_string(),
        ];
        arguments.extend(child.arguments.iter().cloned());
        PlannedCommand {
            program: self.systemd_run_binary.clone(),
            arguments,
            environment: filter_environment(&child.environment),
        }
    }

    pub async fn start(&self, command: &PlannedCommand) -> Result<(), SupervisorError> {
        let output = timeout(
            self.timeout,
            Command::new(&command.program)
                .args(&command.arguments)
                .envs(command.environment.iter().cloned())
                .output(),
        )
        .await
        .map_err(|_| SupervisorError::Timeout(self.timeout))?
        .map_err(SupervisorError::Spawn)?;
        if !output.status.success() {
            return Err(SupervisorError::Rejected {
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(())
    }

    /// Fallback only. Prefer `gsr-cli ... stop`, which acknowledges the saved file.
    pub async fn stop_fallback(&self, session_id: SessionId) -> Result<(), SupervisorError> {
        let unit = Self::unit_name(session_id);
        self.systemctl(&["stop", &unit]).await
    }

    pub async fn is_active(&self, session_id: SessionId) -> Result<bool, SupervisorError> {
        let unit = Self::unit_name(session_id);
        Ok(self
            .systemctl_status(&Self::is_active_argv(&unit))
            .await?
            .success())
    }

    pub async fn unit_state(&self, session_id: SessionId) -> Result<UnitState, SupervisorError> {
        let unit = Self::unit_name(session_id);
        let is_active = self
            .systemctl_status(&Self::is_active_argv(&unit))
            .await?
            .success();
        let is_failed = self
            .systemctl_status(&Self::is_failed_argv(&unit))
            .await?
            .success();
        Ok(Self::parse_unit_state(is_active, is_failed))
    }

    pub async fn wait_inactive(
        &self,
        session_id: SessionId,
        deadline: Duration,
    ) -> Result<UnitState, SupervisorError> {
        let started = tokio::time::Instant::now();
        loop {
            let state = self.unit_state(session_id).await?;
            if matches!(state, UnitState::Inactive | UnitState::Failed) {
                return Ok(state);
            }
            if started.elapsed() >= deadline {
                return Err(SupervisorError::Timeout(deadline));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn journal_tail(
        &self,
        session_id: SessionId,
        lines: u32,
        max_bytes: usize,
    ) -> Result<String, SupervisorError> {
        let unit = Self::unit_name(session_id);
        let output = timeout(
            self.timeout,
            Command::new(&self.journalctl_binary)
                .args(Self::journal_argv(&unit, lines))
                .output(),
        )
        .await
        .map_err(|_| SupervisorError::Timeout(self.timeout))?
        .map_err(SupervisorError::Spawn)?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        if text.len() > max_bytes {
            text.truncate(max_bytes);
        }
        Ok(text)
    }

    /// Reset the exact unit after terminal evidence is durable. Never uses `--collect`.
    pub async fn cleanup(&self, session_id: SessionId) -> Result<(), SupervisorError> {
        let unit = Self::unit_name(session_id);
        match self.systemctl(&["reset-failed", &unit]).await {
            Ok(()) | Err(SupervisorError::Rejected { .. }) => Ok(()),
            Err(error) => Err(error),
        }
    }

    async fn systemctl_status(
        &self,
        arguments: &[String],
    ) -> Result<std::process::ExitStatus, SupervisorError> {
        let output = timeout(
            self.timeout,
            Command::new(&self.systemctl_binary)
                .args(arguments)
                .output(),
        )
        .await
        .map_err(|_| SupervisorError::Timeout(self.timeout))?
        .map_err(SupervisorError::Spawn)?;
        Ok(output.status)
    }

    async fn systemctl(&self, arguments: &[&str]) -> Result<(), SupervisorError> {
        let output = timeout(
            self.timeout,
            Command::new(&self.systemctl_binary)
                .arg("--user")
                .args(arguments)
                .output(),
        )
        .await
        .map_err(|_| SupervisorError::Timeout(self.timeout))?
        .map_err(SupervisorError::Spawn)?;
        if !output.status.success() {
            return Err(SupervisorError::Rejected {
                code: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(())
    }
}

const ALLOWED_ENVIRONMENT: &[&str] = &[
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    "XDG_SESSION_ID",
    "XDG_CURRENT_DESKTOP",
    "DISPLAY",
    "HOME",
    "LANG",
    "LC_ALL",
    "HYPRLAND_INSTANCE_SIGNATURE",
    "DRI_PRIME",
    "__NV_PRIME_RENDER_OFFLOAD",
    "__VK_LAYER_NV_optimus",
    "VK_ICD_FILENAMES",
    "LIBVA_DRIVER_NAME",
];

fn filter_environment(environment: &[(String, String)]) -> Vec<(String, String)> {
    environment
        .iter()
        .filter(|(key, _)| ALLOWED_ENVIRONMENT.contains(&key.as_str()))
        .cloned()
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("failed to execute systemd user command: {0}")]
    Spawn(std::io::Error),
    #[error("systemd user command timed out after {0:?}")]
    Timeout(Duration),
    #[error("systemd user command failed (exit {code:?}): {stderr}")]
    Rejected { code: Option<i32>, stderr: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_uses_exec_type_and_sigint_fallback() {
        let id = SessionId::new();
        let child = PlannedCommand {
            program: PathBuf::from("gpu-screen-recorder"),
            arguments: vec!["-w".to_owned(), "DP-1".to_owned()],
            environment: Vec::new(),
        };
        let plan = SystemdSupervisor::new("systemd-run", "systemctl").plan(id, &child);
        assert!(
            plan.arguments
                .iter()
                .any(|arg| arg == "--property=Type=exec")
        );
        assert!(
            plan.arguments
                .iter()
                .any(|arg| arg == "--property=KillSignal=SIGINT")
        );
        assert!(!plan.arguments.iter().any(|arg| arg == "--collect"));
    }

    #[test]
    fn plan_allows_only_display_session_and_gpu_environment() {
        let id = "00000000-0000-0000-0000-000000000000"
            .parse::<SessionId>()
            .unwrap();
        let child = PlannedCommand {
            program: PathBuf::from("gpu-screen-recorder"),
            arguments: vec!["-w".to_owned(), "DP-1".to_owned()],
            environment: vec![
                ("WAYLAND_DISPLAY".to_owned(), "wayland-1".to_owned()),
                ("SECRET".to_owned(), "nope".to_owned()),
                ("PATH".to_owned(), "/tmp/evil".to_owned()),
                ("DRI_PRIME".to_owned(), "1".to_owned()),
            ],
        };
        let plan = SystemdSupervisor::new("systemd-run", "systemctl").plan(id, &child);
        assert_eq!(
            plan.environment,
            [
                ("WAYLAND_DISPLAY".to_owned(), "wayland-1".to_owned()),
                ("DRI_PRIME".to_owned(), "1".to_owned()),
            ]
        );
        assert_eq!(
            plan.arguments[1],
            "--unit=omarec-session-00000000-0000-0000-0000-000000000000.service"
        );
    }

    #[test]
    fn unit_state_prefers_active_over_failed() {
        assert_eq!(
            SystemdSupervisor::parse_unit_state(true, true),
            UnitState::Active
        );
        assert_eq!(
            SystemdSupervisor::parse_unit_state(false, true),
            UnitState::Failed
        );
        assert_eq!(
            SystemdSupervisor::parse_unit_state(false, false),
            UnitState::Inactive
        );
    }

    #[test]
    fn cleanup_and_journal_target_the_exact_unit() {
        let unit = "omarec-session-00000000-0000-0000-0000-000000000000.service";
        assert_eq!(
            SystemdSupervisor::reset_failed_argv(unit),
            ["--user", "reset-failed", unit]
        );
        let journal = SystemdSupervisor::journal_argv(unit, 50);
        assert!(journal.contains(&"-u".to_owned()));
        assert!(journal.contains(&unit.to_owned()));
        assert!(
            !journal
                .iter()
                .any(|arg| arg.contains("gpu-screen-recorder"))
        );
        assert!(
            !SystemdSupervisor::is_active_argv(unit)
                .iter()
                .any(|arg| arg == "--collect")
        );
    }
}
