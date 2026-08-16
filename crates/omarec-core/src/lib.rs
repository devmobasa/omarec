//! Stable domain types shared by the omarec daemon, CLI, and adapters.

pub mod capabilities;
pub mod config;
pub mod domain;
pub mod first_frame;
pub mod naming;
pub mod paths;
pub mod policy;
pub mod redact;
pub mod state;

pub use capabilities::{Capabilities, HostFacts};
pub use config::{
    BackendConfig, BinaryConfig, CONFIG_SCHEMA_VERSION, Config, ConfigError, DaemonConfig,
    DiagnosticsConfig, OutputConfig, PostprocessConfig, Profile,
};
pub use domain::{
    AudioSource, AudioTrack, CaptureBackend, CaptureTarget, Codec, Container, CoordinateSpace,
    DaemonLifetimeId, EvaluatedSpec, FrameMode, Geometry, HorizontalAlign, LaunchPlan,
    PlannedCommand, PostprocessMode, PreviewId, RecordingRequest, RecordingSpec, RegionEvidence,
    RequestOverrides, SessionId, SpecValidationError, VerticalAlign, WebcamConfig,
};
pub use first_frame::{FirstFrameTimestamp, TimestampError, parse_first_frame_timestamp};
pub use naming::{
    Clock, NamingError, OccupiedNames, OutputNamer, OutputPlan, PathOccupied, parse_user_dirs,
    reserve_explicit, staging_path,
};
pub use paths::{AppPaths, PathError};
pub use policy::{PolicyError, evaluate};
pub use redact::{contains_sensitive, redact_text};
pub use state::{
    DURABLE_RECORD_SCHEMA_VERSION, SessionMachine, SessionPhase, SessionRecord, SessionSnapshot,
    TransitionError,
};
