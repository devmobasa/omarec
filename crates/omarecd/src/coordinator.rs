//! Single-writer session coordinator. State mutexes are not held across systemd/GSR awaits.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use omarec_core::{
    EvaluatedSpec, FirstFrameTimestamp, PlannedCommand, SessionId, SessionPhase, SessionRecord,
    SessionSnapshot, TimestampError, parse_first_frame_timestamp,
};
use tokio::sync::Mutex;

pub use omarec_supervisor::UnitState;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RecorderStatus {
    Running,
    #[default]
    NotRunning,
}

pub trait Supervisor: Send + Sync {
    async fn start(&self, command: &PlannedCommand) -> Result<(), CoordinatorError>;
    async fn unit_state(&self, session_id: SessionId) -> Result<UnitState, CoordinatorError>;
    async fn wait_inactive(
        &self,
        session_id: SessionId,
        deadline: Duration,
    ) -> Result<UnitState, CoordinatorError>;
    async fn stop_fallback(&self, session_id: SessionId) -> Result<(), CoordinatorError>;
    async fn journal_tail(
        &self,
        session_id: SessionId,
        lines: u32,
        max_bytes: usize,
    ) -> Result<String, CoordinatorError>;
    async fn cleanup(&self, session_id: SessionId) -> Result<(), CoordinatorError>;
}

pub trait RecorderControl: Send + Sync {
    async fn status(&self, socket: &Path) -> Result<RecorderStatus, CoordinatorError>;
    async fn set_paused(&self, socket: &Path, paused: bool) -> Result<(), CoordinatorError>;
    async fn stop(&self, socket: &Path) -> Result<PathBuf, CoordinatorError>;
}

pub trait SessionStore: Send + Sync {
    async fn create(&self, record: SessionRecord) -> Result<(), CoordinatorError>;
    async fn load_active(&self) -> Result<Option<SessionRecord>, CoordinatorError>;
    async fn save(&self, record: SessionRecord) -> Result<(), CoordinatorError>;
    async fn load_nonterminal(&self) -> Result<Vec<SessionRecord>, CoordinatorError>;
}

pub trait CoordinatorClock: Send + Sync {
    fn unix_ms(&self) -> u64;
    fn session_id(&self) -> SessionId;
}

