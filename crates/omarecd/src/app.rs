use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use omarec_backend_gsr::{GsrCli, GsrCommandBuilder, ProbeRunner};
use omarec_core::{
    AppPaths, Config, DaemonLifetimeId, EvaluatedSpec, HostFacts, LaunchPlan, PathOccupied,
    RecordingRequest, SessionId, SessionSnapshot, evaluate, reserve_explicit,
};
use omarec_protocol::{
    CheckStatus, DoctorCheck, DoctorReport, Event, EventEnvelope, RecorderCapabilities, Request,
    Response, ResponseEnvelope,
};
use omarec_supervisor::SystemdSupervisor;
use tokio::sync::{RwLock, broadcast};
use tracing::warn;

use crate::clock::SystemClock;
use crate::coordinator::{Admission, Coordinator, CoordinatorError};
use crate::store::FileSessionStore;

type LiveCoordinator = Coordinator<SystemdSupervisor, GsrCli, FileSessionStore, SystemClock>;

#[derive(Clone, Debug)]
pub struct App {
    config: Arc<RwLock<Config>>,
    config_generation: Arc<AtomicU64>,
    paths: Arc<AppPaths>,
    capabilities: Arc<RwLock<Option<RecorderCapabilities>>>,
    events: broadcast::Sender<EventEnvelope>,
    sequence: Arc<AtomicU64>,
    lifetime_id: DaemonLifetimeId,
    coordinator: Arc<LiveCoordinator>,
}

impl App {
    pub fn new(config: Config, paths: AppPaths) -> Self {
        let (events, _) = broadcast::channel(128);
        let supervisor = SystemdSupervisor::new(
            &config.binaries.systemd_run_binary,
            &config.binaries.systemctl_binary,
        )
        .with_journalctl(&config.binaries.journalctl_binary);
        let recorder = GsrCli::new(&config.binaries.recorder_cli_binary);
        let store = FileSessionStore::new(paths.sessions_state.clone());
        let coordinator = Arc::new(
            Coordinator::new(supervisor, recorder, store, SystemClock)
                .with_startup_timeout(Duration::from_millis(config.daemon.startup_timeout_ms)),
        );
        Self {
            config: Arc::new(RwLock::new(config)),
            config_generation: Arc::new(AtomicU64::new(1)),
            paths: Arc::new(paths),
            capabilities: Arc::new(RwLock::new(None)),
            events,
            sequence: Arc::new(AtomicU64::new(0)),
            lifetime_id: DaemonLifetimeId::new(),
            coordinator,
        }
    }

    pub async fn recover(&self) -> Result<(), AppError> {
        self.coordinator.recover().await?;
        let snapshot = self.snapshot().await;
        if snapshot.phase.is_active() {
            self.publish(Event::StateChanged { snapshot });
        }
        Ok(())
    }

    pub fn request_shutdown(&self) {
        self.coordinator.request_shutdown();
    }

    pub fn subscribe(&self) -> broadcast::Receiver<EventEnvelope> {
        self.events.subscribe()
    }

    /// Subscribe first so a state change between snapshot capture and subscription cannot be lost.
    pub async fn watch_setup(&self) -> (broadcast::Receiver<EventEnvelope>, EventEnvelope) {
        let subscription = self.subscribe();
        let snapshot = self.snapshot_event().await;
        (subscription, snapshot)
    }

    pub fn lag_event(&self, skipped: u64) -> EventEnvelope {
        self.event(Event::Lag { skipped })
    }

    pub async fn snapshot(&self) -> SessionSnapshot {
        self.coordinator.snapshot().await
    }

    pub async fn snapshot_event(&self) -> EventEnvelope {
        let snapshot = self.snapshot().await;
        let sequence = self.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        EventEnvelope {
            protocol: omarec_protocol::PROTOCOL_VERSION,
            sequence,
            daemon_lifetime_id: Some(self.lifetime_id),
            event: Event::Snapshot {
                snapshot,
                watermark: sequence,
            },
        }
    }

    pub fn heartbeat_event(&self) -> EventEnvelope {
        self.event(Event::Heartbeat)
    }

    pub async fn respond(&self, request_id: uuid::Uuid, request: Request) -> ResponseEnvelope {
        let response = match self.handle(request).await {
            Ok(response) => response,
            Err(error) => {
                warn!(error = %error, "request failed");
                Response::Error {
                    code: error.code().to_owned(),
                    message: error.to_string(),
                    retryable: error.retryable(),
                    details: None,
                }
            }
        };
        ResponseEnvelope::new(request_id, response)
    }

