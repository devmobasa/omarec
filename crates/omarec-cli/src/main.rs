use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use omarec_core::{
    AppPaths, AudioSource, AudioTrack, CaptureTarget, Codec, Container, CoordinateSpace, FrameMode,
    Geometry, HorizontalAlign, PostprocessMode, RecordingRequest, RegionEvidence, RequestOverrides,
    SessionId, VerticalAlign, WebcamConfig,
};
use omarec_protocol::{
    Event, EventEnvelope, JsonLineConnection, PROTOCOL_VERSION, Request, RequestEnvelope, Response,
    ResponseEnvelope,
};
use tokio::process::Command as ProcessCommand;
use tokio::time::sleep;

mod menu;

#[derive(Debug, Parser)]
#[command(
    name = "omarec",
    version,
    about = "Reliable Omarchy screen recording controller"
)]
struct Cli {
    /// Override `$XDG_RUNTIME_DIR/omarec/control.sock`.
    #[arg(long, env = "OMAREC_SOCKET", global = true)]
    socket: Option<PathBuf>,

    /// Emit JSON; `watch --json` emits one event per line.
    #[arg(long, global = true)]
    json: bool,

    /// Do not ask systemd to start omarecd when the socket is unavailable.
    #[arg(long, global = true)]
    no_autostart: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Show the daemon and current session state.
    Status,
    /// Probe GSR, monitors, codecs, audio devices, and cameras.
    Capabilities {
        #[arg(long)]
        refresh: bool,
    },
    /// Produce the exact recorder and systemd commands without executing them.
    Plan(RecordingArguments),
    /// Start a recording. Returns after durable admission; first frame arrives through watch/status.
    Start(RecordingArguments),
    /// Stop the exact active session. Completion arrives through watch/status; `--wait` stays on that session.
    Stop {
        /// Session that must still be active. Defaults to the daemon's current session.
        #[arg(long = "session")]
        expected_session_id: Option<SessionId>,
        /// Allow the supervisor fallback when GSR IPC is unresponsive.
        #[arg(long)]
        force: bool,
        /// Wait until this session becomes terminal.
        #[arg(long)]
        wait: bool,
    },
    Pause {
        #[arg(long = "session")]
        expected_session_id: Option<SessionId>,
        /// Resume instead when the session is already paused.
        #[arg(long)]
        toggle: bool,
    },
    Resume {
        #[arg(long = "session")]
        expected_session_id: Option<SessionId>,
    },
    /// Stream state changes and heartbeats.
    Watch,
    /// Open a small recording menu. Same start/pause/stop intents as the bar dropdown.
    Menu {
        /// Print the menu labels and exit; do not launch a picker.
        #[arg(long)]
        print: bool,
        /// Horizontal icon pill for a keybind. Idle: region, fullscreen, webcam.
        /// Recording: pause and stop. Trim and folder stay on the labeled card.
        #[arg(long)]
        compact: bool,
    },
    /// Reload daemon configuration without mutating an active evaluated spec.
    Reload,
    /// Run installation, driver, backend, and session checks.
    Doctor {
        /// Write a redacted support bundle directory.
        #[arg(long)]
        bundle: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
#[command(group(
    ArgGroup::new("target")
        .required(true)
        .multiple(false)
        .args(["monitor", "region", "portal", "focused_monitor"])
))]
struct RecordingArguments {
    /// Record a concrete output connector such as DP-1.
    #[arg(long, group = "target")]
    monitor: Option<String>,

    /// Record compositor-logical geometry in WIDTHxHEIGHT+X+Y form unless `--coordinate-space` says otherwise.
    #[arg(long, group = "target", value_parser = parse_geometry)]
    region: Option<Geometry>,

    /// Use xdg-desktop-portal selection/capture.
    #[arg(long, group = "target")]
    portal: bool,

    /// Resolve the focused monitor in the Omarchy compatibility adapter, not the daemon protocol.
    #[arg(long, group = "target")]
    focused_monitor: bool,

    /// Coordinate space for `--region`. Preserve the selector's space; do not relabel logical as physical.
    #[arg(long, value_enum, default_value_t = CoordinateSpaceArgument::Logical)]
    coordinate_space: CoordinateSpaceArgument,

    /// GSR portal restore-token file. Only meaningful with --portal.
    #[arg(long)]
    portal_restore_token: Option<PathBuf>,