pub trait TimestampSource: Send + Sync {
    fn read(&self, path: &Path) -> Result<Option<String>, CoordinatorError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FsTimestamps;

impl TimestampSource for FsTimestamps {
    fn read(&self, path: &Path) -> Result<Option<String>, CoordinatorError> {
        match std::fs::read_to_string(path) {
            Ok(source) if source.trim().is_empty() => Ok(None),
            Ok(source) => Ok(Some(source)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(CoordinatorError::Failed(error.to_string())),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Admission {
    pub session_id: SessionId,
    pub evaluated: EvaluatedSpec,
    pub runtime_directory: PathBuf,
    pub gsr_ipc_socket: PathBuf,
    pub first_frame_timestamp: PathBuf,
    pub staging_output: PathBuf,
    pub unit_name: String,
}

#[derive(Clone, Debug)]
pub struct Coordinator<S, R, T, C, F = FsTimestamps> {
    supervisor: S,
    recorder: R,
    store: T,
    clock: C,
    timestamps: F,
    inner: Arc<Mutex<Inner>>,
    operation: Arc<Mutex<()>>,
    cancel_requested: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    operation_generation: Arc<AtomicU64>,
    startup_timeout: Duration,
}

#[derive(Debug, Default)]
struct Inner {
    snapshot: SessionSnapshot,
}

impl<S, R, T, C> Coordinator<S, R, T, C, FsTimestamps>
where
    S: Supervisor,
    R: RecorderControl,
    T: SessionStore,
    C: CoordinatorClock,
{
    pub fn new(supervisor: S, recorder: R, store: T, clock: C) -> Self {
        Self::with_timestamps(supervisor, recorder, store, clock, FsTimestamps)
    }
}

impl<S, R, T, C, F> Coordinator<S, R, T, C, F>
where
    S: Supervisor,
    R: RecorderControl,
    T: SessionStore,
    C: CoordinatorClock,
    F: TimestampSource,
{
    pub fn with_timestamps(supervisor: S, recorder: R, store: T, clock: C, timestamps: F) -> Self {
        Self {
            supervisor,
            recorder,
            store,
            clock,
            timestamps,
            inner: Arc::new(Mutex::new(Inner::default())),
            operation: Arc::new(Mutex::new(())),
            cancel_requested: Arc::new(AtomicBool::new(false)),
            shutdown: Arc::new(AtomicBool::new(false)),
            operation_generation: Arc::new(AtomicU64::new(0)),
            startup_timeout: Duration::from_secs(10),
        }
    }

    #[must_use]
    pub fn with_startup_timeout(mut self, timeout: Duration) -> Self {
        self.startup_timeout = timeout;
        self
    }

    pub fn request_shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
    }

    pub fn allocate_session_id(&self) -> SessionId {
        self.clock.session_id()
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        self.inner.lock().await.snapshot.clone()
    }

    /// Persist `preparing` and own the admission slot. The caller may then respond `accepted`.
    pub async fn admit(&self, admission: Admission) -> Result<SessionId, CoordinatorError> {
        let Admission {
            session_id,
            evaluated,
            runtime_directory,
            gsr_ipc_socket,
            first_frame_timestamp,
            staging_output,
            unit_name,
        } = admission;
        let _guard = self.operation.lock().await;
        {
            let inner = self.inner.lock().await;
            if inner.snapshot.phase.is_active() {
                return Err(CoordinatorError::ActiveSession(inner.snapshot.phase));
            }
        }
        let now = self.clock.unix_ms();
        let generation = self.operation_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut record = SessionRecord::new(session_id, evaluated.spec.output.clone(), now);
        record.operation_generation = generation;
        record.unit_name = Some(unit_name);
        record.runtime_directory = runtime_directory;
        record.gsr_ipc_socket = gsr_ipc_socket;
        record.first_frame_timestamp = first_frame_timestamp;
        record.staging_output = staging_output;
        record.warnings = evaluated.warnings.clone();
        record.evaluated = Some(evaluated);
        record.phase = SessionPhase::Preparing;
        self.store.create(record.clone()).await?;
        self.cancel_requested.store(false, Ordering::SeqCst);
        {
            let mut inner = self.inner.lock().await;
            inner.snapshot = SessionSnapshot::from_record(&record);
        }
        Ok(session_id)
    }

    pub async fn launch(&self, command: &PlannedCommand) -> Result<(), CoordinatorError> {
        let (session_id, generation, timestamp_path, socket) = {
            let _guard = self.operation.lock().await;
            let inner = self.inner.lock().await;
            let session_id = inner
                .snapshot
                .session_id
                .ok_or(CoordinatorError::NoActiveSession)?;
            let generation = self.operation_generation.load(Ordering::SeqCst);
            drop(inner);
            self.persist(SessionPhase::Launching).await?;
            let record = self.require_record(session_id).await?;
            (
                session_id,
                generation,
                record.first_frame_timestamp,
                record.gsr_ipc_socket,
            )
        };

        if self.cancel_requested.load(Ordering::SeqCst) {
            self.persist(SessionPhase::Cancelled).await?;
            return Ok(());
        }

        self.supervisor.start(command).await?;

        match self
            .wait_ready(session_id, &socket, &timestamp_path, generation)
            .await
        {
            Ok(first_frame) => {
                if self.generation() != generation {
                    return Err(CoordinatorError::StaleGeneration);
                }
                self.persist(SessionPhase::Recording).await?;
                let mut inner = self.inner.lock().await;
                inner.snapshot.first_frame_monotonic_us = Some(first_frame.monotonic_us);
                Ok(())
            }
            Err(CoordinatorError::Cancelled) => {
                let _ = self.supervisor.stop_fallback(session_id).await;
                self.persist(SessionPhase::Cancelled).await?;
                Ok(())
            }
            Err(error) => {
                let _ = self.supervisor.stop_fallback(session_id).await;
                self.fail(&error).await?;
                Err(error)
            }
        }
    }

    pub async fn pause(&self, expected: SessionId) -> Result<(), CoordinatorError> {
        let _guard = self.operation.lock().await;
        let record = self.require_record(expected).await?;
        if record.phase == SessionPhase::Paused {
            return Ok(());
        }
        if record.phase != SessionPhase::Recording {
            return Err(CoordinatorError::InvalidState(record.phase));
        }
        self.recorder
            .set_paused(&record.gsr_ipc_socket, true)
            .await?;
        self.persist(SessionPhase::Paused).await
    }

    pub async fn resume(&self, expected: SessionId) -> Result<(), CoordinatorError> {
        let _guard = self.operation.lock().await;
        let record = self.require_record(expected).await?;
        if record.phase == SessionPhase::Recording {
            return Ok(());
        }
        if record.phase != SessionPhase::Paused {
            return Err(CoordinatorError::InvalidState(record.phase));
        }
        self.recorder
            .set_paused(&record.gsr_ipc_socket, false)
            .await?;
        self.persist(SessionPhase::Recording).await
    }

    pub async fn stop(
        &self,
        expected: SessionId,
        force: bool,
    ) -> Result<Option<PathBuf>, CoordinatorError> {
        self.cancel_requested.store(true, Ordering::SeqCst);
        let _guard = self.operation.lock().await;
        let record = self.require_record(expected).await?;
        if matches!(
            record.phase,
            SessionPhase::Stopping | SessionPhase::Finalizing | SessionPhase::Completed
        ) {
            return Ok(None);
        }
        if matches!(
            record.phase,
            SessionPhase::Preparing | SessionPhase::Launching
        ) {
            let _ = self.supervisor.stop_fallback(expected).await;
            self.persist(SessionPhase::Cancelled).await?;
            return Ok(None);
        }
        self.persist(SessionPhase::Stopping).await?;
        match self.recorder.stop(&record.gsr_ipc_socket).await {
            Ok(saved) => {
                self.persist(SessionPhase::Finalizing).await?;
                Ok(Some(saved))
            }
            Err(_) if force => {
                self.reconcile_then_fallback(expected).await?;
                self.persist(SessionPhase::Finalizing).await?;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    pub async fn active_record(&self) -> Result<Option<SessionRecord>, CoordinatorError> {
        self.store.load_active().await
    }

    pub async fn mark_completed(&self, expected: SessionId) -> Result<(), CoordinatorError> {
        let _guard = self.operation.lock().await;
        let record = self.require_record(expected).await?;
        if record.phase != SessionPhase::Finalizing && record.phase != SessionPhase::Recovering {
            return Err(CoordinatorError::InvalidState(record.phase));
        }
        self.persist(SessionPhase::Completed).await
    }

    /// Reconcile a durable non-terminal record after daemon restart.
    pub async fn recover(&self) -> Result<Option<SessionId>, CoordinatorError> {
        let records = self.store.load_nonterminal().await?;
        let Some(record) = records.into_iter().next() else {
            return Ok(None);
        };
        {
            let mut inner = self.inner.lock().await;
            inner.snapshot = snapshot_from(&record);
        }
        self.operation_generation
            .store(record.operation_generation, Ordering::SeqCst);
        match record.phase {
            SessionPhase::Preparing => {
                self.persist(SessionPhase::Cancelled).await?;
                Ok(Some(record.session_id))
            }
            SessionPhase::Launching => {
                if let Ok(UnitState::Active) = self.supervisor.unit_state(record.session_id).await {
                    self.persist(SessionPhase::Recovering).await?;
                } else {
                    self.persist(SessionPhase::Cancelled).await?;
                }
                Ok(Some(record.session_id))
            }
            SessionPhase::Recording | SessionPhase::Paused | SessionPhase::Stopping => {
                self.persist(SessionPhase::Recovering).await?;
                match self.supervisor.unit_state(record.session_id).await {
                    Ok(UnitState::Active) => {
                        let phase = if record.phase == SessionPhase::Paused {
                            SessionPhase::Paused
                        } else if record.phase == SessionPhase::Stopping {
                            SessionPhase::Stopping
                        } else {
                            SessionPhase::Recording
                        };
                        self.persist(phase).await?;
                    }
                    Ok(UnitState::Failed | UnitState::Inactive) => {
                        self.persist(SessionPhase::Failed).await?;
                    }
                    Err(error) => return Err(error),
                }
                Ok(Some(record.session_id))
            }
            SessionPhase::Finalizing | SessionPhase::Recovering => {
                self.persist(SessionPhase::Recovering).await?;
                if record.final_output.exists() {
                    self.persist(SessionPhase::Completed).await?;
                } else if record.staging_output.exists() {
                    let dest = record
                        .final_output
                        .parent()
                        .unwrap_or_else(|| Path::new("."));
                    match crate::output::promote(
                        dest,
                        &record.staging_output,
                        &record.staging_output,
                        &record.final_output,
                        &crate::output::AcceptingProbe,
                    ) {
                        Ok(()) => self.persist(SessionPhase::Completed).await?,
                        Err(error) => {
                            self.fail(&CoordinatorError::Failed(error.to_string()))
                                .await?;
                        }
                    }
                } else {
                    self.persist(SessionPhase::Failed).await?;
                }
                Ok(Some(record.session_id))
            }
            _ => Ok(Some(record.session_id)),
        }
    }

    async fn wait_ready(
        &self,
        session_id: SessionId,
        socket: &Path,
        timestamp_path: &Path,
        generation: u64,
    ) -> Result<FirstFrameTimestamp, CoordinatorError> {
        let deadline = tokio::time::Instant::now() + self.startup_timeout;
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                return Err(CoordinatorError::Shutdown);
            }
            if self.cancel_requested.load(Ordering::SeqCst) {
                return Err(CoordinatorError::Cancelled);
            }
            if self.generation() != generation {
                return Err(CoordinatorError::StaleGeneration);
            }
            match self.supervisor.unit_state(session_id).await? {
                UnitState::Failed => {
                    let journal = self
                        .supervisor
                        .journal_tail(session_id, 50, 8 * 1024)
                        .await
                        .unwrap_or_default();
                    return Err(CoordinatorError::UnitFailed(journal));
                }
                UnitState::Inactive => return Err(CoordinatorError::UnitExited),
                UnitState::Active => {}
            }
            match self.recorder.status(socket).await {
                Ok(RecorderStatus::Running) => {
                    if let Some(source) = self.timestamps.read(timestamp_path)? {
                        match parse_first_frame_timestamp(&source) {
                            Ok(timestamp) => return Ok(timestamp),
                            Err(
                                TimestampError::MissingMonotonic | TimestampError::MissingRealtime,
                            ) => {}
                            Err(error) => {
                                return Err(CoordinatorError::MalformedTimestamp(error));
                            }
                        }
                    }
                }
                Ok(RecorderStatus::NotRunning) | Err(_) => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CoordinatorError::StartupTimeout);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn reconcile_then_fallback(&self, session_id: SessionId) -> Result<(), CoordinatorError> {
        if let Ok(UnitState::Inactive | UnitState::Failed) =
            self.supervisor.unit_state(session_id).await
        {
            return Ok(());
        }
        self.supervisor.stop_fallback(session_id).await?;
        let _ = self
            .supervisor
            .wait_inactive(session_id, Duration::from_secs(20))
            .await;
        Ok(())
    }

    async fn fail(&self, error: &CoordinatorError) -> Result<(), CoordinatorError> {
        let mut inner = self.inner.lock().await;
        inner.snapshot.last_error = Some(error.to_string());
        drop(inner);
        self.persist(SessionPhase::Failed).await
    }

    async fn persist(&self, phase: SessionPhase) -> Result<(), CoordinatorError> {
        let (session_id, last_error) = {
            let inner = self.inner.lock().await;
            (
                inner
                    .snapshot
                    .session_id
                    .ok_or(CoordinatorError::NoActiveSession)?,
                inner.snapshot.last_error.clone(),
            )
        };
        let Some(mut record) = self.store.load_active().await? else {
            return Err(CoordinatorError::NoActiveSession);
        };
        if record.session_id != session_id {
            return Err(CoordinatorError::SessionMismatch {
                expected: session_id,
                actual: record.session_id,
            });
        }
        record.phase = phase;
        record.updated_unix_ms = self.clock.unix_ms();
        if phase == SessionPhase::Failed {
            record.last_error = last_error;
        }
        let session_id = record.session_id;
        self.store.save(record).await?;
        let mut inner = self.inner.lock().await;
        inner.snapshot.phase = phase;
        inner.snapshot.paused = phase == SessionPhase::Paused;
        drop(inner);
        if phase.is_terminal() {
            let _ = self.supervisor.cleanup(session_id).await;
        }
        Ok(())
    }

    async fn require_record(&self, expected: SessionId) -> Result<SessionRecord, CoordinatorError> {
        let inner = self.inner.lock().await;
        let Some(session_id) = inner.snapshot.session_id else {
            return Err(CoordinatorError::NoActiveSession);
        };
        if expected != session_id {
            return Err(CoordinatorError::SessionMismatch {
                expected,
                actual: session_id,
            });
        }
        drop(inner);
        self.store
            .load_active()
            .await?
            .ok_or(CoordinatorError::NoActiveSession)
    }

    fn generation(&self) -> u64 {
        self.operation_generation.load(Ordering::SeqCst)
    }
}

fn snapshot_from(record: &SessionRecord) -> SessionSnapshot {
    SessionSnapshot::from_record(record)
}

#[derive(Debug, thiserror::Error)]
pub enum CoordinatorError {
    #[error("a recording is already {0:?}")]
    ActiveSession(SessionPhase),
    #[error("there is no active recording session")]
    NoActiveSession,
    #[error("client expected session {expected} but the active session is {actual}")]
    SessionMismatch {
        expected: SessionId,
        actual: SessionId,
    },
    #[error("session is {0:?}")]
    InvalidState(SessionPhase),
    #[error("first-frame timestamp is malformed: {0}")]
    MalformedTimestamp(omarec_core::TimestampError),
    #[error("recorder did not become ready before the startup deadline")]
    StartupTimeout,
    #[error("session unit failed: {0}")]
    UnitFailed(String),
    #[error("session unit became inactive before the first frame")]
    UnitExited,
    #[error("stale operation completion was discarded")]
    StaleGeneration,
    #[error("recording was cancelled before the first frame")]
    Cancelled,
    #[error("daemon shutdown interrupted startup")]
    Shutdown,
    #[error("{0}")]
    Failed(String),
    #[error("durable session store failed: {0}")]
    Store(String),
}

impl CoordinatorError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ActiveSession(_) => "active_session",
            Self::NoActiveSession => "no_active_session",
            Self::SessionMismatch { .. } => "session_mismatch",
            Self::InvalidState(_) => "invalid_state",
            Self::MalformedTimestamp(_) => "malformed_timestamp",
            Self::StartupTimeout => "startup_timeout",
            Self::UnitFailed(_) => "unit_failed",
            Self::UnitExited => "unit_exited",
            Self::StaleGeneration => "stale_generation",
            Self::Cancelled => "cancelled",
            Self::Shutdown => "shutdown",
            Self::Failed(_) => "supervisor_failed",
            Self::Store(_) => "store_failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omarec_core::{
        AudioSource, AudioTrack, CaptureBackend, CaptureTarget, Codec, Container, FrameMode,
        PostprocessMode, RecordingSpec, SessionId,
    };
    use std::collections::HashMap;
    use std::sync::atomic::AtomicU64;

    #[derive(Default)]
    struct FakeSupervisor {
        started: Mutex<u32>,
        state: Mutex<UnitState>,
        fallback: Mutex<u32>,
        cleaned: Mutex<u32>,
        crash_after_start: Mutex<bool>,
    }

    impl Supervisor for FakeSupervisor {
        async fn start(&self, _command: &PlannedCommand) -> Result<(), CoordinatorError> {
            *self.started.lock().await += 1;
            if *self.crash_after_start.lock().await {
                *self.state.lock().await = UnitState::Failed;
            } else {
                *self.state.lock().await = UnitState::Active;
            }
            Ok(())
        }
        async fn unit_state(&self, _session_id: SessionId) -> Result<UnitState, CoordinatorError> {
            Ok(*self.state.lock().await)
        }
        async fn wait_inactive(
            &self,
            _session_id: SessionId,
            _deadline: Duration,
        ) -> Result<UnitState, CoordinatorError> {
            Ok(*self.state.lock().await)
        }
        async fn stop_fallback(&self, _session_id: SessionId) -> Result<(), CoordinatorError> {
            *self.fallback.lock().await += 1;
            *self.state.lock().await = UnitState::Inactive;
            Ok(())
        }
        async fn journal_tail(
            &self,
            _session_id: SessionId,
            _lines: u32,
            _max_bytes: usize,
        ) -> Result<String, CoordinatorError> {
            Ok("unit failed".to_owned())
        }
        async fn cleanup(&self, _session_id: SessionId) -> Result<(), CoordinatorError> {
            *self.cleaned.lock().await += 1;
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeRecorder {
        paused: Mutex<bool>,
        stopped: Mutex<bool>,
        fail_stop: Mutex<bool>,
        status: Mutex<RecorderStatus>,
    }

    impl RecorderControl for FakeRecorder {
        async fn status(&self, _socket: &Path) -> Result<RecorderStatus, CoordinatorError> {
            Ok(*self.status.lock().await)
        }
        async fn set_paused(&self, _socket: &Path, paused: bool) -> Result<(), CoordinatorError> {
            *self.paused.lock().await = paused;
            Ok(())
        }
        async fn stop(&self, _socket: &Path) -> Result<PathBuf, CoordinatorError> {
            if *self.fail_stop.lock().await {
                return Err(CoordinatorError::Failed("ipc timeout".to_owned()));
            }
            *self.stopped.lock().await = true;
            Ok(PathBuf::from("/tmp/out.mp4"))
        }
    }

    #[derive(Default)]
    struct FakeStore {
        record: Mutex<Option<SessionRecord>>,
        fail_next: Mutex<bool>,
    }

    impl SessionStore for FakeStore {
        async fn create(&self, record: SessionRecord) -> Result<(), CoordinatorError> {
            if *self.fail_next.lock().await {
                return Err(CoordinatorError::Store("injected".to_owned()));
            }
            *self.record.lock().await = Some(record);
            Ok(())
        }
        async fn load_active(&self) -> Result<Option<SessionRecord>, CoordinatorError> {
            Ok(self
                .record
                .lock()
                .await
                .clone()
                .filter(|record| record.phase.is_active()))
        }
        async fn save(&self, record: SessionRecord) -> Result<(), CoordinatorError> {
            if *self.fail_next.lock().await {
                return Err(CoordinatorError::Store("injected".to_owned()));
            }
            *self.record.lock().await = Some(record);
            Ok(())
        }
        async fn load_nonterminal(&self) -> Result<Vec<SessionRecord>, CoordinatorError> {
            Ok(self
                .record
                .lock()
                .await
                .clone()
                .filter(|record| record.phase.is_active())
                .into_iter()
                .collect())
        }
    }

    #[derive(Default)]
    struct FakeTimestamps {
        files: std::sync::Mutex<HashMap<PathBuf, String>>,
    }

    impl TimestampSource for FakeTimestamps {
        fn read(&self, path: &Path) -> Result<Option<String>, CoordinatorError> {
            Ok(self
                .files
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(path)
                .cloned())
        }
    }

    struct FakeClock {
        unix_ms: AtomicU64,
        session: SessionId,
    }

    impl CoordinatorClock for FakeClock {
        fn unix_ms(&self) -> u64 {
            self.unix_ms.load(Ordering::Relaxed)
        }
        fn session_id(&self) -> SessionId {
            self.session
        }
    }

    fn evaluated() -> EvaluatedSpec {
        EvaluatedSpec {
            spec: RecordingSpec {
                target: CaptureTarget::Monitor {
                    name: "DP-1".to_owned(),
                },
                output: PathBuf::from("/tmp/out.mp4"),
                fps: 60,
                frame_mode: FrameMode::Constant,
                codec: Codec::H264,
                container: Container::Mp4,
                fallback_cpu_encoding: true,
                audio_tracks: vec![AudioTrack::mixed(vec![AudioSource::DefaultOutput])],
                webcam: None,
                postprocess: PostprocessMode::ValidateOnly,
                overwrite: false,
                exclude_metadata: true,
            },
            backend: CaptureBackend::Direct,
            codec: Codec::H264,
            capture_device: None,
            encoding_device: None,
            capability_generation: 1,
            topology_generation: 1,
            warnings: Vec::new(),
            rationale: Vec::new(),
        }
    }

    type TestCoordinator =
        Coordinator<FakeSupervisor, FakeRecorder, FakeStore, FakeClock, FakeTimestamps>;

    fn coordinator() -> (TestCoordinator, SessionId) {
        let session = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        (
            Coordinator::with_timestamps(
                FakeSupervisor::default(),
                FakeRecorder {
                    status: Mutex::new(RecorderStatus::Running),
                    ..FakeRecorder::default()
                },
                FakeStore::default(),
                FakeClock {
                    unix_ms: AtomicU64::new(10),
                    session,
                },
                FakeTimestamps::default(),
            )
            .with_startup_timeout(Duration::from_millis(200)),
            session,
        )
    }

    async fn admitted() -> (TestCoordinator, SessionId) {
        let (coordinator, session) = coordinator();
        let accepted = coordinator
            .admit(Admission {
                session_id: session,
                evaluated: evaluated(),
                runtime_directory: PathBuf::from("/run/user/1000/omarec/sessions/test"),
                gsr_ipc_socket: PathBuf::from("/run/user/1000/omarec/sessions/test/gsr.sock"),
                first_frame_timestamp: PathBuf::from("/tmp/.omarec.part.mp4.ts"),
                staging_output: PathBuf::from("/tmp/.omarec.part.mp4"),
                unit_name: format!("omarec-session-{session}.service"),
            })
            .await
            .unwrap();
        assert_eq!(accepted, session);
        (coordinator, session)
    }

    fn command() -> PlannedCommand {
        PlannedCommand {
            program: PathBuf::from("gpu-screen-recorder"),
            arguments: vec!["-w".to_owned(), "DP-1".to_owned()],
            environment: Vec::new(),
        }
    }

    fn ready(coordinator: &TestCoordinator) {
        coordinator.timestamps.files.lock().unwrap().insert(
            PathBuf::from("/tmp/.omarec.part.mp4.ts"),
            "1000 2000\n".to_owned(),
        );
    }

    #[tokio::test]
    async fn accepted_happens_after_durable_preparing() {
        let (coordinator, _) = admitted().await;
        let snapshot = coordinator.snapshot().await;
        assert_eq!(snapshot.phase, SessionPhase::Preparing);
        let record = coordinator.store.load_active().await.unwrap().unwrap();
        assert_eq!(record.phase, SessionPhase::Preparing);
        assert_eq!(record.schema_version, 1);
    }

    #[tokio::test]
    async fn simultaneous_starts_are_serialized() {
        let (coordinator, session) = admitted().await;
        let error = coordinator
            .admit(Admission {
                session_id: session,
                evaluated: evaluated(),
                runtime_directory: PathBuf::from("/run/x"),
                gsr_ipc_socket: PathBuf::from("/run/x/gsr.sock"),
                first_frame_timestamp: PathBuf::from("/tmp/a.ts"),
                staging_output: PathBuf::from("/tmp/a.mp4"),
                unit_name: "unit".to_owned(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), "active_session");
    }

    #[tokio::test]
    async fn recording_waits_for_ipc_and_first_frame_timestamp() {
        let (coordinator, session) = admitted().await;
        ready(&coordinator);
        coordinator.launch(&command()).await.unwrap();
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Recording);
        assert_eq!(
            coordinator.snapshot().await.first_frame_monotonic_us,
            Some(1000)
        );
        coordinator.pause(session).await.unwrap();
        coordinator.pause(session).await.unwrap();
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Paused);
        coordinator.resume(session).await.unwrap();
        coordinator.resume(session).await.unwrap();
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Recording);
        coordinator.stop(session, false).await.unwrap();
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Finalizing);
    }

    #[tokio::test]
    async fn stale_session_id_does_not_pause() {
        let (coordinator, _) = admitted().await;
        ready(&coordinator);
        coordinator.launch(&command()).await.unwrap();
        let other = "00000000-0000-0000-0000-000000000099".parse().unwrap();
        let error = coordinator.pause(other).await.unwrap_err();
        assert_eq!(error.code(), "session_mismatch");
    }

    #[tokio::test]
    async fn stop_during_preparation_cancels() {
        let (coordinator, session) = admitted().await;
        coordinator.stop(session, false).await.unwrap();
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Cancelled);
    }

    #[tokio::test]
    async fn crash_before_first_frame_fails() {
        let (coordinator, _) = admitted().await;
        *coordinator.supervisor.crash_after_start.lock().await = true;
        let error = coordinator.launch(&command()).await.unwrap_err();
        assert_eq!(error.code(), "unit_failed");
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Failed);
        assert_eq!(*coordinator.supervisor.cleaned.lock().await, 1);
    }

    #[tokio::test]
    async fn daemon_shutdown_stops_readiness() {
        let (coordinator, _) = admitted().await;
        coordinator.request_shutdown();
        let error = coordinator.launch(&command()).await.unwrap_err();
        assert_eq!(error.code(), "shutdown");
    }

    #[tokio::test]
    async fn malformed_timestamp_fails_readiness() {
        let (coordinator, _) = admitted().await;
        coordinator.timestamps.files.lock().unwrap().insert(
            PathBuf::from("/tmp/.omarec.part.mp4.ts"),
            "not-a-timestamp".to_owned(),
        );
        let error = coordinator.launch(&command()).await.unwrap_err();
        assert_eq!(error.code(), "malformed_timestamp");
        assert_eq!(*coordinator.supervisor.fallback.lock().await, 1);
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Failed);
    }

