use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::{AudioSource, EvaluatedSpec, SessionId, WebcamConfig};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    #[default]
    Idle,
    Preparing,
    Launching,
    Recording,
    Paused,
    Stopping,
    Finalizing,
    Recovering,
    Completed,
    Cancelled,
    Failed,
}

impl SessionPhase {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }

    pub const fn is_active(self) -> bool {
        !matches!(
            self,
            Self::Idle | Self::Completed | Self::Cancelled | Self::Failed
        )
    }

    pub const fn all() -> [Self; 11] {
        [
            Self::Idle,
            Self::Preparing,
            Self::Launching,
            Self::Recording,
            Self::Paused,
            Self::Stopping,
            Self::Finalizing,
            Self::Recovering,
            Self::Completed,
            Self::Cancelled,
            Self::Failed,
        ]
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub session_id: Option<SessionId>,
    pub phase: SessionPhase,
    pub output: Option<PathBuf>,
    pub started_realtime_ms: Option<u64>,
    pub first_frame_monotonic_us: Option<u64>,
    pub paused: bool,
    pub last_error: Option<String>,
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default)]
    pub desktop_audio: bool,
    #[serde(default)]
    pub microphone: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webcam_summary: Option<String>,
}

impl SessionSnapshot {
    pub fn from_record(record: &SessionRecord) -> Self {
        let mut snapshot = Self {
            session_id: Some(record.session_id),
            phase: record.phase,
            output: Some(record.final_output.clone()),
            started_realtime_ms: Some(record.created_unix_ms),
            first_frame_monotonic_us: None,
            paused: record.phase == SessionPhase::Paused,
            last_error: record.last_error.clone(),
            warnings: record.warnings.clone(),
            ..Self::default()
        };
        if let Some(evaluated) = &record.evaluated {
            snapshot.apply_evaluated(evaluated);
        }
        snapshot
    }

    pub fn apply_evaluated(&mut self, evaluated: &EvaluatedSpec) {
        self.target_summary = Some(evaluated.spec.target.summary());
        self.profile = Some(format!(
            "{}/{}",
            evaluated.codec.as_gsr_value(),
            evaluated.spec.container.extension()
        ));
        self.desktop_audio = evaluated
            .spec
            .audio_tracks
            .iter()
            .any(|track| track.sources.iter().any(AudioSource::is_desktop));
        self.microphone = evaluated
            .spec
            .audio_tracks
            .iter()
            .any(|track| track.sources.iter().any(AudioSource::is_microphone));
        self.webcam_summary = evaluated.spec.webcam.as_ref().map(WebcamConfig::summary);
    }
}

#[derive(Clone, Debug, Default)]
pub struct SessionMachine {
    snapshot: SessionSnapshot,
}

impl SessionMachine {
    pub fn snapshot(&self) -> &SessionSnapshot {
        &self.snapshot
    }

    pub fn begin(&mut self, session_id: SessionId, output: PathBuf) -> Result<(), TransitionError> {
        if self.snapshot.phase != SessionPhase::Idle {
            return Err(TransitionError::SessionAlreadyActive(self.snapshot.phase));
        }
        self.snapshot = SessionSnapshot {
            session_id: Some(session_id),
            phase: SessionPhase::Preparing,
            output: Some(output),
            ..SessionSnapshot::default()
        };
        Ok(())
    }

    pub fn transition(&mut self, next: SessionPhase) -> Result<(), TransitionError> {
        let current = self.snapshot.phase;
        if !allowed(current, next) {
            return Err(TransitionError::Invalid { current, next });
        }
        self.snapshot.phase = next;
        self.snapshot.paused = next == SessionPhase::Paused;
        Ok(())
    }

    pub fn record_error(&mut self, message: impl Into<String>) {
        self.snapshot.last_error = Some(message.into());
    }

    pub fn push_warning(&mut self, message: impl Into<String>) {
        self.snapshot.warnings.push(message.into());
    }

    /// Clear a terminal session after clients have observed it.
    pub fn reset(&mut self) -> Result<(), TransitionError> {
        if !self.snapshot.phase.is_terminal() {
            return Err(TransitionError::CannotReset(self.snapshot.phase));
        }
        self.snapshot = SessionSnapshot::default();
        Ok(())
    }
}