    #[arg(long)]
    output: PathBuf,

    #[arg(long)]
    profile: Option<String>,

    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u16).range(1..=1000))]
    fps: u16,

    #[arg(long, value_enum, default_value_t = CodecArgument::Auto)]
    codec: CodecArgument,

    #[arg(long, value_enum, default_value_t = FrameModeArgument::Constant)]
    frame_mode: FrameModeArgument,

    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    fallback_cpu_encoding: bool,

    #[arg(long)]
    desktop_audio: bool,

    #[arg(long)]
    microphone: bool,

    /// Additional source: device:NAME, app:NAME, or app-inverse:NAME.
    #[arg(long = "audio", value_parser = parse_audio_source)]
    audio: Vec<AudioSource>,

    /// Native V4L2 webcam source, for example /dev/video0.
    #[arg(long)]
    webcam: Option<PathBuf>,

    #[arg(long, default_value_t = 25, value_parser = clap::value_parser!(u8).range(1..=100))]
    webcam_size_percent: u8,

    #[arg(long, value_enum, default_value_t = WebcamPosition::BottomRight)]
    webcam_position: WebcamPosition,

    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    webcam_mirror: bool,

    #[arg(long)]
    webcam_width: Option<u32>,

    #[arg(long)]
    webcam_height: Option<u32>,

    #[arg(long)]
    webcam_fps: Option<u32>,

    #[arg(long, value_enum, default_value_t = PostprocessArgument::ValidateOnly)]
    postprocess: PostprocessArgument,

    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    exclude_metadata: bool,
}