    #[tokio::test]
    async fn header_only_timestamp_waits_instead_of_failing() {
        let (mut coordinator, _) = admitted().await;
        coordinator.startup_timeout = Duration::from_millis(40);
        coordinator.timestamps.files.lock().unwrap().insert(
            PathBuf::from("/tmp/.omarec.part.mp4.ts"),
            "monotonic_microsec\trealtime_microsec\n".to_owned(),
        );
        let error = coordinator.launch(&command()).await.unwrap_err();
        assert_eq!(error.code(), "startup_timeout");
        assert_eq!(*coordinator.supervisor.fallback.lock().await, 1);
    }

    #[tokio::test]
    async fn gsr6_headered_timestamp_reaches_recording() {
        let (coordinator, _) = admitted().await;
        coordinator.timestamps.files.lock().unwrap().insert(
            PathBuf::from("/tmp/.omarec.part.mp4.ts"),
            "monotonic_microsec\trealtime_microsec\n261773753358\t1786686405432508\n".to_owned(),
        );
        coordinator.launch(&command()).await.unwrap();
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Recording);
        assert_eq!(
            coordinator.snapshot().await.first_frame_monotonic_us,
            Some(261_773_753_358)
        );
    }

    #[tokio::test]
    async fn missing_socket_times_out() {
        let (mut coordinator, _) = admitted().await;
        coordinator.startup_timeout = Duration::from_millis(40);
        *coordinator.recorder.status.lock().await = RecorderStatus::NotRunning;
        *coordinator.supervisor.state.lock().await = UnitState::Active;
        let error = coordinator.launch(&command()).await.unwrap_err();
        assert_eq!(error.code(), "startup_timeout");
    }

