//! Stable domain types shared by the omarec daemon, CLI, and adapters.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Stable identifier for a recording attempt. `UUIDv7` keeps filesystem listings sortable.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub const fn as_uuid(self) -> Uuid {
        self.0
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::str::FromStr for SessionId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self::from_uuid(value.parse()?))
    }
}

/// Advisory identifier returned by `plan`. It is not a reserved session ID.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PreviewId(Uuid);

impl PreviewId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

impl Default for PreviewId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for PreviewId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Identifies one daemon process lifetime. Event sequences are only comparable within it.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DaemonLifetimeId(Uuid);

impl DaemonLifetimeId {
    pub fn new() -> Self {
        Self(Uuid::now_v7())
    }

    pub const fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }
}

impl Default for DaemonLifetimeId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for DaemonLifetimeId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Geometry {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Geometry {
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    pub fn to_gsr_region(self) -> String {
        format!("{}x{}+{}+{}", self.width, self.height, self.x, self.y)
    }
}

/// Coordinate space of a region request. The selector is the authority; the daemon never relabels.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinateSpace {
    Logical,
    PhysicalPixels,
}

/// Optional topology evidence captured with a region so start-time revalidation can detect staleness.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RegionEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monitor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CaptureTarget {
    /// A concrete Hyprland/GSR connector name, for example `DP-1`.
    Monitor { name: String },
    /// A rectangle tagged with the coordinate space emitted by the selector.
    Region {
        geometry: Geometry,
        coordinate_space: CoordinateSpace,
        #[serde(default)]
        evidence: RegionEvidence,
    },
    /// Let xdg-desktop-portal perform selection and capture.
    Portal {
        #[serde(default)]
        restore_token_file: Option<PathBuf>,
    },
}

