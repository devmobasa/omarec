use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use omarec_core::{
    CaptureTarget, EvaluatedSpec, PlannedCommand, RecordingSpec, SessionId, WebcamConfig,
};

#[derive(Clone, Debug)]
pub struct GsrCommandBuilder {
    recorder_binary: PathBuf,
    allow_extra_arguments: bool,
    trusted_plugins: Vec<PathBuf>,
}

impl GsrCommandBuilder {
    pub fn new(recorder_binary: impl Into<PathBuf>) -> Self {
        Self {
            recorder_binary: recorder_binary.into(),
            allow_extra_arguments: false,
            trusted_plugins: Vec::new(),
        }
    }

    #[must_use]
    pub fn allow_extra_arguments(mut self, value: bool) -> Self {
        self.allow_extra_arguments = value;
        self
    }

    #[must_use]
    pub fn trusted_plugins(mut self, plugins: Vec<PathBuf>) -> Self {
        self.trusted_plugins = plugins;
        self
    }

    pub fn plan(
        &self,
        session_id: SessionId,
        runtime_directory: &Path,
        evaluated: &EvaluatedSpec,
    ) -> Result<GsrPlan, PlanError> {
        let spec = &evaluated.spec;
        spec.validate().map_err(PlanError::InvalidSpec)?;

        let gsr_ipc_socket = runtime_directory.join("gsr.sock");
        let final_output = spec.output.clone();
        let staging_output = staging_path(&final_output, session_id)?;
        let first_frame_timestamp = PathBuf::from(format!("{}.ts", staging_output.display()));

        let mut arguments = Vec::new();
        arguments.push("-w".to_owned());
        arguments.push(capture_source(spec));

        if let CaptureTarget::Region { geometry, .. } = &spec.target {
            arguments.push("-region".to_owned());
            arguments.push(geometry.to_gsr_region());
        }
        if let CaptureTarget::Portal {
            restore_token_file: Some(path),
        } = &spec.target
        {
            arguments.push("-portal-session-token-filepath".to_owned());
            arguments.push(path.display().to_string());
            arguments.push("-restore-portal-session".to_owned());
            arguments.push("yes".to_owned());
        }

        arguments.extend(["-f".to_owned(), spec.fps.to_string()]);
        arguments.extend(["-fm".to_owned(), spec.frame_mode.as_gsr_value().to_owned()]);
        arguments.extend(["-k".to_owned(), evaluated.codec.as_gsr_value().to_owned()]);
        arguments.extend([
            "-fallback-cpu-encoding".to_owned(),
            yes_no(spec.fallback_cpu_encoding).to_owned(),
        ]);
        arguments.extend([
            "-exclude-metadata".to_owned(),
            yes_no(spec.exclude_metadata).to_owned(),
        ]);
        arguments.extend(["-write-first-frame-ts".to_owned(), "yes".to_owned()]);
        arguments.extend(["-ipc".to_owned(), gsr_ipc_socket.display().to_string()]);

        for track in &spec.audio_tracks {
            arguments.push("-a".to_owned());
            arguments.push(track.to_gsr_value().map_err(PlanError::InvalidSpec)?);
        }
        for plugin in &self.trusted_plugins {
            arguments.push("-p".to_owned());
            arguments.push(plugin.display().to_string());
        }
        arguments.push("-o".to_owned());
        arguments.push(staging_output.display().to_string());

        Ok(GsrPlan {
            session_id,
            command: PlannedCommand {
                program: self.recorder_binary.clone(),
                arguments,
                environment: Vec::new(),
            },
            gsr_ipc_socket,
            staging_output,
            final_output,
            first_frame_timestamp,
        })
    }
}

fn capture_source(spec: &RecordingSpec) -> String {
    let base = match &spec.target {
        CaptureTarget::Monitor { name } => name.clone(),
        CaptureTarget::Region { .. } => "region".to_owned(),
        CaptureTarget::Portal { .. } => "portal".to_owned(),
    };
    match &spec.webcam {
        Some(webcam) => format!("{base}|{}", webcam_source(webcam)),
        None => base,
    }
}

fn webcam_source(webcam: &WebcamConfig) -> String {
    let mut options = vec![
        format!("halign={}", webcam.horizontal_align.as_gsr_value()),
        format!("valign={}", webcam.vertical_align.as_gsr_value()),
        format!("hflip={}", webcam.horizontal_flip),
        format!("vflip={}", webcam.vertical_flip),
        format!("width={}%", webcam.width_percent),
        format!("height={}%", webcam.height_percent),
    ];
    if let Some(value) = webcam.camera_width {
        options.push(format!("camera_width={value}"));
    }
    if let Some(value) = webcam.camera_height {
        options.push(format!("camera_height={value}"));
    }
    if let Some(value) = webcam.camera_fps {
        options.push(format!("camera_fps={value}"));
    }
    format!("{};{}", webcam.device.display(), options.join(";"))
}