    #[tokio::test]
    async fn ipc_timeout_uses_exact_unit_fallback_when_forced() {
        let (coordinator, session) = admitted().await;
        ready(&coordinator);
        coordinator.launch(&command()).await.unwrap();
        *coordinator.recorder.fail_stop.lock().await = true;
        coordinator.stop(session, true).await.unwrap();
        assert_eq!(*coordinator.supervisor.fallback.lock().await, 1);
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Finalizing);
    }

    #[tokio::test]
    async fn recover_cancels_unlaunched_preparing() {
        let (coordinator, session) = admitted().await;
        coordinator.recover().await.unwrap();
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Cancelled);
        assert_eq!(coordinator.snapshot().await.session_id, Some(session));
    }

    #[tokio::test]
    async fn recover_does_not_start_a_second_unit() {
        let (coordinator, _) = admitted().await;
        ready(&coordinator);
        coordinator.launch(&command()).await.unwrap();
        let started = *coordinator.supervisor.started.lock().await;
        coordinator.recover().await.unwrap();
        assert_eq!(*coordinator.supervisor.started.lock().await, started);
    }

    #[tokio::test]
    async fn snapshot_carries_evaluated_target_and_audio() {
        let (coordinator, _) = admitted().await;
        let snapshot = coordinator.snapshot().await;
        assert_eq!(snapshot.target_summary.as_deref(), Some("monitor DP-1"));
        assert_eq!(snapshot.profile.as_deref(), Some("h264/mp4"));
        assert!(snapshot.desktop_audio);
        assert!(!snapshot.microphone);
    }

    #[tokio::test]
    async fn store_create_failure_leaves_idle() {
        let (coordinator, session) = coordinator();
        *coordinator.store.fail_next.lock().await = true;
        let error = coordinator
            .admit(Admission {
                session_id: session,
                evaluated: evaluated(),
                runtime_directory: PathBuf::from("/run/x"),
                gsr_ipc_socket: PathBuf::from("/run/x/gsr.sock"),
                first_frame_timestamp: PathBuf::from("/tmp/a.ts"),
                staging_output: PathBuf::from("/tmp/a.mp4"),
                unit_name: "unit".to_owned(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), "store_failed");
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Idle);
    }

    #[tokio::test]
    async fn persist_failure_keeps_preparing() {
        let (coordinator, _) = admitted().await;
        *coordinator.store.fail_next.lock().await = true;
        let error = coordinator.launch(&command()).await.unwrap_err();
        assert_eq!(error.code(), "store_failed");
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Preparing);
    }

    #[tokio::test]
    async fn recover_promotes_staging_during_finalizing() {
        let dir = unique_dir();
        let staging = dir.join(".omarec-part.mp4");
        let final_path = dir.join("out.mp4");
        std::fs::write(&staging, b"media").unwrap();
        let (coordinator, _) = admitted().await;
        {
            let mut record = coordinator.store.record.lock().await.take().unwrap();
            record.phase = SessionPhase::Finalizing;
            record.staging_output = staging.clone();
            record.final_output = final_path.clone();
            *coordinator.store.record.lock().await = Some(record);
        }
        coordinator.recover().await.unwrap();
        assert_eq!(coordinator.snapshot().await.phase, SessionPhase::Completed);
        assert!(final_path.is_file());
        assert!(!staging.exists());
    }

    fn unique_dir() -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("omarec-coord-{nanos}"));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
}
