//! Adapter for GPU Screen Recorder 6.x.
//!
//! Keep this crate process-oriented in v1. GSR already owns DRM, `PipeWire`, `V4L2`,
//! encoding, and its per-instance IPC protocol; omarec owns policy and lifecycle.

mod command;
mod control;
mod parse;
mod probe;

pub use command::{GsrCommandBuilder, GsrPlan, PlanError, shell_join};
pub use control::{ControlError, GsrCli, GsrStatus};
pub use parse::{
    ApplicationAudio, AudioDevice, CameraDevice, CameraMode, CaptureOption, CodecSet, GpuInfo,
    GsrVersion, HelpFeatures, InfoDocument, MAX_PROBE_BYTES, MonitorOption, ParseError,
    ProbeContext, ProbeDocuments, SystemInfo, assemble_capabilities, decode_bounded,
    parse_application_audio, parse_audio_devices, parse_cameras, parse_capture_options, parse_help,
    parse_info, parse_monitors, parse_version,
};
pub use probe::{ProbeError, ProbeRunner};