fn staging_path(final_output: &Path, session_id: SessionId) -> Result<PathBuf, PlanError> {
    let parent = final_output.parent().unwrap_or_else(|| Path::new("."));
    let extension = final_output
        .extension()
        .and_then(OsStr::to_str)
        .ok_or_else(|| PlanError::MissingOutputExtension(final_output.to_path_buf()))?;
    Ok(parent.join(format!(".omarec-{session_id}.part.{extension}")))
}

const fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[derive(Clone, Debug)]
pub struct GsrPlan {
    pub session_id: SessionId,
    pub command: PlannedCommand,
    pub gsr_ipc_socket: PathBuf,
    pub staging_output: PathBuf,
    pub final_output: PathBuf,
    pub first_frame_timestamp: PathBuf,
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("recording request is invalid: {0}")]
    InvalidSpec(#[source] omarec_core::domain::SpecValidationError),
    #[error("output path {0} needs a container extension such as .mp4 or .mkv")]
    MissingOutputExtension(PathBuf),
}

/// Human-readable only. Never feed this string back to a shell.
pub fn shell_join(command: &PlannedCommand) -> String {
    std::iter::once(command.program.display().to_string())
        .chain(command.arguments.iter().cloned())
        .map(|argument| shell_escape(&argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_escape(value: &str) -> String {
    if !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_./:@%+=,-".contains(&byte))
    {
        value.to_owned()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use omarec_core::{
        AudioSource, AudioTrack, CaptureBackend, CaptureTarget, Codec, Container, EvaluatedSpec,
        FrameMode, PostprocessMode, RecordingSpec,
    };

    use super::*;

    fn evaluated(spec: RecordingSpec, codec: Codec) -> EvaluatedSpec {
        EvaluatedSpec {
            spec,
            backend: CaptureBackend::Direct,
            codec,
            capture_device: Some("nvidia".to_owned()),
            encoding_device: Some("nvidia".to_owned()),
            capability_generation: 1,
            topology_generation: 1,
            warnings: Vec::new(),
            rationale: Vec::new(),
        }
    }

    #[test]
    fn plan_uses_per_session_ipc_and_staging_file() {
        let session_id = SessionId::new();
        let spec = RecordingSpec {
            target: CaptureTarget::Monitor {
                name: "DP-1".to_owned(),
            },
            output: PathBuf::from("/home/test/Videos/capture.mp4"),
            fps: 60,
            frame_mode: FrameMode::Constant,
            codec: Codec::Auto,
            container: Container::Mp4,
            fallback_cpu_encoding: true,
            audio_tracks: vec![AudioTrack::mixed(vec![AudioSource::DefaultOutput])],
            webcam: None,
            postprocess: PostprocessMode::ValidateOnly,
            overwrite: false,
            exclude_metadata: true,
        };
        let plan = GsrCommandBuilder::new("gpu-screen-recorder")
            .plan(
                session_id,
                Path::new("/run/user/1000/omarec/sessions/test"),
                &evaluated(spec, Codec::Hevc),
            )
            .unwrap();
        assert!(
            plan.command
                .arguments
                .windows(2)
                .any(|pair| pair == ["-ipc", "/run/user/1000/omarec/sessions/test/gsr.sock"])
        );
        assert!(
            plan.command
                .arguments
                .windows(2)
                .any(|pair| pair == ["-k", "hevc"])
        );
        assert!(plan.staging_output.to_string_lossy().ends_with(".part.mp4"));
    }

    fn pair(arguments: &[String], flag: &str) -> Option<String> {
        arguments
            .windows(2)
            .find(|window| window[0] == flag)
            .map(|window| window[1].clone())
    }

    fn base_spec(target: CaptureTarget, output: &str) -> RecordingSpec {
        RecordingSpec {
            target,
            output: PathBuf::from(output),
            fps: 60,
            frame_mode: FrameMode::Constant,
            codec: Codec::Auto,
            container: Container::Mp4,
            fallback_cpu_encoding: true,
            audio_tracks: Vec::new(),
            webcam: None,
            postprocess: PostprocessMode::ValidateOnly,
            overwrite: false,
            exclude_metadata: true,
        }
    }

    fn plan_args(spec: RecordingSpec, codec: Codec) -> Vec<String> {
        let session_id = "00000000-0000-0000-0000-000000000001"
            .parse::<SessionId>()
            .unwrap();
        GsrCommandBuilder::new("gpu-screen-recorder")
            .plan(
                session_id,
                Path::new("/run/user/1000/omarec/sessions/test"),
                &evaluated(spec, codec),
            )
            .unwrap()
            .command
            .arguments
    }

    #[test]
    fn golden_monitor_plan_uses_argv_vector() {
        let arguments = plan_args(
            base_spec(
                CaptureTarget::Monitor {
                    name: "DP-1".to_owned(),
                },
                "/home/alice/Videos/Screenrecordings/demo.mp4",
            ),
            Codec::Hevc,
        );
        assert_eq!(pair(&arguments, "-w").as_deref(), Some("DP-1"));
        assert_eq!(pair(&arguments, "-k").as_deref(), Some("hevc"));
        assert_eq!(pair(&arguments, "-f").as_deref(), Some("60"));
        assert_eq!(pair(&arguments, "-fm").as_deref(), Some("cfr"));
        assert_eq!(
            pair(&arguments, "-fallback-cpu-encoding").as_deref(),
            Some("yes")
        );
        assert_eq!(
            pair(&arguments, "-ipc").as_deref(),
            Some("/run/user/1000/omarec/sessions/test/gsr.sock")
        );
        assert!(!arguments.iter().any(|argument| argument.contains(' ')));
    }

    #[test]
    fn golden_region_keeps_negative_logical_coordinates() {
        use omarec_core::{CoordinateSpace, Geometry, RegionEvidence};
        let arguments = plan_args(
            base_spec(
                CaptureTarget::Region {
                    geometry: Geometry {
                        x: -1920,
                        y: 0,
                        width: 1920,
                        height: 1080,
                    },
                    coordinate_space: CoordinateSpace::Logical,
                    evidence: RegionEvidence {
                        monitor: Some("HDMI-A-1".to_owned()),
                        ..RegionEvidence::default()
                    },
                },
                "/tmp/demo.mp4",
            ),
            Codec::H264,
        );
        assert_eq!(pair(&arguments, "-w").as_deref(), Some("region"));
        assert_eq!(
            pair(&arguments, "-region").as_deref(),
            Some("1920x1080+-1920+0")
        );
    }

    #[test]
    fn golden_mixed_desktop_and_microphone_is_one_track() {
        let mut spec = base_spec(
            CaptureTarget::Monitor {
                name: "DP-1".to_owned(),
            },
            "/tmp/demo.mp4",
        );
        spec.audio_tracks = vec![AudioTrack::mixed(vec![
            AudioSource::DefaultOutput,
            AudioSource::DefaultInput,
        ])];
        let arguments = plan_args(spec, Codec::H264);
        assert_eq!(
            pair(&arguments, "-a").as_deref(),
            Some("default_output|default_input")
        );
        assert_eq!(
            arguments
                .iter()
                .filter(|argument| *argument == "-a")
                .count(),
            1
        );
    }

    #[test]
    fn golden_portal_and_dash_prefixed_and_unicode_paths() {
        let session_id = "00000000-0000-0000-0000-000000000001"
            .parse::<SessionId>()
            .unwrap();
        let portal = GsrCommandBuilder::new("gpu-screen-recorder")
            .plan(
                session_id,
                Path::new("/run/user/1000/omarec/sessions/test"),
                &evaluated(
                    base_spec(
                        CaptureTarget::Portal {
                            restore_token_file: Some(PathBuf::from("/tmp/portal-token")),
                        },
                        "/tmp/демо.mp4",
                    ),
                    Codec::H264,
                ),
            )
            .unwrap();
        assert_eq!(
            pair(&portal.command.arguments, "-w").as_deref(),
            Some("portal")
        );
        assert_eq!(
            pair(&portal.command.arguments, "-portal-session-token-filepath").as_deref(),
            Some("/tmp/portal-token")
        );
        assert!(portal.final_output.to_string_lossy().contains("демо"));
        assert_eq!(
            pair(&portal.command.arguments, "-o").as_deref(),
            Some(portal.staging_output.to_str().unwrap())
        );

        let dash = plan_args(
            base_spec(
                CaptureTarget::Monitor {
                    name: "-DP-1".to_owned(),
                },
                "/tmp/demo.mp4",
            ),
            Codec::H264,
        );
        assert_eq!(pair(&dash, "-w").as_deref(), Some("-DP-1"));
        assert!(!dash.contains(&"--DP-1".to_owned()));
    }

    #[test]
    fn golden_webcam_native_composition_and_hdr_portal_rejection() {
        use omarec_core::{HorizontalAlign, VerticalAlign, WebcamConfig};
        let mut spec = base_spec(
            CaptureTarget::Monitor {
                name: "DP-1".to_owned(),
            },
            "/tmp/demo.mp4",
        );
        spec.webcam = Some(WebcamConfig {
            device: PathBuf::from("/dev/video0"),
            width_percent: 25,
            height_percent: 25,
            horizontal_align: HorizontalAlign::End,
            vertical_align: VerticalAlign::End,
            horizontal_flip: true,
            vertical_flip: false,
            camera_width: None,
            camera_height: None,
            camera_fps: None,
        });
        let arguments = plan_args(spec, Codec::H264);
        assert!(
            pair(&arguments, "-w")
                .unwrap()
                .starts_with("DP-1|/dev/video0;")
        );
        assert!(pair(&arguments, "-w").unwrap().contains("width=25%"));

        let hdr = base_spec(
            CaptureTarget::Portal {
                restore_token_file: None,
            },
            "/tmp/demo.mp4",
        );
        let mut hdr = evaluated(hdr, Codec::HevcHdr);
        hdr.spec.codec = Codec::HevcHdr;
        let error = GsrCommandBuilder::new("gpu-screen-recorder")
            .plan(
                "00000000-0000-0000-0000-000000000001"
                    .parse::<SessionId>()
                    .unwrap(),
                Path::new("/tmp"),
                &hdr,
            )
            .unwrap_err();
        assert!(matches!(
            error,
            PlanError::InvalidSpec(omarec_core::SpecValidationError::HdrPortalUnsupported)
        ));
    }
}