impl CaptureTarget {
    pub fn summary(&self) -> String {
        match self {
            Self::Monitor { name } => format!("monitor {name}"),
            Self::Region {
                geometry,
                coordinate_space,
                ..
            } => {
                let space = match coordinate_space {
                    CoordinateSpace::Logical => "logical",
                    CoordinateSpace::PhysicalPixels => "physical",
                };
                format!("region {} ({space})", geometry.to_gsr_region())
            }
            Self::Portal { .. } => "portal".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AudioSource {
    DefaultOutput,
    DefaultInput,
    Device { name: String },
    Application { name: String },
    ApplicationExcept { name: String },
}

impl AudioSource {
    pub fn to_gsr_value(&self) -> String {
        match self {
            Self::DefaultOutput => "default_output".to_owned(),
            Self::DefaultInput => "default_input".to_owned(),
            Self::Device { name } => format!("device:{name}"),
            Self::Application { name } => format!("app:{name}"),
            Self::ApplicationExcept { name } => format!("app-inverse:{name}"),
        }
    }

    pub const fn is_desktop(&self) -> bool {
        matches!(
            self,
            Self::DefaultOutput | Self::Application { .. } | Self::ApplicationExcept { .. }
        )
    }

    pub const fn is_microphone(&self) -> bool {
        matches!(self, Self::DefaultInput | Self::Device { .. })
    }
}

/// Sources in one track are mixed with `|`. Separate tracks become separate `-a` arguments.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AudioTrack {
    pub sources: Vec<AudioSource>,
}

impl AudioTrack {
    pub fn mixed(sources: Vec<AudioSource>) -> Self {
        Self { sources }
    }

    pub fn to_gsr_value(&self) -> Result<String, SpecValidationError> {
        if self.sources.is_empty() {
            return Err(SpecValidationError::EmptyAudioTrack);
        }
        Ok(self
            .sources
            .iter()
            .map(AudioSource::to_gsr_value)
            .collect::<Vec<_>>()
            .join("|"))
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Codec {
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

impl Codec {
    pub const fn as_gsr_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::H264 => "h264",
            Self::Hevc => "hevc",
            Self::HevcHdr => "hevc_hdr",
            Self::Av1 => "av1",
            Self::Av1Hdr => "av1_hdr",
            Self::Vp8 => "vp8",
            Self::Vp9 => "vp9",
        }
    }

    pub const fn is_hdr(self) -> bool {
        matches!(self, Self::HevcHdr | Self::Av1Hdr)
    }

    pub fn from_gsr_value(value: &str) -> Option<Self> {
        match value {
            "auto" => Some(Self::Auto),
            "h264" => Some(Self::H264),
            "hevc" => Some(Self::Hevc),
            "hevc_hdr" => Some(Self::HevcHdr),
            "av1" => Some(Self::Av1),
            "av1_hdr" => Some(Self::Av1Hdr),
            "vp8" => Some(Self::Vp8),
            "vp9" => Some(Self::Vp9),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FrameMode {
    #[default]
    Constant,
    Variable,
    Content,
}

impl FrameMode {
    pub const fn as_gsr_value(self) -> &'static str {
        match self {
            Self::Constant => "cfr",
            Self::Variable => "vfr",
            Self::Content => "content",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Container {
    #[default]
    Mp4,
    Mkv,
    Webm,
}

impl Container {
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
        }
    }

    pub fn from_extension(extension: &str) -> Option<Self> {
        match extension {
            "mp4" => Some(Self::Mp4),
            "mkv" => Some(Self::Mkv),
            "webm" => Some(Self::Webm),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureBackend {
    #[default]
    Auto,
    Direct,
    Portal,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HorizontalAlign {
    Start,
    Center,
    #[default]
    End,
}

impl HorizontalAlign {
    pub const fn as_gsr_value(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Center => "center",
            Self::End => "end",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VerticalAlign {
    Start,
    Center,
    #[default]
    End,
}

impl VerticalAlign {
    pub const fn as_gsr_value(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Center => "center",
            Self::End => "end",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WebcamConfig {
    pub device: PathBuf,
    #[serde(default = "default_webcam_percent")]
    pub width_percent: u8,
    #[serde(default = "default_webcam_percent")]
    pub height_percent: u8,
    #[serde(default)]
    pub horizontal_align: HorizontalAlign,
    #[serde(default)]
    pub vertical_align: VerticalAlign,
    #[serde(default = "default_true")]
    pub horizontal_flip: bool,
    #[serde(default)]
    pub vertical_flip: bool,
    #[serde(default)]
    pub camera_width: Option<u32>,
    #[serde(default)]
    pub camera_height: Option<u32>,
    #[serde(default)]
    pub camera_fps: Option<u32>,
}

impl WebcamConfig {
    pub fn summary(&self) -> String {
        format!(
            "{} {}x{}%",
            self.device.display(),
            self.width_percent,
            self.height_percent
        )
    }
}

const fn default_webcam_percent() -> u8 {
    25
}

impl Default for WebcamConfig {
    fn default() -> Self {
        Self {
            device: PathBuf::new(),
            width_percent: default_webcam_percent(),
            height_percent: default_webcam_percent(),
            horizontal_align: HorizontalAlign::default(),
            vertical_align: VerticalAlign::default(),
            horizontal_flip: true,
            vertical_flip: false,
            camera_width: None,
            camera_height: None,
            camera_fps: None,
        }
    }
}

const fn default_true() -> bool {
    true
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PostprocessMode {
    /// Trust the file acknowledged by GSR and perform only validation/atomic promotion.
    #[default]
    ValidateOnly,
    /// Reproduce the existing Omarchy ffmpeg trim/normalization behavior during migration.
    OmarchyCompat,
}

/// Optional fields a client may send. Unset values inherit from the selected profile.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fps: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub codec: Option<Codec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_mode: Option<FrameMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container: Option<Container>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_cpu_encoding: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_tracks: Option<Vec<AudioTrack>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webcam: Option<WebcamConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub postprocess: Option<PostprocessMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overwrite: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_metadata: Option<bool>,
}

/// Protocol-facing start/plan payload. Selection is already resolved.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordingRequest {
    pub target: CaptureTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<PathBuf>,
    #[serde(default)]
    pub overrides: RequestOverrides,
}

impl RecordingRequest {
    pub fn monitor(name: impl Into<String>) -> Self {
        Self {
            target: CaptureTarget::Monitor { name: name.into() },
            profile: None,
            output: None,
            overrides: RequestOverrides::default(),
        }
    }
}

/// Concrete specification after profile/default resolution and before capability policy.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecordingSpec {
    pub target: CaptureTarget,
    pub output: PathBuf,
    pub fps: u16,
    pub frame_mode: FrameMode,
    pub codec: Codec,
    pub container: Container,
    pub fallback_cpu_encoding: bool,
    pub audio_tracks: Vec<AudioTrack>,
    pub webcam: Option<WebcamConfig>,
    pub postprocess: PostprocessMode,
    pub overwrite: bool,
    pub exclude_metadata: bool,
}

impl RecordingSpec {
    pub fn validate(&self) -> Result<(), SpecValidationError> {
        if self.fps == 0 || self.fps > 1000 {
            return Err(SpecValidationError::InvalidFps(self.fps));
        }
        if self.output.as_os_str().is_empty() {
            return Err(SpecValidationError::MissingOutput);
        }
        if self.overwrite {
            return Err(SpecValidationError::OverwriteUnsupported);
        }
        if let CaptureTarget::Monitor { name } = &self.target
            && (name.is_empty() || name.contains('\0'))
        {
            return Err(SpecValidationError::InvalidMonitorName);
        }
        if let CaptureTarget::Region { geometry, .. } = &self.target
            && geometry.is_empty()
        {
            return Err(SpecValidationError::EmptyRegion);
        }
        if matches!(&self.target, CaptureTarget::Portal { .. }) && self.codec.is_hdr() {
            return Err(SpecValidationError::HdrPortalUnsupported);
        }
        if let Some(extension) = self.output.extension().and_then(|value| value.to_str()) {
            match Container::from_extension(extension) {
                Some(container) if container == self.container => {}
                Some(_) | None => {
                    return Err(SpecValidationError::ContainerExtensionMismatch {
                        container: self.container,
                        extension: extension.to_owned(),
                    });
                }
            }
        } else {
            return Err(SpecValidationError::MissingOutputExtension);
        }
        for track in &self.audio_tracks {
            track.to_gsr_value()?;
        }
        if let Some(webcam) = &self.webcam
            && (!(1..=100).contains(&webcam.width_percent)
                || !(1..=100).contains(&webcam.height_percent))
        {
            return Err(SpecValidationError::InvalidWebcamSize);
        }
        Ok(())
    }
}

/// Immutable result of capability policy. Process planners consume only this type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EvaluatedSpec {
    pub spec: RecordingSpec,
    pub backend: CaptureBackend,
    pub codec: Codec,
    pub capture_device: Option<String>,
    pub encoding_device: Option<String>,
    pub capability_generation: u64,
    pub topology_generation: u64,
    pub warnings: Vec<String>,
    pub rationale: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SpecValidationError {
    #[error("frame rate {0} is outside the supported range 1..=1000")]
    InvalidFps(u16),
    #[error("output path is empty")]
    MissingOutput,
    #[error("output path needs a container extension such as .mp4 or .mkv")]
    MissingOutputExtension,
    #[error("container {container:?} does not match output extension {extension:?}")]
    ContainerExtensionMismatch {
        container: Container,
        extension: String,
    },
    #[error("monitor name is empty or contains a NUL byte")]
    InvalidMonitorName,
    #[error("capture region has zero width or height")]
    EmptyRegion,
    #[error("audio track must contain at least one source")]
    EmptyAudioTrack,
    #[error("webcam width and height percentages must be in 1..=100")]
    InvalidWebcamSize,
    #[error("HDR is unavailable with portal capture in the supported GPU Screen Recorder backend")]
    HdrPortalUnsupported,
    #[error("overwrite is unsupported in v1")]
    OverwriteUnsupported,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlannedCommand {
    pub program: PathBuf,
    pub arguments: Vec<String>,
    #[serde(default)]
    pub environment: Vec<(String, String)>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LaunchPlan {
    #[serde(default)]
    pub preview_id: PreviewId,
    #[serde(default = "default_true")]
    pub advisory: bool,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    pub runtime_directory: PathBuf,
    pub gsr_ipc_socket: PathBuf,
    pub first_frame_timestamp: PathBuf,
    pub staging_output: PathBuf,
    pub final_output: PathBuf,
    pub recorder: PlannedCommand,
    pub supervisor: PlannedCommand,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub rationale: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn region_format_matches_gsr_contract() {
        let geometry = Geometry {
            x: -1920,
            y: 0,
            width: 1920,
            height: 1080,
        };
        assert_eq!(geometry.to_gsr_region(), "1920x1080+-1920+0");
    }

    #[test]
    fn mixed_audio_track_uses_pipe_separator() {
        let track = AudioTrack::mixed(vec![AudioSource::DefaultOutput, AudioSource::DefaultInput]);
        assert_eq!(
            track.to_gsr_value().unwrap(),
            "default_output|default_input"
        );
    }

    #[test]
    fn portal_hdr_is_rejected_before_launch() {
        let spec = RecordingSpec {
            target: CaptureTarget::Portal {
                restore_token_file: None,
            },
            output: PathBuf::from("capture.mkv"),
            fps: 60,
            frame_mode: FrameMode::Constant,
            codec: Codec::HevcHdr,
            container: Container::Mkv,
            fallback_cpu_encoding: true,
            audio_tracks: Vec::new(),
            webcam: None,
            postprocess: PostprocessMode::ValidateOnly,
            overwrite: false,
            exclude_metadata: true,
        };
        assert!(matches!(
            spec.validate(),
            Err(SpecValidationError::HdrPortalUnsupported)
        ));
    }

    #[test]
    fn empty_region_is_rejected() {
        let spec = RecordingSpec {
            target: CaptureTarget::Region {
                geometry: Geometry {
                    x: 0,
                    y: 0,
                    width: 0,
                    height: 1080,
                },
                coordinate_space: CoordinateSpace::Logical,
                evidence: RegionEvidence::default(),
            },
            output: PathBuf::from("capture.mp4"),
            fps: 60,
            frame_mode: FrameMode::Constant,
            codec: Codec::Auto,
            container: Container::Mp4,
            fallback_cpu_encoding: true,
            audio_tracks: Vec::new(),
            webcam: None,
            postprocess: PostprocessMode::ValidateOnly,
            overwrite: false,
            exclude_metadata: false,
        };
        assert!(matches!(
            spec.validate(),
            Err(SpecValidationError::EmptyRegion)
        ));
    }

    #[test]
    fn overwrite_is_rejected_in_v1() {
        let spec = RecordingSpec {
            target: CaptureTarget::Monitor {
                name: "DP-1".to_owned(),
            },
            output: PathBuf::from("capture.mp4"),
            fps: 60,
            frame_mode: FrameMode::Constant,
            codec: Codec::Auto,
            container: Container::Mp4,
            fallback_cpu_encoding: true,
            audio_tracks: Vec::new(),
            webcam: None,
            postprocess: PostprocessMode::ValidateOnly,
            overwrite: true,
            exclude_metadata: true,
        };
        assert!(matches!(
            spec.validate(),
            Err(SpecValidationError::OverwriteUnsupported)
        ));
    }

    #[test]
    fn region_json_requires_coordinate_space() {
        let json = serde_json::json!({
            "kind": "region",
            "geometry": { "x": 0, "y": 0, "width": 100, "height": 100 }
        });
        assert!(serde_json::from_value::<CaptureTarget>(json).is_err());
    }

    #[test]
    fn target_summaries_are_stable() {
        assert_eq!(
            CaptureTarget::Monitor {
                name: "DP-1".to_owned()
            }
            .summary(),
            "monitor DP-1"
        );
        assert_eq!(
            CaptureTarget::Region {
                geometry: Geometry {
                    x: 0,
                    y: 0,
                    width: 100,
                    height: 80
                },
                coordinate_space: CoordinateSpace::Logical,
                evidence: RegionEvidence::default(),
            }
            .summary(),
            "region 100x80+0+0 (logical)"
        );
        assert_eq!(
            CaptureTarget::Portal {
                restore_token_file: None
            }
            .summary(),
            "portal"
        );
    }
}
