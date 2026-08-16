//! Pure parsers for GPU Screen Recorder probe documents.
//!
//! These functions perform no process or filesystem I/O. A failed required probe must
//! not be turned into an empty capability list by the caller.

use std::collections::BTreeMap;

use omarec_core::Capabilities;

pub const MAX_PROBE_BYTES: usize = 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GsrVersion {
    pub raw: String,
    pub version: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SystemInfo {
    pub display_server: Option<String>,
    pub supports_app_audio: Option<bool>,
    pub is_steam_deck: Option<bool>,
    pub gsr_version: Option<String>,
    pub unknown: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GpuInfo {
    pub vendor: Option<String>,
    pub card_path: Option<String>,
    pub unknown: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CaptureOption {
    pub name: String,
    pub detail: Option<String>,
}

impl CaptureOption {
    pub fn is_portal(&self) -> bool {
        self.name == "portal"
    }

    pub fn is_region(&self) -> bool {
        self.name == "region"
    }

    pub fn is_monitor(&self) -> bool {
        self.detail
            .as_deref()
            .is_some_and(|detail| detail.contains('x'))
            && !self.is_portal()
            && !self.is_region()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MonitorOption {
    pub name: String,
    pub resolution: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CameraMode {
    pub raw: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub fps: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CameraDevice {
    pub path: String,
    pub name: Option<String>,
    pub modes: Vec<CameraMode>,
    pub raw: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioDevice {
    pub id: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApplicationAudio {
    pub name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CodecSet {
    pub names: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct HelpFeatures {
    pub ipc: bool,
    pub first_frame_timestamp: bool,
    pub native_multi_source: bool,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InfoDocument {
    pub system: SystemInfo,
    pub gpu: GpuInfo,
    pub codecs: CodecSet,
    pub capture_options: Vec<CaptureOption>,
    pub unknown_sections: BTreeMap<String, Vec<String>>,
    pub warnings: Vec<String>,
    pub raw_lines: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ProbeDocuments<'a> {
    pub version: Option<&'a str>,
    pub info: &'a str,
    pub capture_options: &'a str,
    pub monitors: Option<&'a str>,
    pub cameras: Option<&'a str>,
    pub audio: Option<&'a str>,
    pub applications: Option<&'a str>,
    pub help: Option<&'a str>,
}

#[derive(Clone, Debug, Default)]
pub struct ProbeContext {
    pub gsr_cli_available: bool,
    pub generation: u64,
    pub probed_unix_ms: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ParseError {
    #[error("GPU Screen Recorder probe document is empty")]
    Empty,
    #[error("GPU Screen Recorder probe exceeded {limit} bytes")]
    Oversized { limit: usize },
    #[error("GPU Screen Recorder probe was not valid UTF-8")]
    InvalidUtf8,
    #[error("GPU Screen Recorder version output {0:?} has no version number")]
    MissingVersion(String),
}

pub fn decode_bounded(bytes: &[u8], limit: usize) -> Result<String, ParseError> {
    if bytes.len() > limit {
        return Err(ParseError::Oversized { limit });
    }
    std::str::from_utf8(bytes)
        .map(str::to_owned)
        .map_err(|_| ParseError::InvalidUtf8)
}

pub fn parse_version(document: &str) -> Result<GsrVersion, ParseError> {
    let raw = document.trim().to_owned();
    if raw.is_empty() {
        return Err(ParseError::Empty);
    }
    let version = raw
        .split_whitespace()
        .find_map(extract_version)
        .ok_or_else(|| ParseError::MissingVersion(raw.clone()))?;
    Ok(GsrVersion { raw, version })
}

pub fn parse_info(document: &str) -> Result<InfoDocument, ParseError> {
    if document.trim().is_empty() {
        return Err(ParseError::Empty);
    }
    let mut info = InfoDocument {
        raw_lines: document.lines().map(str::to_owned).collect(),
        ..InfoDocument::default()
    };
    let mut current = String::new();
    let mut lines: Vec<String> = Vec::new();
    let flush = |section: &str, lines: &[String], info: &mut InfoDocument| match section {
        "" => {}
        "system_info" => info.system = parse_system_info(lines, &mut info.warnings),
        "gpu_info" => info.gpu = parse_gpu_info(lines),
        "video_codecs" => {
            info.codecs.names = lines
                .iter()
                .map(|line| line.trim().to_owned())
                .filter(|line| !line.is_empty())
                .collect();
        }
        "capture_options" => info.capture_options = parse_capture_options(&lines.join("\n")),
        other => {
            info.unknown_sections
                .insert(other.to_owned(), lines.to_vec());
        }
    };
    for line in document.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix("section=") {
            flush(&current, &lines, &mut info);
            name.trim().clone_into(&mut current);
            lines.clear();
            continue;
        }
        if !trimmed.is_empty() {
            lines.push(trimmed.to_owned());
        }
    }
    flush(&current, &lines, &mut info);
    if info.system.gsr_version.is_none() {
        info.warnings
            .push("system_info did not include gsr_version".to_owned());
    }
    Ok(info)
}

pub fn parse_capture_options(document: &str) -> Vec<CaptureOption> {
    parse_named_list(document)
        .into_iter()
        .map(|(name, detail)| CaptureOption { name, detail })
        .collect()
}

pub fn parse_monitors(document: &str) -> Vec<MonitorOption> {
    parse_capture_options(document)
        .into_iter()
        .filter(CaptureOption::is_monitor)
        .map(|option| MonitorOption {
            name: option.name,
            resolution: option.detail,
        })
        .collect()
}

pub fn parse_cameras(document: &str) -> Vec<CameraDevice> {
    document
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (path, rest) = split_pipe(line);
            let (name, modes) = match rest {
                None => (None, Vec::new()),
                Some(rest) if rest.contains('x') && rest.contains('@') => {
                    (None, vec![parse_camera_mode(&rest)])
                }
                Some(rest) => (Some(rest.clone()), Vec::new()),
            };
            CameraDevice {
                path,
                name,
                modes,
                raw: line.to_owned(),
            }
        })
        .collect()
}

pub fn parse_audio_devices(document: &str) -> Vec<AudioDevice> {
    parse_named_list(document)
        .into_iter()
        .map(|(id, description)| AudioDevice { id, description })
        .collect()
}

pub fn parse_application_audio(document: &str) -> Vec<ApplicationAudio> {
    document
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| ApplicationAudio {
            name: line.to_owned(),
        })
        .collect()
}

pub fn parse_help(document: &str) -> HelpFeatures {
    let lower = document.to_ascii_lowercase();
    HelpFeatures {
        ipc: lower.contains("-ipc"),
        first_frame_timestamp: lower.contains("-write-first-frame-ts"),
        native_multi_source: lower.contains("combine sources with |")
            || lower.contains("additional options can be passed to each capture source")
            || lower.contains("v4l2_device_path"),
    }
}

pub fn assemble_capabilities(
    documents: &ProbeDocuments<'_>,
    context: &ProbeContext,
) -> Result<Capabilities, ParseError> {
    let info = parse_info(documents.info)?;
    let capture_options = {
        let listed = parse_capture_options(documents.capture_options);
        if listed.is_empty() {
            info.capture_options.clone()
        } else {
            listed
        }
    };
    if documents.capture_options.trim().is_empty() && info.capture_options.is_empty() {
        return Err(ParseError::Empty);
    }
    let monitors = documents.monitors.map_or_else(
        || {
            capture_options
                .iter()
                .filter(|option| option.is_monitor())
                .map(|option| option.name.clone())
                .collect()
        },
        |document| {
            parse_monitors(document)
                .into_iter()
                .map(|monitor| monitor.name)
                .collect()
        },
    );
    let cameras = documents
        .cameras
        .map(parse_cameras)
        .unwrap_or_default()
        .into_iter()
        .map(|camera| camera.path)
        .collect();
    let audio_devices = documents
        .audio
        .map(parse_audio_devices)
        .unwrap_or_default()
        .into_iter()
        .map(|device| device.id)
        .collect();
    let applications = documents
        .applications
        .map(parse_application_audio)
        .unwrap_or_default()
        .into_iter()
        .map(|application| application.name)
        .collect();
    let help = documents.help.map(parse_help).unwrap_or_default();
    let recorder_version = documents
        .version
        .and_then(|document| parse_version(document).ok())
        .map(|version| version.version)
        .or(info.system.gsr_version.clone());
    let mut warnings = info.warnings;
    if documents.help.is_none() {
        warnings.push("recorder help probe was unavailable".to_owned());
    }
    Ok(Capabilities {
        recorder_version,
        capture_device: info.gpu.vendor.clone(),
        encoding_device: info.gpu.vendor,
        portal_available: capture_options.iter().any(CaptureOption::is_portal),
        ipc_available: context.gsr_cli_available && help.ipc,
        native_multi_source_available: help.native_multi_source,
        capture_options: capture_options
            .into_iter()
            .map(|option| option.name)
            .collect(),
        monitors,
        cameras,
        audio_devices,
        applications,
        codecs: info.codecs.names,
        generation: context.generation,
        probed_unix_ms: context.probed_unix_ms,
        raw_probe: info.raw_lines,
        warnings,
    })
}

fn parse_system_info(lines: &[String], warnings: &mut Vec<String>) -> SystemInfo {
    let mut info = SystemInfo::default();
    for line in lines {
        let (key, value) = split_pipe(line);
        match key.as_str() {
            "display_server" => info.display_server = empty_to_none(value),
            "supports_app_audio" => {
                info.supports_app_audio = parse_flag(value.as_deref(), warnings);
            }
            "is_steam_deck" => {
                info.is_steam_deck = parse_flag(value.as_deref(), warnings);
            }
            "gsr_version" => info.gsr_version = empty_to_none(value),
            other => {
                if let Some(value) = value {
                    info.unknown.insert(other.to_owned(), value);
                }
            }
        }
    }
    info
}

fn parse_gpu_info(lines: &[String]) -> GpuInfo {
    let mut info = GpuInfo::default();
    for line in lines {
        let (key, value) = split_pipe(line);
        match key.as_str() {
            "vendor" => info.vendor = empty_to_none(value),
            "card_path" => info.card_path = empty_to_none(value),
            other => {
                if let Some(value) = value {
                    info.unknown.insert(other.to_owned(), value);
                }
            }
        }
    }
    info
}

fn parse_named_list(document: &str) -> Vec<(String, Option<String>)> {
    document
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(split_pipe)
        .collect()
}

fn parse_camera_mode(raw: &str) -> CameraMode {
    let mut mode = CameraMode {
        raw: raw.to_owned(),
        width: None,
        height: None,
        fps: None,
    };
    let (size, fps) = raw.split_once('@').unwrap_or((raw, ""));
    if let Some((width, height)) = size.split_once('x') {
        mode.width = width.parse().ok();
        mode.height = height.parse().ok();
    }
    if !fps.is_empty() {
        mode.fps = fps.trim_end_matches("fps").trim().parse().ok();
    }
    mode
}

fn parse_flag(value: Option<&str>, warnings: &mut Vec<String>) -> Option<bool> {
    match value.map(str::trim) {
        Some("yes" | "true" | "1") => Some(true),
        Some("no" | "false" | "0") => Some(false),
        Some("") | None => None,
        Some(other) => {
            warnings.push(format!("unrecognized boolean probe value {other:?}"));
            None
        }
    }
}

fn split_pipe(line: &str) -> (String, Option<String>) {
    match line.split_once('|') {
        Some((left, right)) => (
            left.trim().to_owned(),
            empty_to_none(Some(right.trim().to_owned())),
        ),
        None => (line.trim().to_owned(), None),
    }
}

fn empty_to_none(value: Option<String>) -> Option<String> {
    value.filter(|item| !item.is_empty())
}

fn extract_version(field: &str) -> Option<String> {
    let field = field.trim_start_matches('v');
    let mut parts = field.split('.');
    let major = parts.next()?;
    let minor = parts.next()?;
    if major.chars().all(|value| value.is_ascii_digit())
        && minor.chars().all(|value| value.is_ascii_digit())
    {
        Some(field.to_owned())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(relative: &str) -> String {
        std::fs::read_to_string(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/gsr")
                .join(relative),
        )
        .unwrap()
    }

    #[test]
    fn parses_captured_nvidia_info() {
        let info = parse_info(&fixture("captured/nvidia-6.0.0/info.txt")).unwrap();
        assert_eq!(info.system.gsr_version.as_deref(), Some("6.0.0"));
        assert_eq!(info.gpu.vendor.as_deref(), Some("nvidia"));
        assert!(info.codecs.names.contains(&"hevc_hdr".to_owned()));
        assert!(
            info.capture_options
                .iter()
                .any(|option| option.name == "DP-1" && option.is_monitor())
        );
        assert!(info.capture_options.iter().any(CaptureOption::is_portal));
    }

    #[test]
    fn parses_captured_nvidia_version() {
        let version = parse_version(&fixture("captured/nvidia-6.0.0/version.txt")).unwrap();
        assert_eq!(version.version, "6.0.0");
    }

    #[test]
    fn parses_captured_help_features() {
        let help = parse_help(&fixture("captured/nvidia-6.0.0/help.txt"));
        assert!(help.ipc);
        assert!(help.first_frame_timestamp);
        assert!(help.native_multi_source);
    }

    #[test]
    fn parses_synthetic_amd_and_intel_info() {
        let amd = parse_info(&fixture("synthetic/amd-info.txt")).unwrap();
        assert_eq!(amd.gpu.vendor.as_deref(), Some("amd"));
        let intel = parse_info(&fixture("synthetic/intel-legacy-info.txt")).unwrap();
        assert_eq!(intel.codecs.names, ["h264", "h264_software"]);
    }

    #[test]
    fn parses_transformed_monitors_without_relabeling() {
        let monitors = parse_monitors(&fixture("synthetic/transformed-capture-options.txt"));
        assert_eq!(
            monitors,
            [
                MonitorOption {
                    name: "DP-1".to_owned(),
                    resolution: Some("2560x1440".to_owned()),
                },
                MonitorOption {
                    name: "HDMI-A-1".to_owned(),
                    resolution: Some("1080x1920".to_owned()),
                },
            ]
        );
    }

    #[test]
    fn parses_cameras_audio_and_applications() {
        let cameras = parse_cameras(&fixture("synthetic/cameras.txt"));
        assert_eq!(cameras[0].path, "/dev/video0");
        assert_eq!(cameras[0].name.as_deref(), Some("Integrated Camera"));
        let audio = parse_audio_devices(&fixture("captured/nvidia-6.0.0/list-audio-devices.txt"));
        assert_eq!(audio[0].id, "default_output");
        let applications =
            parse_application_audio(&fixture("captured/nvidia-6.0.0/list-application-audio.txt"));
        assert_eq!(applications[0].name, "example-browser");
    }

    #[test]
    fn malformed_truncated_info_is_parsed_with_warnings() {
        let info = parse_info(&fixture("malformed/truncated-section.txt")).unwrap();
        assert!(info.system.gsr_version.is_none());
        assert!(info.gpu.vendor.is_none());
        assert!(
            info.warnings
                .iter()
                .any(|warning| warning.contains("gsr_version"))
        );
    }

    #[test]
    fn unknown_info_sections_are_preserved() {
        let info = parse_info(&fixture("malformed/unknown-section.txt")).unwrap();
        assert!(
            info.unknown_sections
                .contains_key("future_vendor_extension")
        );
        assert_eq!(info.system.gsr_version.as_deref(), Some("6.0.0"));
    }

    #[test]
    fn empty_required_documents_are_errors() {
        assert_eq!(parse_info("").unwrap_err(), ParseError::Empty);
        assert_eq!(parse_version("\n").unwrap_err(), ParseError::Empty);
        assert!(matches!(
            decode_bounded(&[0xff], 16),
            Err(ParseError::InvalidUtf8)
        ));
        assert!(matches!(
            decode_bounded(&[b'a'; 8], 4),
            Err(ParseError::Oversized { limit: 4 })
        ));
    }

    #[test]
    fn assemble_uses_typed_parsers_and_does_not_invent_empty_caps() {
        let capabilities = assemble_capabilities(
            &ProbeDocuments {
                version: Some("6.0.0\n"),
                info: &fixture("captured/nvidia-6.0.0/info.txt"),
                capture_options: &fixture("captured/nvidia-6.0.0/list-capture-options.txt"),
                monitors: Some(&fixture("captured/nvidia-6.0.0/list-monitors.txt")),
                cameras: Some(""),
                audio: Some(&fixture("captured/nvidia-6.0.0/list-audio-devices.txt")),
                applications: Some(&fixture("captured/nvidia-6.0.0/list-application-audio.txt")),
                help: Some(&fixture("captured/nvidia-6.0.0/help.txt")),
            },
            &ProbeContext {
                gsr_cli_available: true,
                generation: 4,
                probed_unix_ms: Some(1),
            },
        )
        .unwrap();
        assert_eq!(capabilities.recorder_version.as_deref(), Some("6.0.0"));
        assert_eq!(capabilities.capture_device.as_deref(), Some("nvidia"));
        assert!(capabilities.portal_available);
        assert!(capabilities.ipc_available);
        assert_eq!(capabilities.generation, 4);
        assert!(capabilities.monitors.contains(&"DP-1".to_owned()));
        assert!(
            assemble_capabilities(
                &ProbeDocuments {
                    version: None,
                    info: "",
                    capture_options: "",
                    monitors: None,
                    cameras: None,
                    audio: None,
                    applications: None,
                    help: None,
                },
                &ProbeContext::default(),
            )
            .is_err()
        );
    }
}
