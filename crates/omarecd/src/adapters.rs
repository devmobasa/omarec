use std::path::{Path, PathBuf};
use std::time::Duration;

use omarec_backend_gsr::{GsrCli, GsrStatus};
use omarec_core::{PlannedCommand, SessionId};
use omarec_supervisor::{SystemdSupervisor, UnitState};

use crate::coordinator::{CoordinatorError, RecorderControl, RecorderStatus, Supervisor};

impl Supervisor for SystemdSupervisor {
    async fn start(&self, command: &PlannedCommand) -> Result<(), CoordinatorError> {
        SystemdSupervisor::start(self, command)
            .await
            .map_err(failed)
    }

    async fn unit_state(&self, session_id: SessionId) -> Result<UnitState, CoordinatorError> {
        SystemdSupervisor::unit_state(self, session_id)
            .await
            .map_err(failed)
    }

    async fn wait_inactive(
        &self,
        session_id: SessionId,
        deadline: Duration,
    ) -> Result<UnitState, CoordinatorError> {
        SystemdSupervisor::wait_inactive(self, session_id, deadline)
            .await
            .map_err(failed)
    }

    async fn stop_fallback(&self, session_id: SessionId) -> Result<(), CoordinatorError> {
        SystemdSupervisor::stop_fallback(self, session_id)
            .await
            .map_err(failed)
    }

    async fn journal_tail(
        &self,
        session_id: SessionId,
        lines: u32,
        max_bytes: usize,
    ) -> Result<String, CoordinatorError> {
        SystemdSupervisor::journal_tail(self, session_id, lines, max_bytes)
            .await
            .map_err(failed)
    }

    async fn cleanup(&self, session_id: SessionId) -> Result<(), CoordinatorError> {
        SystemdSupervisor::cleanup(self, session_id)
            .await
            .map_err(failed)
    }
}

impl RecorderControl for GsrCli {
    async fn status(&self, socket: &Path) -> Result<RecorderStatus, CoordinatorError> {
        match GsrCli::status(self, socket).await.map_err(failed)? {
            GsrStatus::Running => Ok(RecorderStatus::Running),
            GsrStatus::NotRunning => Ok(RecorderStatus::NotRunning),
        }
    }

    async fn set_paused(&self, socket: &Path, paused: bool) -> Result<(), CoordinatorError> {
        GsrCli::set_paused(self, socket, paused)
            .await
            .map_err(failed)
    }

    async fn stop(&self, socket: &Path) -> Result<PathBuf, CoordinatorError> {
        GsrCli::stop(self, socket).await.map_err(failed)
    }
}

fn failed(error: impl std::fmt::Display) -> CoordinatorError {
    CoordinatorError::Failed(error.to_string())
}
