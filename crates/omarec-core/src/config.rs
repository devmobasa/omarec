use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{
    AudioSource, AudioTrack, CaptureBackend, Codec, Container, FrameMode, PostprocessMode,
    RecordingRequest, RecordingSpec, SpecValidationError, WebcamConfig,
};

pub const CONFIG_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_schema_version")]
    pub version: u32,
    #[serde(default = "default_profile_name")]
    pub default_profile: String,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
    #[serde(default)]
    pub daemon: DaemonConfig,
    #[serde(default)]
    pub output: OutputConfig,
    #[serde(default)]
    pub backend: BackendConfig,
    #[serde(default)]
    pub postprocess: PostprocessConfig,
    #[serde(default)]
    pub diagnostics: DiagnosticsConfig,
    #[serde(default)]
    pub binaries: BinaryConfig,
}

const fn default_schema_version() -> u32 {
    CONFIG_SCHEMA_VERSION
}

fn default_profile_name() -> String {
    "default".to_owned()
}

impl Default for Config {
    fn default() -> Self {
        let mut profiles = BTreeMap::new();
        profiles.insert("default".to_owned(), Profile::default());
        Self {
            version: CONFIG_SCHEMA_VERSION,
            default_profile: default_profile_name(),
            profiles,
            daemon: DaemonConfig::default(),
            output: OutputConfig::default(),
            backend: BackendConfig::default(),
            postprocess: PostprocessConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            binaries: BinaryConfig::default(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let source = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        Self::parse(&source, path)
    }

    pub fn parse(source: &str, path: &Path) -> Result<Self, ConfigError> {
        let config = toml::from_str::<Self>(source).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.version != CONFIG_SCHEMA_VERSION {
            return Err(ConfigError::UnsupportedVersion(self.version));
        }
        if !self.profiles.contains_key(&self.default_profile) {
            return Err(ConfigError::UnknownDefaultProfile(
                self.default_profile.clone(),
            ));
        }
        Ok(())
    }

    pub fn profile(&self, name: Option<&str>) -> Result<&Profile, ConfigError> {
        let name = name.unwrap_or(&self.default_profile);
        self.profiles
            .get(name)
            .ok_or_else(|| ConfigError::UnknownProfile(name.to_owned()))
    }

    /// Resolve a request against profiles and compiled defaults. Performs no I/O.
    pub fn resolve_request(
        &self,
        request: &RecordingRequest,
    ) -> Result<RecordingSpec, ConfigError> {
        let profile = self.profile(request.profile.as_deref())?;
        let overrides = &request.overrides;
        if overrides.overwrite == Some(true) {
            return Err(ConfigError::OverwriteUnsupported);
        }
        let output = request.output.clone().ok_or(ConfigError::MissingOutput)?;
        let spec = RecordingSpec {
            target: request.target.clone(),
            output,
            fps: overrides.fps.unwrap_or(profile.fps),
            frame_mode: overrides.frame_mode.unwrap_or(profile.frame_mode),
            codec: overrides.codec.unwrap_or(profile.codec),
            container: overrides.container.unwrap_or(profile.container),
            fallback_cpu_encoding: overrides
                .fallback_cpu_encoding
                .unwrap_or(profile.fallback_cpu_encoding),
            audio_tracks: overrides
                .audio_tracks
                .clone()
                .unwrap_or_else(|| profile.audio_tracks.clone()),
            webcam: overrides.webcam.clone().or_else(|| profile.webcam.clone()),
            postprocess: overrides.postprocess.unwrap_or(profile.postprocess),
            overwrite: false,
            exclude_metadata: overrides
                .exclude_metadata
                .unwrap_or(profile.exclude_metadata),
        };
        spec.validate().map_err(ConfigError::InvalidSpec)?;
        Ok(spec)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DaemonConfig {
    pub startup_timeout_ms: u64,
    pub control_timeout_ms: u64,
    pub finalization_timeout_ms: u64,
    pub terminal_state_retention_ms: u64,
    pub capability_cache_ms: u64,
    pub max_protocol_frame_bytes: usize,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            startup_timeout_ms: 10_000,
            control_timeout_ms: 20_000,
            finalization_timeout_ms: 120_000,
            terminal_state_retention_ms: 5_000,
            capability_cache_ms: 5_000,
            max_protocol_frame_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OutputConfig {
    pub directory: Option<PathBuf>,
    pub filename: String,
    pub default_container: Container,
    pub overwrite: bool,
    pub minimum_free_space_mib: u64,
    pub warn_free_space_mib: u64,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            directory: None,
            filename: "%Y-%m-%d_%H-%M-%S".to_owned(),
            default_container: Container::Mp4,
            overwrite: false,
            minimum_free_space_mib: 512,
            warn_free_space_mib: 2048,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[allow(clippy::struct_excessive_bools)]
pub struct BackendConfig {
    pub preferred: CaptureBackend,
    pub allow_portal_fallback: bool,
    pub allow_cpu_encoding_fallback: bool,
    pub allow_native_webcam_composition: bool,
    pub allow_compatibility_webcam_overlay: bool,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            preferred: CaptureBackend::Auto,
            allow_portal_fallback: true,
            allow_cpu_encoding_fallback: true,
            allow_native_webcam_composition: true,
            allow_compatibility_webcam_overlay: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PostprocessConfig {
    pub default: PostprocessMode,
    pub ffprobe_binary: PathBuf,
    pub ffmpeg_binary: PathBuf,
}

impl Default for PostprocessConfig {
    fn default() -> Self {
        Self {
            default: PostprocessMode::ValidateOnly,
            ffprobe_binary: PathBuf::from("ffprobe"),
            ffmpeg_binary: PathBuf::from("ffmpeg"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiagnosticsConfig {
    pub retain_sessions: u32,
    pub journal_lines: u32,
    pub redact_user_paths: bool,
    pub redact_hostnames: bool,
}

impl Default for DiagnosticsConfig {
    fn default() -> Self {
        Self {
            retain_sessions: 25,
            journal_lines: 200,
            redact_user_paths: true,
            redact_hostnames: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BinaryConfig {
    pub recorder_binary: PathBuf,
    pub recorder_cli_binary: PathBuf,
    pub systemd_run_binary: PathBuf,
    pub systemctl_binary: PathBuf,
    pub journalctl_binary: PathBuf,
}

impl Default for BinaryConfig {
    fn default() -> Self {
        Self {
            recorder_binary: PathBuf::from("gpu-screen-recorder"),
            recorder_cli_binary: PathBuf::from("gsr-cli"),
            systemd_run_binary: PathBuf::from("systemd-run"),
            systemctl_binary: PathBuf::from("systemctl"),
            journalctl_binary: PathBuf::from("journalctl"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Profile {
    pub fps: u16,
    pub codec: Codec,
    pub frame_mode: FrameMode,
    pub container: Container,
    pub fallback_cpu_encoding: bool,
    pub audio_tracks: Vec<AudioTrack>,
    pub exclude_metadata: bool,
    pub postprocess: PostprocessMode,
    pub webcam: Option<WebcamConfig>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            fps: 60,
            codec: Codec::Auto,
            frame_mode: FrameMode::Constant,
            container: Container::Mp4,
            fallback_cpu_encoding: true,
            audio_tracks: vec![AudioTrack::mixed(vec![AudioSource::DefaultOutput])],
            exclude_metadata: true,
            postprocess: PostprocessMode::ValidateOnly,
            webcam: None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("unsupported config schema version {0}")]
    UnsupportedVersion(u32),
    #[error("default profile {0:?} is not defined")]
    UnknownDefaultProfile(String),
    #[error("profile {0:?} is not defined")]
    UnknownProfile(String),
    #[error("output path is required")]
    MissingOutput,
    #[error("overwrite is unsupported in v1")]
    OverwriteUnsupported,
    #[error("recording request is invalid: {0}")]
    InvalidSpec(#[source] SpecValidationError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CaptureTarget, RequestOverrides};

    fn request_with(overrides: RequestOverrides) -> RecordingRequest {
        RecordingRequest {
            target: CaptureTarget::Monitor {
                name: "DP-1".to_owned(),
            },
            profile: None,
            output: Some(PathBuf::from("/tmp/out.mp4")),
            overrides,
        }
    }

    #[test]
    fn defaults_are_self_consistent() {
        let config = Config::default();
        config.validate().unwrap();
        assert_eq!(config.profile(None).unwrap().fps, 60);
    }

    #[test]
    fn unset_override_inherits_profile() {
        let config = Config::default();
        let spec = config
            .resolve_request(&request_with(RequestOverrides::default()))
            .unwrap();
        assert_eq!(spec.fps, 60);
        assert_eq!(spec.codec, Codec::Auto);
        assert_eq!(
            spec.audio_tracks,
            vec![AudioTrack::mixed(vec![AudioSource::DefaultOutput])]
        );
    }

    #[test]
    fn explicit_override_beats_profile() {
        let config = Config::default();
        let spec = config
            .resolve_request(&request_with(RequestOverrides {
                fps: Some(30),
                codec: Some(Codec::H264),
                audio_tracks: Some(Vec::new()),
                ..RequestOverrides::default()
            }))
            .unwrap();
        assert_eq!(spec.fps, 30);
        assert_eq!(spec.codec, Codec::H264);
        assert!(spec.audio_tracks.is_empty());
    }

    #[test]
    fn named_profile_beats_default_profile() {
        let mut config = Config::default();
        config.profiles.insert(
            "presentation".to_owned(),
            Profile {
                fps: 30,
                codec: Codec::H264,
                ..Profile::default()
            },
        );
        let mut request = request_with(RequestOverrides::default());
        request.profile = Some("presentation".to_owned());
        let spec = config.resolve_request(&request).unwrap();
        assert_eq!(spec.fps, 30);
        assert_eq!(spec.codec, Codec::H264);
    }

    #[test]
    fn overwrite_request_is_rejected() {
        let config = Config::default();
        let error = config
            .resolve_request(&request_with(RequestOverrides {
                overwrite: Some(true),
                ..RequestOverrides::default()
            }))
            .unwrap_err();
        assert!(matches!(error, ConfigError::OverwriteUnsupported));
    }

    #[test]
    fn example_config_deserializes() {
        let source = include_str!("../../../integration/config/config.example.toml");
        Config::parse(source, Path::new("config.example.toml")).unwrap();
    }
}