impl RecordingArguments {
    fn into_request(self) -> Result<RecordingRequest, CliError> {
        if self.portal_restore_token.is_some() && !self.portal {
            return Err(CliError::InvalidArguments(
                "--portal-restore-token requires --portal".to_owned(),
            ));
        }
        let target = if let Some(name) = self.monitor {
            CaptureTarget::Monitor { name }
        } else if let Some(geometry) = self.region {
            CaptureTarget::Region {
                geometry,
                coordinate_space: self.coordinate_space.into(),
                evidence: RegionEvidence::default(),
            }
        } else if self.portal {
            CaptureTarget::Portal {
                restore_token_file: self.portal_restore_token,
            }
        } else if self.focused_monitor {
            return Err(CliError::InvalidArguments(
                "focused monitor is a CLI/Omarchy adapter convenience; pass --monitor NAME"
                    .to_owned(),
            ));
        } else {
            return Err(CliError::InvalidArguments(
                "a capture target is required".to_owned(),
            ));
        };

        let mut sources = Vec::new();
        if self.desktop_audio {
            sources.push(AudioSource::DefaultOutput);
        }
        if self.microphone {
            sources.push(AudioSource::DefaultInput);
        }
        sources.extend(self.audio);
        let audio_tracks = if sources.is_empty() {
            Vec::new()
        } else {
            vec![AudioTrack::mixed(sources)]
        };

        let webcam = self.webcam.map(|device| {
            let (horizontal_align, vertical_align) = self.webcam_position.alignments();
            WebcamConfig {
                device,
                width_percent: self.webcam_size_percent,
                height_percent: self.webcam_size_percent,
                horizontal_align,
                vertical_align,
                horizontal_flip: self.webcam_mirror,
                vertical_flip: false,
                camera_width: self.webcam_width,
                camera_height: self.webcam_height,
                camera_fps: self.webcam_fps,
            }
        });

        let output = self.output;
        let container = Container::from_extension(
            output
                .extension()
                .and_then(|value| value.to_str())
                .unwrap_or("mp4"),
        )
        .unwrap_or(Container::Mp4);

        Ok(RecordingRequest {
            target,
            profile: self.profile,
            output: Some(output),
            overrides: RequestOverrides {
                fps: Some(self.fps),
                frame_mode: Some(self.frame_mode.into()),
                codec: Some(self.codec.into()),
                container: Some(container),
                fallback_cpu_encoding: Some(self.fallback_cpu_encoding),
                audio_tracks: if audio_tracks.is_empty() {
                    None
                } else {
                    Some(audio_tracks)
                },
                webcam,
                postprocess: Some(self.postprocess.into()),
                overwrite: None,
                exclude_metadata: Some(self.exclude_metadata),
            },
        })
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CodecArgument {
    #[default]
    Auto,
    H264,
    Hevc,
    HevcHdr,
    Av1,
    Av1Hdr,
    Vp8,
    Vp9,
}

impl From<CodecArgument> for Codec {
    fn from(value: CodecArgument) -> Self {
        match value {
            CodecArgument::Auto => Self::Auto,
            CodecArgument::H264 => Self::H264,
            CodecArgument::Hevc => Self::Hevc,
            CodecArgument::HevcHdr => Self::HevcHdr,
            CodecArgument::Av1 => Self::Av1,
            CodecArgument::Av1Hdr => Self::Av1Hdr,
            CodecArgument::Vp8 => Self::Vp8,
            CodecArgument::Vp9 => Self::Vp9,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum FrameModeArgument {
    #[default]
    Constant,
    Variable,
    Content,
}

impl From<FrameModeArgument> for FrameMode {
    fn from(value: FrameModeArgument) -> Self {
        match value {
            FrameModeArgument::Constant => Self::Constant,
            FrameModeArgument::Variable => Self::Variable,
            FrameModeArgument::Content => Self::Content,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum PostprocessArgument {
    #[default]
    ValidateOnly,
    OmarchyCompat,
}

impl From<PostprocessArgument> for PostprocessMode {
    fn from(value: PostprocessArgument) -> Self {
        match value {
            PostprocessArgument::ValidateOnly => Self::ValidateOnly,
            PostprocessArgument::OmarchyCompat => Self::OmarchyCompat,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum CoordinateSpaceArgument {
    #[default]
    Logical,
    PhysicalPixels,
}

impl From<CoordinateSpaceArgument> for CoordinateSpace {
    fn from(value: CoordinateSpaceArgument) -> Self {
        match value {
            CoordinateSpaceArgument::Logical => Self::Logical,
            CoordinateSpaceArgument::PhysicalPixels => Self::PhysicalPixels,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum WebcamPosition {
    TopLeft,
    TopRight,
    BottomLeft,
    #[default]
    BottomRight,
    Center,
}

impl WebcamPosition {
    const fn alignments(self) -> (HorizontalAlign, VerticalAlign) {
        match self {
            Self::TopLeft => (HorizontalAlign::Start, VerticalAlign::Start),
            Self::TopRight => (HorizontalAlign::End, VerticalAlign::Start),
            Self::BottomLeft => (HorizontalAlign::Start, VerticalAlign::End),
            Self::BottomRight => (HorizontalAlign::End, VerticalAlign::End),
            Self::Center => (HorizontalAlign::Center, VerticalAlign::Center),
        }
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run(Cli::parse()).await {
        eprintln!("omarec: {error}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let paths = AppPaths::discover()?;
    let custom_socket = cli.socket.is_some();
    let socket = cli.socket.unwrap_or(paths.control_socket);

    let autostart = !cli.no_autostart && !custom_socket;
    if matches!(&cli.command, Commands::Watch) {
        return watch(&socket, autostart, cli.json).await;
    }
    if let Commands::Menu { print, compact } = &cli.command {
        return menu::run_menu(&socket, autostart, *print, *compact).await;
    }

    let request = match cli.command {
        Commands::Status => Request::Status,
        Commands::Capabilities { refresh } => Request::Capabilities { refresh },
        Commands::Plan(arguments) => Request::Plan {
            request: arguments.into_request()?,
        },
        Commands::Start(arguments) => Request::Start {
            request: arguments.into_request()?,
        },
        Commands::Stop {
            expected_session_id,
            force,
            wait,
        } => {
            let expected_session_id = match expected_session_id {
                Some(id) => id,
                None => current_session_id(&socket, autostart).await?,
            };
            let response = request_once(
                &socket,
                autostart,
                Request::Stop {
                    expected_session_id,
                    force,
                },
            )
            .await?;
            print_response(&response, cli.json)?;
            if wait {
                wait_until_terminal(&socket, autostart, expected_session_id, cli.json).await?;
            }
            return Ok(());
        }
        Commands::Pause {
            expected_session_id,
            toggle,
        } => {
            if toggle {
                let snapshot = current_snapshot(&socket, autostart).await?;
                let expected_session_id = expected_session_id.or(snapshot.session_id).ok_or_else(
                    || {
                        CliError::InvalidArguments(
                            "there is no active session; pass --session when controlling a recording"
                                .to_owned(),
                        )
                    },
                )?;
                if snapshot.paused {
                    Request::Resume {
                        expected_session_id,
                    }
                } else {
                    Request::Pause {
                        expected_session_id,
                    }
                }
            } else {
                Request::Pause {
                    expected_session_id: match expected_session_id {
                        Some(id) => id,
                        None => current_session_id(&socket, autostart).await?,
                    },
                }
            }
        }
        Commands::Resume {
            expected_session_id,
        } => Request::Resume {
            expected_session_id: match expected_session_id {
                Some(id) => id,
                None => current_session_id(&socket, autostart).await?,
            },
        },
        Commands::Reload => Request::Reload,
        Commands::Doctor { bundle } => {
            let response = request_once(&socket, autostart, Request::Doctor).await?;
            if let Some(directory) = bundle {
                write_doctor_bundle(&directory, &response)?;
            }
            return print_response(&response, cli.json);
        }
        Commands::Watch | Commands::Menu { .. } => unreachable!("watch/menu handled above"),
    };
    let response = request_once(&socket, autostart, request).await?;
    print_response(&response, cli.json)
}

async fn request_once(
    socket: &Path,
    autostart: bool,
    request: Request,
) -> Result<ResponseEnvelope, CliError> {
    let mut connection = connect(socket, autostart).await?;
    let request = RequestEnvelope::new(request);
    let request_id = request.request_id;
    connection.send(&request).await?;
    let response = connection
        .receive::<ResponseEnvelope>()
        .await?
        .ok_or(CliError::DaemonClosed)?;
    validate_response(request_id, &response)?;
    Ok(response)
}

async fn watch(socket: &Path, autostart: bool, json: bool) -> Result<(), CliError> {
    let mut connection = connect(socket, autostart).await?;
    let request = RequestEnvelope::new(Request::Watch);
    let request_id = request.request_id;
    connection.send(&request).await?;
    let acknowledgement = connection
        .receive::<ResponseEnvelope>()
        .await?
        .ok_or(CliError::DaemonClosed)?;
    validate_response(request_id, &acknowledgement)?;
    ensure_success(&acknowledgement.response)?;

    while let Some(event) = connection.receive::<EventEnvelope>().await? {
        if event.protocol != PROTOCOL_VERSION {
            return Err(CliError::ProtocolMismatch {
                expected: PROTOCOL_VERSION,
                actual: event.protocol,
            });
        }
        if json {
            println!("{}", serde_json::to_string(&event)?);
        } else {
            print_event(&event);
        }
    }
    Err(CliError::DaemonClosed)
}

async fn wait_until_terminal(
    socket: &Path,
    autostart: bool,
    expected: SessionId,
    json: bool,
) -> Result<(), CliError> {
    let mut connection = connect(socket, autostart).await?;
    let request = RequestEnvelope::new(Request::Watch);
    let request_id = request.request_id;
    connection.send(&request).await?;
    let acknowledgement = connection
        .receive::<ResponseEnvelope>()
        .await?
        .ok_or(CliError::DaemonClosed)?;
    validate_response(request_id, &acknowledgement)?;
    ensure_success(&acknowledgement.response)?;

    while let Some(event) = connection.receive::<EventEnvelope>().await? {
        if event.protocol != PROTOCOL_VERSION {
            return Err(CliError::ProtocolMismatch {
                expected: PROTOCOL_VERSION,
                actual: event.protocol,
            });
        }
        if json {
            println!("{}", serde_json::to_string(&event)?);
        } else {
            print_event(&event);
        }
        if session_reached_terminal(&event, expected) {
            return Ok(());
        }
    }
    Err(CliError::DaemonClosed)
}

fn session_reached_terminal(envelope: &EventEnvelope, expected: SessionId) -> bool {
    match &envelope.event {
        Event::Snapshot { snapshot, .. } | Event::StateChanged { snapshot } => {
            snapshot.session_id == Some(expected) && snapshot.phase.is_terminal()
        }
        Event::FileSaved { session_id, .. } => *session_id == expected,
        _ => false,
    }
}

async fn connect(socket: &Path, autostart: bool) -> Result<JsonLineConnection, CliError> {
    match JsonLineConnection::connect(socket).await {
        Ok(connection) => return Ok(connection),
        Err(first_error) if !autostart => return Err(CliError::Transport(first_error)),
        Err(_) => {}
    }

    let status = ProcessCommand::new("systemctl")
        .args(["--user", "start", "omarec.service"])
        .status()
        .await
        .map_err(CliError::Autostart)?;
    if !status.success() {
        return Err(CliError::AutostartRejected(status.code()));
    }
    for _ in 0..40 {
        match JsonLineConnection::connect(socket).await {
            Ok(connection) => return Ok(connection),
            Err(_) => sleep(Duration::from_millis(50)).await,
        }
    }
    Err(CliError::AutostartTimeout(socket.to_path_buf()))
}

async fn current_snapshot(
    socket: &Path,
    autostart: bool,
) -> Result<omarec_core::SessionSnapshot, CliError> {
    let response = request_once(socket, autostart, Request::Status).await?;
    match response.response {
        Response::Status { snapshot, .. } => Ok(snapshot),
        Response::Error {
            code,
            message,
            retryable,
            ..
        } => Err(CliError::Daemon {
            code,
            message,
            retryable,
        }),
        _ => Err(CliError::InvalidArguments(
            "status response did not include a session".to_owned(),
        )),
    }
}

async fn current_session_id(socket: &Path, autostart: bool) -> Result<SessionId, CliError> {
    current_snapshot(socket, autostart)
        .await?
        .session_id
        .ok_or_else(|| {
            CliError::InvalidArguments(
                "there is no active session; pass --session when controlling a recording"
                    .to_owned(),
            )
        })
}

fn write_doctor_bundle(directory: &Path, response: &ResponseEnvelope) -> Result<(), CliError> {
    std::fs::create_dir_all(directory).map_err(CliError::Bundle)?;
    let home = std::env::var("HOME").unwrap_or_default();
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_default();
    let encoded = serde_json::to_string_pretty(response)?;
    let redacted = omarec_core::redact_text(&encoded, &home, &runtime);
    if omarec_core::contains_sensitive(&redacted, &home) {
        return Err(CliError::BundleSensitive);
    }
    std::fs::write(directory.join("doctor.json"), redacted.as_bytes()).map_err(CliError::Bundle)?;
    let manifest = serde_json::json!({
        "kind": "omarec-doctor-bundle",
        "redactions_applied": ["home", "xdg_runtime_dir"],
        "files": ["doctor.json"]
    });
    std::fs::write(
        directory.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )
    .map_err(CliError::Bundle)?;
    Ok(())
}

fn validate_response(request_id: uuid::Uuid, response: &ResponseEnvelope) -> Result<(), CliError> {
    if response.protocol != PROTOCOL_VERSION {
        return Err(CliError::ProtocolMismatch {
            expected: PROTOCOL_VERSION,
            actual: response.protocol,
        });
    }
    if response.request_id != request_id {
        return Err(CliError::MismatchedRequestId);
    }
    Ok(())
}

fn print_response(envelope: &ResponseEnvelope, json: bool) -> Result<(), CliError> {
    if json {
        println!("{}", serde_json::to_string_pretty(&envelope)?);
        return ensure_success(&envelope.response);
    }
    match &envelope.response {
        Response::Hello {
            daemon_version,
            protocol,
            features,
        } => println!(
            "omarecd {daemon_version}, protocol {protocol}; {}",
            features.join(", ")
        ),
        Response::Status { snapshot, .. } => {
            println!("state: {:?}", snapshot.phase);
            if let Some(session_id) = snapshot.session_id {
                println!("session: {session_id}");
            }
            if let Some(output) = &snapshot.output {
                println!("output: {}", output.display());
            }
            if let Some(error) = &snapshot.last_error {
                println!("last error: {error}");
            }
        }
        Response::Capabilities { capabilities } => {
            println!(
                "recorder: {}",
                capabilities
                    .recorder_version
                    .as_deref()
                    .unwrap_or("detected")
            );
            println!(
                "capture options: {}",
                capabilities.capture_options.join(", ")
            );
            println!("monitors: {}", capabilities.monitors.join(", "));
            println!("codecs: {}", capabilities.codecs.join(", "));
            println!("per-instance IPC: {}", capabilities.ipc_available);
        }
        Response::Plan { plan } => {
            println!("preview: {}", plan.preview_id);
            println!("advisory: {}", plan.advisory);
            println!("runtime: {}", plan.runtime_directory.display());
            println!("staging output: {}", plan.staging_output.display());
            println!("final output: {}", plan.final_output.display());
            println!("recorder: {}", render_command(&plan.recorder));
            println!("supervisor: {}", render_command(&plan.supervisor));
            for warning in &plan.warnings {
                println!("warning: {warning}");
            }
        }
        Response::Accepted { session_id } => println!("recording session accepted: {session_id}"),
        Response::Acknowledged => println!("acknowledged"),
        Response::Reloaded { config_generation } => {
            println!("config reloaded (generation {config_generation})");
        }
        Response::Doctor { report } => {
            for check in &report.checks {
                println!("{:?}: {} — {}", check.status, check.id, check.summary);
                if let Some(remediation) = &check.remediation {
                    println!("  fix: {remediation}");
                }
            }
        }
        Response::Error { .. } => {}
    }
    ensure_success(&envelope.response)
}

fn ensure_success(response: &Response) -> Result<(), CliError> {
    if let Response::Error {
        code,
        message,
        retryable,
        ..
    } = response
    {
        return Err(CliError::Daemon {
            code: code.clone(),
            message: message.clone(),
            retryable: *retryable,
        });
    }
    Ok(())
}

fn print_event(envelope: &EventEnvelope) {
    match &envelope.event {
        Event::Snapshot { snapshot, .. } | Event::StateChanged { snapshot } => {
            println!("#{} state={:?}", envelope.sequence, snapshot.phase);
        }
        Event::FirstFrame { session_id } => {
            println!("#{} first frame: {session_id}", envelope.sequence);
        }
        Event::FileSaved { session_id, output } => println!(
            "#{} saved {}: {}",
            envelope.sequence,
            session_id,
            output.display()
        ),
        Event::ConfigReloaded { generation } => {
            println!("#{} config generation {generation}", envelope.sequence);
        }
        Event::Lag { skipped } => {
            println!("#{} lag skipped={skipped}", envelope.sequence);
        }
        Event::Warning { message, .. } => println!("#{} warning: {message}", envelope.sequence),
        Event::Error { code, message, .. } => {
            println!("#{} error {code}: {message}", envelope.sequence);
        }
        Event::Heartbeat => {}
    }
}

fn render_command(command: &omarec_core::PlannedCommand) -> String {
    std::iter::once(command.program.display().to_string())
        .chain(command.arguments.iter().cloned())
        .map(|value| {
            if !value.is_empty()
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_./:@%+=,-".contains(&byte))
            {
                value
            } else {
                format!("'{}'", value.replace('\'', "'\\''"))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_geometry(value: &str) -> Result<Geometry, String> {
    let (size, offsets) = value
        .split_once('+')
        .ok_or_else(|| "expected WIDTHxHEIGHT+X+Y".to_owned())?;
    let (width, height) = size
        .split_once('x')
        .ok_or_else(|| "expected WIDTHxHEIGHT+X+Y".to_owned())?;
    let (x, y) = offsets
        .split_once('+')
        .ok_or_else(|| "expected WIDTHxHEIGHT+X+Y".to_owned())?;
    let geometry = Geometry {
        width: width.parse().map_err(|_| "invalid width".to_owned())?,
        height: height.parse().map_err(|_| "invalid height".to_owned())?,
        x: x.parse().map_err(|_| "invalid X offset".to_owned())?,
        y: y.parse().map_err(|_| "invalid Y offset".to_owned())?,
    };
    if geometry.is_empty() {
        return Err("width and height must be non-zero".to_owned());
    }
    Ok(geometry)
}

fn parse_audio_source(value: &str) -> Result<AudioSource, String> {
    match value {
        "default_output" => Ok(AudioSource::DefaultOutput),
        "default_input" => Ok(AudioSource::DefaultInput),
        _ if value.starts_with("device:") => Ok(AudioSource::Device {
            name: nonempty_suffix(value, "device:")?,
        }),
        _ if value.starts_with("app:") => Ok(AudioSource::Application {
            name: nonempty_suffix(value, "app:")?,
        }),
        _ if value.starts_with("app-inverse:") => Ok(AudioSource::ApplicationExcept {
            name: nonempty_suffix(value, "app-inverse:")?,
        }),
        _ => Err(
            "expected default_output, default_input, device:NAME, app:NAME, or app-inverse:NAME"
                .to_owned(),
        ),
    }
}

fn nonempty_suffix(value: &str, prefix: &str) -> Result<String, String> {
    let suffix = value.strip_prefix(prefix).unwrap_or_default();
    if suffix.is_empty() {
        Err(format!("{prefix} requires a name"))
    } else {
        Ok(suffix.to_owned())
    }
}

#[derive(Debug, thiserror::Error)]
enum CliError {
    #[error(transparent)]
    Paths(#[from] omarec_core::PathError),
    #[error(transparent)]
    Transport(#[from] omarec_protocol::TransportError),
    #[error("invalid arguments: {0}")]
    InvalidArguments(String),
    #[error("failed to start omarecd through systemd: {0}")]
    Autostart(std::io::Error),
    #[error("systemd rejected omarecd startup (exit {0:?})")]
    AutostartRejected(Option<i32>),
    #[error("omarecd did not create {0} after systemd startup")]
    AutostartTimeout(PathBuf),
    #[error("daemon closed the connection without a response")]
    DaemonClosed,
    #[error("protocol mismatch: expected {expected}, received {actual}")]
    ProtocolMismatch { expected: u16, actual: u16 },
    #[error("daemon response request id does not match the request")]
    MismatchedRequestId,
    #[error("daemon error {code}: {message} (retryable={retryable})")]
    Daemon {
        code: String,
        message: String,
        retryable: bool,
    },
    #[error("failed to write doctor bundle: {0}")]
    Bundle(std::io::Error),
    #[error("doctor bundle still contained a home path after redaction")]
    BundleSensitive,
    #[error("failed to encode JSON output: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to run {0}: {1}")]
    Command(PathBuf, std::io::Error),
    #[error("{0} exited with status {1:?}")]
    CommandFailed(PathBuf, Option<i32>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_parser_accepts_negative_offsets() {
        assert_eq!(
            parse_geometry("1920x1080+-1920+0").unwrap(),
            Geometry {
                width: 1920,
                height: 1080,
                x: -1920,
                y: 0,
            }
        );
    }

    #[test]
    fn pause_toggle_flag_parses() {
        let cli = Cli::try_parse_from(["omarec", "pause", "--toggle"]).unwrap();
        match cli.command {
            Commands::Pause { toggle, .. } => assert!(toggle),
            other => panic!("expected pause, parsed {other:?}"),
        }
    }

    #[test]
    fn menu_print_flag_parses() {
        let cli = Cli::try_parse_from(["omarec", "menu", "--print"]).unwrap();
        match cli.command {
            Commands::Menu { print, compact } => {
                assert!(print);
                assert!(!compact);
            }
            other => panic!("expected menu, parsed {other:?}"),
        }
    }

    #[test]
    fn menu_compact_flag_parses() {
        let cli = Cli::try_parse_from(["omarec", "menu", "--compact"]).unwrap();
        match cli.command {
            Commands::Menu { compact, print } => {
                assert!(compact);
                assert!(!print);
            }
            other => panic!("expected menu, parsed {other:?}"),
        }
    }

    #[test]
    fn audio_parser_adds_required_prefix_semantics() {
        assert_eq!(
            parse_audio_source("app:firefox").unwrap(),
            AudioSource::Application {
                name: "firefox".to_owned()
            }
        );
    }

    #[test]
    fn doctor_bundle_redacts_home_paths() {
        let directory = std::env::temp_dir().join(format!(
            "omarec-bundle-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let home = std::env::var("HOME").unwrap_or_else(|_| "/home/alice".to_owned());
        let envelope = ResponseEnvelope::new(
            uuid::Uuid::nil(),
            Response::Doctor {
                report: omarec_protocol::DoctorReport {
                    ok: true,
                    checks: vec![omarec_protocol::DoctorCheck {
                        id: "xdg_runtime".to_owned(),
                        status: omarec_protocol::CheckStatus::Pass,
                        summary: format!("{home}/Videos/out.mp4"),
                        detail: None,
                        remediation: None,
                    }],
                    redactions_applied: vec!["home".to_owned()],
                },
            },
        );
        write_doctor_bundle(&directory, &envelope).unwrap();
        let doctor = std::fs::read_to_string(directory.join("doctor.json")).unwrap();
        assert!(!doctor.contains(&home));
        assert!(directory.join("manifest.json").is_file());
    }
}
