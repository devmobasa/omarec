//! Pure capability policy. No process or filesystem I/O.

use crate::{
    AudioSource, BackendConfig, Capabilities, CaptureBackend, CaptureTarget, Codec, EvaluatedSpec,
    HostFacts, RecordingSpec, WebcamConfig,
};

const AUTO_CODEC_ORDER: [Codec; 5] = [Codec::Hevc, Codec::Av1, Codec::H264, Codec::Vp9, Codec::Vp8];

pub fn evaluate(
    spec: &RecordingSpec,
    capabilities: &Capabilities,
    host: &HostFacts,
    backend: &BackendConfig,
) -> Result<EvaluatedSpec, PolicyError> {
    spec.validate().map_err(PolicyError::InvalidSpec)?;
    if !capabilities.ipc_available {
        return Err(PolicyError::BackendUnavailable {
            backend: "gsr_ipc".to_owned(),
            reason: "per-instance GPU Screen Recorder IPC is required".to_owned(),
            alternatives: Vec::new(),
        });
    }

    let mut warnings = capabilities.warnings.clone();
    let mut rationale = Vec::new();
    let selected_backend = select_backend(spec, capabilities, backend, &mut rationale)?;
    let codec = select_codec(spec, capabilities, backend, &mut rationale, &mut warnings)?;
    validate_target(spec, capabilities, selected_backend)?;
    validate_audio(spec, capabilities)?;
    validate_webcam(spec.webcam.as_ref(), capabilities, backend, &mut warnings)?;

    if selected_backend == CaptureBackend::Portal && codec.is_hdr() {
        return Err(PolicyError::BackendCodecConflict {
            backend: selected_backend,
            codec,
            alternatives: non_hdr_alternatives(capabilities, backend),
        });
    }

    Ok(EvaluatedSpec {
        spec: spec.clone(),
        backend: selected_backend,
        codec,
        capture_device: capabilities
            .capture_device
            .clone()
            .or_else(|| host.gpu_vendor.clone()),
        encoding_device: capabilities
            .encoding_device
            .clone()
            .or_else(|| host.gpu_vendor.clone()),
        capability_generation: capabilities.generation,
        topology_generation: host.topology_generation,
        warnings,
        rationale,
    })
}

fn select_backend(
    spec: &RecordingSpec,
    capabilities: &Capabilities,
    backend: &BackendConfig,
    rationale: &mut Vec<String>,
) -> Result<CaptureBackend, PolicyError> {
    let selected = match &spec.target {
        CaptureTarget::Portal { .. } => CaptureBackend::Portal,
        CaptureTarget::Monitor { .. } | CaptureTarget::Region { .. } => CaptureBackend::Direct,
    };
    match selected {
        CaptureBackend::Portal if !capabilities.portal_available => {
            return Err(PolicyError::BackendUnavailable {
                backend: "portal".to_owned(),
                reason: "portal capture is not advertised by GPU Screen Recorder".to_owned(),
                alternatives: direct_alternatives(capabilities),
            });
        }
        CaptureBackend::Direct
            if backend.preferred == CaptureBackend::Portal && capabilities.portal_available =>
        {
            rationale.push(
                "keeping the resolved monitor/region target; the daemon never retargets to portal"
                    .to_owned(),
            );
        }
        _ => {}
    }
    rationale.push(format!("backend {selected:?}"));
    Ok(selected)
}