    async fn handle(&self, request: Request) -> Result<Response, AppError> {
        match request {
            Request::Hello => Ok(Response::Hello {
                daemon_version: env!("CARGO_PKG_VERSION").to_owned(),
                protocol: omarec_protocol::PROTOCOL_VERSION,
                features: vec![
                    "plan".to_owned(),
                    "capability_probe".to_owned(),
                    "watch".to_owned(),
                    "doctor".to_owned(),
                    "reload".to_owned(),
                    "start".to_owned(),
                    "pause".to_owned(),
                    "resume".to_owned(),
                    "stop".to_owned(),
                    "gsr_ipc_design".to_owned(),
                ],
            }),
            Request::Status => Ok(Response::Status {
                snapshot: self.snapshot().await,
                daemon_lifetime_id: Some(self.lifetime_id),
                config_generation: self.config_generation.load(Ordering::Relaxed),
            }),
            Request::Capabilities { refresh } => Ok(Response::Capabilities {
                capabilities: self.capabilities(refresh).await?,
            }),
            Request::Plan { request } => Ok(Response::Plan {
                plan: self.plan_request(&request).await?,
            }),
            Request::Start { request } => {
                let session_id = self.start_session(request).await?;
                Ok(Response::Accepted { session_id })
            }
            Request::Stop {
                expected_session_id,
                force,
            } => {
                let saved = self.coordinator.stop(expected_session_id, force).await?;
                if let Some(saved) = saved {
                    self.finalize_output(expected_session_id, saved).await?;
                }
                self.publish_snapshot().await;
                Ok(Response::Acknowledged)
            }
            Request::Pause {
                expected_session_id,
            } => {
                self.coordinator.pause(expected_session_id).await?;
                self.publish_snapshot().await;
                Ok(Response::Acknowledged)
            }
            Request::Resume {
                expected_session_id,
            } => {
                self.coordinator.resume(expected_session_id).await?;
                self.publish_snapshot().await;
                Ok(Response::Acknowledged)
            }
            Request::Reload => Ok(Response::Reloaded {
                config_generation: self.reload().await?,
            }),
            Request::Doctor => Ok(Response::Doctor {
                report: self.doctor().await,
            }),
            Request::Watch => Err(AppError::WatchHandledByTransport),
        }
    }

    async fn snapshot_config(&self) -> Config {
        self.config.read().await.clone()
    }

    async fn reload(&self) -> Result<u64, AppError> {
        let loaded = Config::load(&self.paths.config_file).map_err(AppError::Config)?;
        loaded.validate().map_err(AppError::Config)?;
        *self.config.write().await = loaded;
        let generation = self.config_generation.fetch_add(1, Ordering::Relaxed) + 1;
        self.publish(Event::ConfigReloaded { generation });
        Ok(generation)
    }

    async fn plan_request(&self, request: &RecordingRequest) -> Result<LaunchPlan, AppError> {
        let config = self.snapshot_config().await;
        let spec = config.resolve_request(request).map_err(AppError::Config)?;
        let capabilities = self.capabilities(false).await?;
        let host = HostFacts {
            display_server: None,
            gpu_vendor: capabilities.capture_device.clone(),
            card_path: None,
            topology_generation: capabilities.generation,
        };
        let evaluated =
            evaluate(&spec, &capabilities, &host, &config.backend).map_err(AppError::Policy)?;
        self.plan(&evaluated, &config)
    }