pub const fn allowed(current: SessionPhase, next: SessionPhase) -> bool {
    use SessionPhase as P;
    matches!(
        (current, next),
        (P::Preparing, P::Launching | P::Cancelled | P::Failed)
            | (
                P::Launching,
                P::Recording | P::Stopping | P::Cancelled | P::Failed | P::Recovering
            )
            | (
                P::Recording,
                P::Paused | P::Stopping | P::Failed | P::Recovering
            )
            | (
                P::Paused,
                P::Recording | P::Stopping | P::Failed | P::Recovering
            )
            | (
                P::Stopping,
                P::Finalizing | P::Completed | P::Cancelled | P::Failed | P::Recovering
            )
            | (P::Finalizing, P::Completed | P::Failed)
            | (
                P::Recovering,
                P::Recording | P::Paused | P::Stopping | P::Finalizing | P::Cancelled | P::Failed
            )
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum TransitionError {
    #[error("a session is already active in phase {0:?}")]
    SessionAlreadyActive(SessionPhase),
    #[error("invalid recording transition from {current:?} to {next:?}")]
    Invalid {
        current: SessionPhase,
        next: SessionPhase,
    },
    #[error("cannot reset a non-terminal session in phase {0:?}")]
    CannotReset(SessionPhase),
}

pub const DURABLE_RECORD_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub schema_version: u32,
    pub session_id: SessionId,
    pub phase: SessionPhase,
    pub operation_generation: u64,
    pub unit_name: Option<String>,
    pub runtime_directory: PathBuf,
    pub gsr_ipc_socket: PathBuf,
    pub first_frame_timestamp: PathBuf,
    pub staging_output: PathBuf,
    pub final_output: PathBuf,
    pub evaluated: Option<EvaluatedSpec>,
    pub created_unix_ms: u64,
    pub updated_unix_ms: u64,
    pub last_error: Option<String>,
    pub warnings: Vec<String>,
}

impl SessionRecord {
    pub fn new(session_id: SessionId, final_output: PathBuf, now_unix_ms: u64) -> Self {
        Self {
            schema_version: DURABLE_RECORD_SCHEMA_VERSION,
            session_id,
            phase: SessionPhase::Preparing,
            operation_generation: 1,
            unit_name: None,
            runtime_directory: PathBuf::new(),
            gsr_ipc_socket: PathBuf::new(),
            first_frame_timestamp: PathBuf::new(),
            staging_output: PathBuf::new(),
            final_output,
            evaluated: None,
            created_unix_ms: now_unix_ms,
            updated_unix_ms: now_unix_ms,
            last_error: None,
            warnings: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn happy_path_is_explicit() {
        let mut machine = SessionMachine::default();
        machine
            .begin(SessionId::new(), PathBuf::from("capture.mp4"))
            .unwrap();
        for phase in [
            SessionPhase::Launching,
            SessionPhase::Recording,
            SessionPhase::Paused,
            SessionPhase::Recording,
            SessionPhase::Stopping,
            SessionPhase::Finalizing,
            SessionPhase::Completed,
        ] {
            machine.transition(phase).unwrap();
        }
        machine.reset().unwrap();
        assert_eq!(machine.snapshot().phase, SessionPhase::Idle);
    }

    #[test]
    fn pause_before_recording_is_rejected() {
        let mut machine = SessionMachine::default();
        machine
            .begin(SessionId::new(), PathBuf::from("capture.mp4"))
            .unwrap();
        assert!(machine.transition(SessionPhase::Paused).is_err());
    }

    #[test]
    fn stop_during_launch_can_cancel() {
        let mut machine = SessionMachine::default();
        machine
            .begin(SessionId::new(), PathBuf::from("capture.mp4"))
            .unwrap();
        machine.transition(SessionPhase::Launching).unwrap();
        machine.transition(SessionPhase::Cancelled).unwrap();
        assert!(machine.snapshot().phase.is_terminal());
    }

    #[test]
    fn transition_table_matches_production_function() {
        use SessionPhase as P;
        let allowed_pairs = [
            (P::Preparing, P::Launching),
            (P::Preparing, P::Cancelled),
            (P::Preparing, P::Failed),
            (P::Launching, P::Recording),
            (P::Launching, P::Stopping),
            (P::Launching, P::Cancelled),
            (P::Launching, P::Failed),
            (P::Launching, P::Recovering),
            (P::Recording, P::Paused),
            (P::Recording, P::Stopping),
            (P::Recording, P::Failed),
            (P::Recording, P::Recovering),
            (P::Paused, P::Recording),
            (P::Paused, P::Stopping),
            (P::Paused, P::Failed),
            (P::Paused, P::Recovering),
            (P::Stopping, P::Finalizing),
            (P::Stopping, P::Completed),
            (P::Stopping, P::Cancelled),
            (P::Stopping, P::Failed),
            (P::Stopping, P::Recovering),
            (P::Finalizing, P::Completed),
            (P::Finalizing, P::Failed),
            (P::Recovering, P::Recording),
            (P::Recovering, P::Paused),
            (P::Recovering, P::Stopping),
            (P::Recovering, P::Finalizing),
            (P::Recovering, P::Cancelled),
            (P::Recovering, P::Failed),
        ];
        for current in SessionPhase::all() {
            for next in SessionPhase::all() {
                let expected = allowed_pairs.contains(&(current, next));
                assert_eq!(
                    allowed(current, next),
                    expected,
                    "transition {current:?} -> {next:?}"
                );
            }
        }
    }

    #[test]
    fn awaiting_selection_is_not_a_daemon_phase() {
        let json = serde_json::to_string(&SessionPhase::Preparing).unwrap();
        assert!(!json.contains("awaiting"));
        assert!(serde_json::from_str::<SessionPhase>("\"awaiting_selection\"").is_err());
    }
}