fn select_codec(
    spec: &RecordingSpec,
    capabilities: &Capabilities,
    backend: &BackendConfig,
    rationale: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Result<Codec, PolicyError> {
    if spec.codec == Codec::Auto {
        for codec in AUTO_CODEC_ORDER {
            if codec_available(codec, capabilities, backend.allow_cpu_encoding_fallback) {
                rationale.push(format!(
                    "codec auto resolved to {} from capability generation {}",
                    codec.as_gsr_value(),
                    capabilities.generation
                ));
                if uses_software_only(codec, capabilities) {
                    warnings.push(format!(
                        "using software {} because a GPU encoder was not advertised",
                        codec.as_gsr_value()
                    ));
                }
                return Ok(codec);
            }
        }
        return Err(PolicyError::CodecUnavailable {
            codec: Codec::Auto,
            alternatives: advertised_codecs(capabilities),
        });
    }
    if codec_available(
        spec.codec,
        capabilities,
        backend.allow_cpu_encoding_fallback,
    ) {
        rationale.push(format!("codec {}", spec.codec.as_gsr_value()));
        return Ok(spec.codec);
    }
    Err(PolicyError::CodecUnavailable {
        codec: spec.codec,
        alternatives: advertised_codecs(capabilities),
    })
}

fn validate_target(
    spec: &RecordingSpec,
    capabilities: &Capabilities,
    backend: CaptureBackend,
) -> Result<(), PolicyError> {
    match &spec.target {
        CaptureTarget::Monitor { name } => {
            if !capabilities.has_monitor(name) {
                return Err(PolicyError::TargetUnavailable {
                    target: name.clone(),
                    alternatives: capabilities.monitors.clone(),
                });
            }
        }
        CaptureTarget::Region { evidence, .. } => {
            if !capabilities.has_capture_option("region") {
                return Err(PolicyError::TargetUnavailable {
                    target: "region".to_owned(),
                    alternatives: capabilities.capture_options.clone(),
                });
            }
            if let Some(monitor) = &evidence.monitor
                && !capabilities.has_monitor(monitor)
            {
                return Err(PolicyError::TargetUnavailable {
                    target: monitor.clone(),
                    alternatives: capabilities.monitors.clone(),
                });
            }
        }
        CaptureTarget::Portal { .. } => {
            if backend != CaptureBackend::Portal {
                return Err(PolicyError::BackendUnavailable {
                    backend: "portal".to_owned(),
                    reason: "portal target requires portal backend".to_owned(),
                    alternatives: Vec::new(),
                });
            }
        }
    }
    Ok(())
}

fn validate_audio(spec: &RecordingSpec, capabilities: &Capabilities) -> Result<(), PolicyError> {
    for track in &spec.audio_tracks {
        for source in &track.sources {
            match source {
                AudioSource::DefaultOutput | AudioSource::DefaultInput => {
                    let id = source.to_gsr_value();
                    if !capabilities.audio_devices.is_empty() && !capabilities.has_audio_device(&id)
                    {
                        return Err(PolicyError::AudioSourceUnavailable {
                            id,
                            alternatives: capabilities.audio_devices.clone(),
                        });
                    }
                }
                AudioSource::Device { name } => {
                    if !capabilities.has_audio_device(name) {
                        return Err(PolicyError::AudioSourceUnavailable {
                            id: name.clone(),
                            alternatives: capabilities.audio_devices.clone(),
                        });
                    }
                }
                AudioSource::Application { name } | AudioSource::ApplicationExcept { name } => {
                    if !capabilities.applications.is_empty() && !capabilities.has_application(name)
                    {
                        return Err(PolicyError::AudioSourceUnavailable {
                            id: name.clone(),
                            alternatives: capabilities.applications.clone(),
                        });
                    }
                }
            }
        }
    }
    Ok(())
}

fn validate_webcam(
    webcam: Option<&WebcamConfig>,
    capabilities: &Capabilities,
    backend: &BackendConfig,
    warnings: &mut Vec<String>,
) -> Result<(), PolicyError> {
    let Some(webcam) = webcam else {
        return Ok(());
    };
    let path = webcam.device.display().to_string();
    if !capabilities.cameras.is_empty() && !capabilities.has_camera(&path) {
        return Err(PolicyError::CameraUnavailable {
            device: path,
            alternatives: capabilities.cameras.clone(),
        });
    }
    if capabilities.native_multi_source_available && backend.allow_native_webcam_composition {
        return Ok(());
    }
    if backend.allow_compatibility_webcam_overlay {
        warnings.push(
            "native multi-source composition is unavailable; keep the compatibility overlay as a fallback"
                .to_owned(),
        );
        return Ok(());
    }
    Err(PolicyError::CameraUnavailable {
        device: path,
        alternatives: Vec::new(),
    })
}

fn codec_available(codec: Codec, capabilities: &Capabilities, allow_cpu: bool) -> bool {
    let name = codec.as_gsr_value();
    capabilities.codecs.iter().any(|advertised| {
        advertised == name || (allow_cpu && advertised == &format!("{name}_software"))
    })
}

fn uses_software_only(codec: Codec, capabilities: &Capabilities) -> bool {
    let name = codec.as_gsr_value();
    let gpu = capabilities
        .codecs
        .iter()
        .any(|advertised| advertised == name);
    let software = capabilities
        .codecs
        .iter()
        .any(|advertised| advertised == &format!("{name}_software"));
    software && !gpu
}

fn advertised_codecs(capabilities: &Capabilities) -> Vec<String> {
    capabilities
        .codecs
        .iter()
        .filter(|name| Codec::from_gsr_value(name).is_some())
        .cloned()
        .collect()
}

fn non_hdr_alternatives(capabilities: &Capabilities, backend: &BackendConfig) -> Vec<String> {
    AUTO_CODEC_ORDER
        .into_iter()
        .filter(|codec| codec_available(*codec, capabilities, backend.allow_cpu_encoding_fallback))
        .map(|codec| codec.as_gsr_value().to_owned())
        .collect()
}

fn direct_alternatives(capabilities: &Capabilities) -> Vec<String> {
    capabilities.monitors.clone()
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PolicyError {
    #[error("recording request is invalid: {0}")]
    InvalidSpec(#[source] crate::SpecValidationError),
    #[error("capture target {target} is not available")]
    TargetUnavailable {
        target: String,
        alternatives: Vec<String>,
    },
    #[error("backend {backend} is unavailable: {reason}")]
    BackendUnavailable {
        backend: String,
        reason: String,
        alternatives: Vec<String>,
    },
    #[error("codec {codec:?} is unavailable")]
    CodecUnavailable {
        codec: Codec,
        alternatives: Vec<String>,
    },
    #[error("backend {backend:?} cannot encode {codec:?}")]
    BackendCodecConflict {
        backend: CaptureBackend,
        codec: Codec,
        alternatives: Vec<String>,
    },
    #[error("audio source {id} is unavailable")]
    AudioSourceUnavailable {
        id: String,
        alternatives: Vec<String>,
    },
    #[error("camera {device} is unavailable")]
    CameraUnavailable {
        device: String,
        alternatives: Vec<String>,
    },
}

impl PolicyError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidSpec(_) => "invalid_request",
            Self::TargetUnavailable { .. } => "target_unavailable",
            Self::BackendUnavailable { .. } => "backend_unavailable",
            Self::CodecUnavailable { .. } | Self::BackendCodecConflict { .. } => {
                "backend_codec_conflict"
            }
            Self::AudioSourceUnavailable { .. } => "audio_source_unavailable",
            Self::CameraUnavailable { .. } => "camera_unavailable",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::{
        AudioTrack, Container, CoordinateSpace, FrameMode, Geometry, PostprocessMode,
        RegionEvidence,
    };

    fn nvidia() -> Capabilities {
        Capabilities {
            recorder_version: Some("6.0.0".to_owned()),
            capture_device: Some("nvidia".to_owned()),
            encoding_device: Some("nvidia".to_owned()),
            capture_options: vec![
                "DP-1".to_owned(),
                "DP-3".to_owned(),
                "region".to_owned(),
                "portal".to_owned(),
            ],
            monitors: vec!["DP-1".to_owned(), "DP-3".to_owned()],
            cameras: vec!["/dev/video0".to_owned()],
            audio_devices: vec!["default_output".to_owned(), "default_input".to_owned()],
            applications: vec!["example-browser".to_owned()],
            codecs: vec![
                "h264".to_owned(),
                "hevc".to_owned(),
                "hevc_hdr".to_owned(),
                "av1".to_owned(),
            ],
            portal_available: true,
            ipc_available: true,
            native_multi_source_available: true,
            generation: 7,
            probed_unix_ms: Some(1),
            raw_probe: Vec::new(),
            warnings: Vec::new(),
        }
    }

    fn intel_legacy() -> Capabilities {
        Capabilities {
            codecs: vec!["h264".to_owned(), "h264_software".to_owned()],
            capture_options: vec!["eDP-1".to_owned(), "region".to_owned(), "portal".to_owned()],
            monitors: vec!["eDP-1".to_owned()],
            portal_available: true,
            ipc_available: true,
            ..Capabilities::default()
        }
    }

    fn spec(target: CaptureTarget, codec: Codec) -> RecordingSpec {
        RecordingSpec {
            target,
            output: PathBuf::from("/tmp/out.mp4"),
            fps: 60,
            frame_mode: FrameMode::Constant,
            codec,
            container: Container::Mp4,
            fallback_cpu_encoding: true,
            audio_tracks: vec![AudioTrack::mixed(vec![AudioSource::DefaultOutput])],
            webcam: None,
            postprocess: PostprocessMode::ValidateOnly,
            overwrite: false,
            exclude_metadata: true,
        }
    }

    fn host() -> HostFacts {
        HostFacts {
            display_server: Some("wayland".to_owned()),
            gpu_vendor: Some("nvidia".to_owned()),
            card_path: Some("/dev/dri/card1".to_owned()),
            topology_generation: 3,
        }
    }

    #[test]
    fn auto_codec_prefers_hevc_on_nvidia() {
        let evaluated = evaluate(
            &spec(
                CaptureTarget::Monitor {
                    name: "DP-1".to_owned(),
                },
                Codec::Auto,
            ),
            &nvidia(),
            &host(),
            &BackendConfig::default(),
        )
        .unwrap();
        assert_eq!(evaluated.codec, Codec::Hevc);
        assert_eq!(evaluated.backend, CaptureBackend::Direct);
        assert_eq!(evaluated.capability_generation, 7);
        assert_eq!(evaluated.topology_generation, 3);
        assert!(evaluated.rationale.iter().any(|line| line.contains("hevc")));
    }

    #[test]
    fn auto_codec_falls_back_to_h264_on_intel_legacy() {
        let evaluated = evaluate(
            &spec(
                CaptureTarget::Monitor {
                    name: "eDP-1".to_owned(),
                },
                Codec::Auto,
            ),
            &intel_legacy(),
            &HostFacts::default(),
            &BackendConfig::default(),
        )
        .unwrap();
        assert_eq!(evaluated.codec, Codec::H264);
    }

    #[test]
    fn missing_monitor_is_not_silently_retargeted() {
        let error = evaluate(
            &spec(
                CaptureTarget::Monitor {
                    name: "HDMI-A-1".to_owned(),
                },
                Codec::H264,
            ),
            &nvidia(),
            &host(),
            &BackendConfig::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "target_unavailable");
        match error {
            PolicyError::TargetUnavailable { alternatives, .. } => {
                assert_eq!(alternatives, ["DP-1", "DP-3"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn hdr_portal_is_rejected_with_alternatives() {
        let error = evaluate(
            &spec(
                CaptureTarget::Portal {
                    restore_token_file: None,
                },
                Codec::HevcHdr,
            ),
            &nvidia(),
            &host(),
            &BackendConfig::default(),
        )
        .unwrap_err();
        assert!(matches!(error, PolicyError::InvalidSpec(_)));
    }

    #[test]
    fn unsupported_codec_lists_alternatives() {
        let error = evaluate(
            &spec(
                CaptureTarget::Monitor {
                    name: "eDP-1".to_owned(),
                },
                Codec::Av1,
            ),
            &intel_legacy(),
            &HostFacts::default(),
            &BackendConfig::default(),
        )
        .unwrap_err();
        assert_eq!(error.code(), "backend_codec_conflict");
    }

    #[test]
    fn stale_region_evidence_is_rejected() {
        let spec = spec(
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
                    scale: Some("1.0".to_owned()),
                    transform: Some(1),
                },
            },
            Codec::H264,
        );
        let error = evaluate(&spec, &nvidia(), &host(), &BackendConfig::default()).unwrap_err();
        assert_eq!(error.code(), "target_unavailable");
    }

    #[test]
    fn missing_audio_device_is_rejected() {
        let mut spec = spec(
            CaptureTarget::Monitor {
                name: "DP-1".to_owned(),
            },
            Codec::H264,
        );
        spec.audio_tracks = vec![AudioTrack::mixed(vec![AudioSource::Device {
            name: "missing".to_owned(),
        }])];
        let error = evaluate(&spec, &nvidia(), &host(), &BackendConfig::default()).unwrap_err();
        assert_eq!(error.code(), "audio_source_unavailable");
    }

    #[test]
    fn webcam_uses_native_composition_when_advertised() {
        let mut spec = spec(
            CaptureTarget::Monitor {
                name: "DP-1".to_owned(),
            },
            Codec::H264,
        );
        spec.webcam = Some(WebcamConfig {
            device: PathBuf::from("/dev/video0"),
            ..WebcamConfig::default()
        });
        let evaluated = evaluate(&spec, &nvidia(), &host(), &BackendConfig::default()).unwrap();
        assert_eq!(evaluated.backend, CaptureBackend::Direct);
        assert!(evaluated.warnings.is_empty());
    }
}