    async fn start_session(&self, request: RecordingRequest) -> Result<SessionId, AppError> {
        let config = self.snapshot_config().await;
        let spec = config.resolve_request(&request).map_err(AppError::Config)?;
        let capabilities = self.capabilities(false).await?;
        let host = HostFacts {
            display_server: None,
            gpu_vendor: capabilities.capture_device.clone(),
            card_path: None,
            topology_generation: capabilities.generation,
        };
        let mut evaluated =
            evaluate(&spec, &capabilities, &host, &config.backend).map_err(AppError::Policy)?;
        let session_id = self.coordinator.allocate_session_id();
        let reserved = reserve_explicit(&evaluated.spec.output, session_id, &PathOccupied)
            .map_err(AppError::Naming)?;
        evaluated.spec.output = reserved.final_output.clone();
        let runtime_directory = self.paths.session_runtime(&session_id);
        std::fs::create_dir_all(&runtime_directory).map_err(AppError::Io)?;
        let gsr = GsrCommandBuilder::new(&config.binaries.recorder_binary)
            .plan(session_id, &runtime_directory, &evaluated)
            .map_err(AppError::Plan)?;
        if let Some(parent) = gsr.staging_output.parent() {
            std::fs::create_dir_all(parent).map_err(AppError::Io)?;
            let dir = crate::output::open_dir_nofollow(parent).map_err(AppError::Output)?;
            if let Some(name) = gsr.staging_output.file_name() {
                drop(
                    crate::output::create_staging_exclusive(&dir, name)
                        .map_err(AppError::Output)?,
                );
            }
        }
        let supervisor = SystemdSupervisor::new(
            &config.binaries.systemd_run_binary,
            &config.binaries.systemctl_binary,
        )
        .plan(session_id, &gsr.command);
        self.coordinator
            .admit(Admission {
                session_id,
                evaluated,
                runtime_directory,
                gsr_ipc_socket: gsr.gsr_ipc_socket,
                first_frame_timestamp: gsr.first_frame_timestamp,
                staging_output: gsr.staging_output,
                unit_name: omarec_supervisor::SystemdSupervisor::unit_name(session_id),
            })
            .await?;
        self.publish_snapshot().await;
        let coordinator = Arc::clone(&self.coordinator);
        let app = self.clone();
        tokio::spawn(async move {
            let result = coordinator.launch(&supervisor).await;
            let snapshot = coordinator.snapshot().await;
            if snapshot.first_frame_monotonic_us.is_some()
                && let Some(session_id) = snapshot.session_id
            {
                app.publish(Event::FirstFrame { session_id });
            }
            app.publish(Event::StateChanged { snapshot });
            if let Err(error) = result {
                app.publish(Event::Error {
                    session_id: coordinator.snapshot().await.session_id,
                    code: error.code().to_owned(),
                    message: error.to_string(),
                });
            }
        });
        Ok(session_id)
    }

    async fn publish_snapshot(&self) {
        self.publish(Event::StateChanged {
            snapshot: self.snapshot().await,
        });
    }

    async fn finalize_output(
        &self,
        expected: SessionId,
        saved: std::path::PathBuf,
    ) -> Result<(), AppError> {
        let Some(record) = self.coordinator.active_record().await? else {
            return Err(AppError::Coordinator(CoordinatorError::NoActiveSession));
        };
        if record.session_id != expected {
            return Err(AppError::Coordinator(CoordinatorError::SessionMismatch {
                expected,
                actual: record.session_id,
            }));
        }
        let dest = record
            .final_output
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."));
        let mut source = record.staging_output.clone();
        let mut saved_path = saved;
        let config = self.snapshot_config().await;
        if record.evaluated.as_ref().is_some_and(|evaluated| {
            evaluated.spec.postprocess == omarec_core::PostprocessMode::OmarchyCompat
        }) {
            let compat = source.with_file_name(format!(
                "{}.compat.mp4",
                source.file_name().unwrap_or_default().to_string_lossy()
            ));
            crate::postprocess::omarchy_compat(
                &config.postprocess.ffmpeg_binary,
                &source,
                &compat,
                Duration::from_millis(config.daemon.finalization_timeout_ms),
            )
            .map_err(AppError::Postprocess)?;
            source = compat.clone();
            saved_path = compat;
        }
        crate::output::promote(
            dest,
            &source,
            &saved_path,
            &record.final_output,
            &crate::output::Ffprobe::new(&config.postprocess.ffprobe_binary),
        )
        .map_err(AppError::Output)?;
        self.coordinator.mark_completed(expected).await?;
        self.publish(Event::FileSaved {
            session_id: expected,
            output: record.final_output,
        });
        Ok(())
    }

    fn plan(&self, evaluated: &EvaluatedSpec, config: &Config) -> Result<LaunchPlan, AppError> {
        let spec = &evaluated.spec;
        let session_id = omarec_core::SessionId::new();
        let runtime_directory = self.paths.session_runtime(&session_id);
        let gsr = GsrCommandBuilder::new(&config.binaries.recorder_binary)
            .plan(session_id, &runtime_directory, evaluated)
            .map_err(AppError::Plan)?;
        let supervisor = SystemdSupervisor::new(
            &config.binaries.systemd_run_binary,
            &config.binaries.systemctl_binary,
        )
        .plan(session_id, &gsr.command);

        let mut warnings = evaluated.warnings.clone();
        if spec.webcam.is_some() {
            warnings.push(
                "the plan uses GSR native multi-source V4L2 composition; keep the current mpv overlay as a migration fallback until hardware coverage is complete"
                    .to_owned(),
            );
        }

        Ok(LaunchPlan {
            preview_id: omarec_core::PreviewId::new(),
            advisory: true,
            session_id: None,
            runtime_directory,
            gsr_ipc_socket: gsr.gsr_ipc_socket,
            first_frame_timestamp: gsr.first_frame_timestamp,
            staging_output: gsr.staging_output,
            final_output: gsr.final_output,
            recorder: gsr.command,
            supervisor,
            warnings,
            rationale: evaluated.rationale.clone(),
        })
    }

    async fn capabilities(&self, refresh: bool) -> Result<RecorderCapabilities, AppError> {
        if !refresh && let Some(capabilities) = self.capabilities.read().await.as_ref() {
            return Ok(capabilities.clone());
        }
        let config = self.snapshot_config().await;
        let capabilities = ProbeRunner::new(&config.binaries.recorder_binary)
            .with_cli_binary(config.binaries.recorder_cli_binary)
            .probe()
            .await
            .map_err(AppError::Probe)?;
        *self.capabilities.write().await = Some(capabilities.clone());
        Ok(capabilities)
    }

    async fn doctor(&self) -> DoctorReport {
        let config = self.snapshot_config().await;
        let mut checks = runtime_checks(&self.paths, &config);
        checks.extend(self.recorder_checks().await);
        let ok = checks.iter().all(|check| check.status != CheckStatus::Fail);
        DoctorReport {
            ok,
            checks,
            redactions_applied: vec!["home".to_owned(), "xdg_runtime_dir".to_owned()],
        }
    }

    async fn recorder_checks(&self) -> Vec<DoctorCheck> {
        match self.capabilities(true).await {
            Ok(capabilities) => vec![
                DoctorCheck {
                    id: "gpu_screen_recorder".to_owned(),
                    status: CheckStatus::Pass,
                    summary: capabilities.recorder_version.map_or_else(
                        || "GPU Screen Recorder probe succeeded".to_owned(),
                        |version| format!("GPU Screen Recorder {version}"),
                    ),
                    detail: Some(format!(
                        "capture options: {}",
                        capabilities.capture_options.join(", ")
                    )),
                    remediation: None,
                },
                DoctorCheck {
                    id: "gsr_ipc".to_owned(),
                    status: if capabilities.ipc_available {
                        CheckStatus::Pass
                    } else {
                        CheckStatus::Fail
                    },
                    summary: if capabilities.ipc_available {
                        "per-instance GSR IPC appears available".to_owned()
                    } else {
                        "gsr-cli/per-instance IPC was not detected".to_owned()
                    },
                    detail: None,
                    remediation: (!capabilities.ipc_available).then(|| {
                        "install the current Arch gpu-screen-recorder package (6.x target)"
                            .to_owned()
                    }),
                },
            ],
            Err(error) => vec![DoctorCheck {
                id: "gpu_screen_recorder".to_owned(),
                status: CheckStatus::Fail,
                summary: "GPU Screen Recorder probe failed".to_owned(),
                detail: Some(error.to_string()),
                remediation: Some(
                    "install gpu-screen-recorder from Arch Extra and verify `gpu-screen-recorder --info`"
                        .to_owned(),
                ),
            }],
        }
    }

    fn event(&self, event: Event) -> EventEnvelope {
        EventEnvelope {
            protocol: omarec_protocol::PROTOCOL_VERSION,
            sequence: self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
            daemon_lifetime_id: Some(self.lifetime_id),
            event,
        }
    }

    fn publish(&self, event: Event) {
        let _ = self.events.send(self.event(event));
    }
}

fn runtime_checks(paths: &omarec_core::AppPaths, config: &Config) -> Vec<DoctorCheck> {
    let ffmpeg_required = config.postprocess.default == omarec_core::PostprocessMode::OmarchyCompat;
    vec![
        DoctorCheck {
            id: "xdg_runtime".to_owned(),
            status: CheckStatus::Pass,
            summary: format!("runtime root: {}", paths.runtime_root.display()),
            detail: Some("omarec refuses an insecure /tmp control socket fallback".to_owned()),
            remediation: None,
        },
        binary_check(
            "systemd_run",
            &config.binaries.systemd_run_binary,
            true,
            "install systemd so `systemd-run --user` can own recording units",
        ),
        binary_check(
            "systemctl",
            &config.binaries.systemctl_binary,
            true,
            "install systemd so `systemctl --user` can query exact session units",
        ),
        binary_check(
            "journalctl",
            &config.binaries.journalctl_binary,
            true,
            "install systemd so doctor bundles can collect exact unit journals",
        ),
        binary_check(
            "gsr_cli",
            &config.binaries.recorder_cli_binary,
            true,
            "install gpu-screen-recorder so `gsr-cli` can pause and stop the owned instance",
        ),
        binary_check(
            "ffprobe",
            &config.postprocess.ffprobe_binary,
            true,
            "install ffmpeg so finalization can validate a video stream",
        ),
        binary_check(
            "ffmpeg",
            &config.postprocess.ffmpeg_binary,
            ffmpeg_required,
            "install ffmpeg for Omarchy-compatible post-processing",
        ),
        binary_check(
            "hyprctl",
            std::path::Path::new("hyprctl"),
            false,
            "install hyprland so monitor topology can be probed on this host",
        ),
        DoctorCheck {
            id: "telemetry".to_owned(),
            status: CheckStatus::Pass,
            summary: "omarec has no network telemetry".to_owned(),
            detail: Some(
                "doctor bundles stay on local disk; no metrics client is compiled in".to_owned(),
            ),
            remediation: None,
        },
    ]
}

fn binary_available(path: &std::path::Path) -> bool {
    if path.as_os_str().is_empty() {
        return false;
    }
    if path.is_absolute() || path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|directory| directory.join(path).is_file())
    })
}

fn binary_check(
    id: &str,
    path: &std::path::Path,
    required: bool,
    remediation: &str,
) -> DoctorCheck {
    if binary_available(path) {
        DoctorCheck {
            id: id.to_owned(),
            status: CheckStatus::Pass,
            summary: format!("{} is available", path.display()),
            detail: None,
            remediation: None,
        }
    } else {
        DoctorCheck {
            id: id.to_owned(),
            status: if required {
                CheckStatus::Fail
            } else {
                CheckStatus::Warning
            },
            summary: format!("{} was not found on PATH", path.display()),
            detail: None,
            remediation: Some(remediation.to_owned()),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("failed to plan GPU Screen Recorder: {0}")]
    Plan(#[source] omarec_backend_gsr::PlanError),
    #[error("failed to probe GPU Screen Recorder: {0}")]
    Probe(#[source] omarec_backend_gsr::ProbeError),
    #[error("invalid configuration or request: {0}")]
    Config(#[source] omarec_core::ConfigError),
    #[error("recording request is not supported: {0}")]
    Policy(#[source] omarec_core::PolicyError),
    #[error("failed to reserve output path: {0}")]
    Naming(#[source] omarec_core::NamingError),
    #[error(transparent)]
    Coordinator(#[from] CoordinatorError),
    #[error("failed to prepare session runtime: {0}")]
    Io(std::io::Error),
    #[error(transparent)]
    Output(#[from] crate::output::OutputError),
    #[error(transparent)]
    Postprocess(#[from] crate::postprocess::PostprocessError),
    #[error("watch requests are handled by the streaming transport")]
    WatchHandledByTransport,
}

impl AppError {
    const fn code(&self) -> &'static str {
        match self {
            Self::Plan(_) => "plan_failed",
            Self::Probe(_) => "probe_failed",
            Self::Config(_) => "invalid_request",
            Self::Policy(error) => error.code(),
            Self::Naming(_) => "naming_failed",
            Self::Coordinator(error) => error.code(),
            Self::Io(_) => "io_error",
            Self::Output(_) => "finalization_failed",
            Self::Postprocess(_) => "postprocess_failed",
            Self::WatchHandledByTransport => "transport_error",
        }
    }

    const fn retryable(&self) -> bool {
        matches!(self, Self::Probe(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_app() -> App {
        App::new(
            Config::default(),
            AppPaths {
                runtime_root: PathBuf::from("/tmp/omarec-test"),
                control_socket: PathBuf::from("/tmp/omarec-test/control.sock"),
                sessions_runtime: PathBuf::from("/tmp/omarec-test/sessions"),
                state_root: PathBuf::from("/tmp/omarec-test/state"),
                sessions_state: PathBuf::from("/tmp/omarec-test/state/sessions"),
                config_file: PathBuf::from("/tmp/omarec-test/config.toml"),
            },
        )
    }

    #[tokio::test]
    async fn watch_setup_subscribes_before_snapshot_watermark() {
        let app = test_app();
        let (_subscription, snapshot) = app.watch_setup().await;
        match snapshot.event {
            Event::Snapshot { watermark, .. } => assert_eq!(watermark, snapshot.sequence),
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(app.config_generation.load(Ordering::Relaxed), 1);
    }
}
