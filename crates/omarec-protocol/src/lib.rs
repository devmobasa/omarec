//! Versioned, newline-delimited JSON protocol used on the local Unix socket.

use std::path::{Path, PathBuf};

use futures_util::{SinkExt, StreamExt};
use omarec_core::{DaemonLifetimeId, LaunchPlan, RecordingRequest, SessionId, SessionSnapshot};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LinesCodec, LinesCodecError};
use uuid::Uuid;

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const ERROR_SESSION_MISMATCH: &str = "session_mismatch";
pub const ERROR_OVERWRITE_UNSUPPORTED: &str = "overwrite_unsupported";
pub const ERROR_INVALID_REQUEST: &str = "invalid_request";
pub const ERROR_UNSUPPORTED_PROTOCOL: &str = "unsupported_protocol";
pub const ERROR_UNAUTHORIZED: &str = "unauthorized";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub protocol: u16,
    pub request_id: Uuid,
    pub request: Request,
}

impl RequestEnvelope {
    pub fn new(request: Request) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            request_id: Uuid::now_v7(),
            request,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Hello,
    Status,
    Capabilities {
        refresh: bool,
    },
    Plan {
        request: RecordingRequest,
    },
    Start {
        request: RecordingRequest,
    },
    Stop {
        expected_session_id: SessionId,
        force: bool,
    },
    Pause {
        expected_session_id: SessionId,
    },
    Resume {
        expected_session_id: SessionId,
    },
    Reload,
    Watch,
    Doctor,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResponseEnvelope {
    pub protocol: u16,
    pub request_id: Uuid,
    pub response: Response,
}

impl ResponseEnvelope {
    pub fn new(request_id: Uuid, response: Response) -> Self {
        Self {
            protocol: PROTOCOL_VERSION,
            request_id,
            response,
        }
    }

    pub fn error(request_id: Uuid, code: impl Into<String>, message: impl Into<String>) -> Self {
        Self::new(
            request_id,
            Response::Error {
                code: code.into(),
                message: message.into(),
                retryable: false,
                details: None,
            },
        )
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Hello {
        daemon_version: String,
        protocol: u16,
        features: Vec<String>,
    },
    Status {
        snapshot: SessionSnapshot,
        #[serde(default)]
        daemon_lifetime_id: Option<DaemonLifetimeId>,
        #[serde(default)]
        config_generation: u64,
    },
    Capabilities {
        capabilities: RecorderCapabilities,
    },
    Plan {
        plan: LaunchPlan,
    },
    Accepted {
        session_id: SessionId,
    },
    Acknowledged,
    Reloaded {
        config_generation: u64,
    },
    Doctor {
        report: DoctorReport,
    },
    Error {
        code: String,
        message: String,
        retryable: bool,
        details: Option<serde_json::Value>,
    },
}

pub use omarec_core::Capabilities as RecorderCapabilities;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub checks: Vec<DoctorCheck>,
    pub redactions_applied: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DoctorCheck {
    pub id: String,
    pub status: CheckStatus,
    pub summary: String,
    pub detail: Option<String>,
    pub remediation: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Pass,
    Warning,
    Fail,
    Skipped,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub protocol: u16,
    pub sequence: u64,
    #[serde(default)]
    pub daemon_lifetime_id: Option<DaemonLifetimeId>,
    pub event: Event,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Snapshot {
        snapshot: SessionSnapshot,
        #[serde(default)]
        watermark: u64,
    },
    StateChanged {
        snapshot: SessionSnapshot,
    },
    FirstFrame {
        session_id: SessionId,
    },
    FileSaved {
        session_id: SessionId,
        output: PathBuf,
    },
    ConfigReloaded {
        generation: u64,
    },
    Lag {
        skipped: u64,
    },
    Warning {
        session_id: Option<SessionId>,
        message: String,
    },
    Error {
        session_id: Option<SessionId>,
        code: String,
        message: String,
    },
    Heartbeat,
}

/// A bounded NDJSON connection. The protocol deliberately avoids an in-process ABI.
#[derive(Debug)]
pub struct JsonLineConnection {
    framed: Framed<UnixStream, LinesCodec>,
}

impl JsonLineConnection {
    pub async fn connect(path: &Path) -> Result<Self, TransportError> {
        let stream = UnixStream::connect(path)
            .await
            .map_err(TransportError::Connect)?;
        Ok(Self::from_stream(stream))
    }

    pub fn from_stream(stream: UnixStream) -> Self {
        Self {
            framed: Framed::new(stream, LinesCodec::new_with_max_length(MAX_FRAME_BYTES)),
        }
    }

    pub async fn send<T: Serialize>(&mut self, value: &T) -> Result<(), TransportError> {
        let line = serde_json::to_string(value).map_err(TransportError::Serialize)?;
        self.framed.send(line).await.map_err(TransportError::Codec)
    }

    pub async fn receive<T: DeserializeOwned>(&mut self) -> Result<Option<T>, TransportError> {
        let Some(line) = self.framed.next().await else {
            return Ok(None);
        };
        let line = line.map_err(TransportError::Codec)?;
        let value = serde_json::from_str(&line).map_err(TransportError::Deserialize)?;
        Ok(Some(value))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("failed to connect to daemon: {0}")]
    Connect(std::io::Error),
    #[error("NDJSON framing failed: {0}")]
    Codec(LinesCodecError),
    #[error("failed to serialize protocol frame: {0}")]
    Serialize(serde_json::Error),
    #[error("failed to deserialize protocol frame: {0}")]
    Deserialize(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use omarec_core::RecordingRequest;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/protocol")
            .join(name)
    }

    fn read_json(name: &str) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(fixture(name)).unwrap()).unwrap()
    }

    #[test]
    fn request_shape_is_stable() {
        let envelope = RequestEnvelope {
            protocol: PROTOCOL_VERSION,
            request_id: Uuid::nil(),
            request: Request::Status,
        };
        let json = serde_json::to_value(envelope).unwrap();
        assert_eq!(json["protocol"], 1);
        assert_eq!(json["request"]["type"], "status");
    }

    #[test]
    fn mutating_controls_require_expected_session_id() {
        let json = read_json("request-stop.json");
        let envelope: RequestEnvelope = serde_json::from_value(json).unwrap();
        match envelope.request {
            Request::Stop {
                expected_session_id,
                force,
            } => {
                assert!(!force);
                assert_eq!(
                    expected_session_id.to_string(),
                    "00000000-0000-0000-0000-000000000001"
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn completed_response_is_not_part_of_v1() {
        let json = serde_json::json!({
            "protocol": 1,
            "request_id": "00000000-0000-0000-0000-000000000000",
            "response": { "type": "completed", "output": "/tmp/a.mp4" }
        });
        assert!(serde_json::from_value::<ResponseEnvelope>(json).is_err());
    }

    #[test]
    fn old_status_response_deserializes_after_additive_fields() {
        let json = read_json("response-status-old.json");
        let envelope: ResponseEnvelope = serde_json::from_value(json).unwrap();
        match envelope.response {
            Response::Status {
                snapshot,
                daemon_lifetime_id,
                config_generation,
            } => {
                assert_eq!(snapshot.phase, omarec_core::SessionPhase::Idle);
                assert!(daemon_lifetime_id.is_none());
                assert_eq!(config_generation, 0);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn golden_request_plan_roundtrip() {
        let json = read_json("request-plan.json");
        let envelope: RequestEnvelope = serde_json::from_value(json.clone()).unwrap();
        assert!(matches!(
            envelope.request,
            Request::Plan {
                request: RecordingRequest { .. }
            }
        ));
        let encoded = serde_json::to_value(&envelope).unwrap();
        assert_eq!(encoded["request"]["type"], "plan");
        assert_eq!(encoded["request"]["request"]["target"]["kind"], "monitor");
    }

    #[test]
    fn golden_error_session_mismatch() {
        let json = read_json("response-session-mismatch.json");
        let envelope: ResponseEnvelope = serde_json::from_value(json).unwrap();
        match envelope.response {
            Response::Error { code, .. } => assert_eq!(code, ERROR_SESSION_MISMATCH),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn golden_event_snapshot_has_lifetime_and_watermark() {
        let json = read_json("event-snapshot.json");
        let envelope: EventEnvelope = serde_json::from_value(json).unwrap();
        assert!(envelope.daemon_lifetime_id.is_some());
        match envelope.event {
            Event::Snapshot { watermark, .. } => assert_eq!(watermark, 7),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn durable_record_schema_v1_deserializes() {
        let json = read_json("durable-session-record.json");
        let record: omarec_core::SessionRecord = serde_json::from_value(json).unwrap();
        assert_eq!(record.schema_version, 1);
        assert_eq!(record.phase, omarec_core::SessionPhase::Preparing);
    }

    #[test]
    fn golden_fixtures_deserialize() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/protocol");
        let mut seen = Vec::new();
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name == "provenance.json"
                || path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let json: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
            if name.starts_with("request-") {
                serde_json::from_value::<RequestEnvelope>(json)
                    .unwrap_or_else(|error| panic!("{name} failed as request: {error}"));
            } else if name.starts_with("response-") {
                serde_json::from_value::<ResponseEnvelope>(json)
                    .unwrap_or_else(|error| panic!("{name} failed as response: {error}"));
            } else if name.starts_with("event-") {
                serde_json::from_value::<EventEnvelope>(json)
                    .unwrap_or_else(|error| panic!("{name} failed as event: {error}"));
            } else if name.starts_with("durable-") {
                serde_json::from_value::<omarec_core::SessionRecord>(json)
                    .unwrap_or_else(|error| panic!("{name} failed as durable record: {error}"));
            } else {
                panic!("unexpected fixture {name}");
            }
            seen.push(name);
        }
        assert!(seen.iter().any(|name| name == "request-hello.json"));
        assert!(seen.iter().any(|name| name == "request-start-region.json"));
        assert!(seen.iter().any(|name| name == "response-reloaded.json"));
        assert!(seen.iter().any(|name| name == "event-lag.json"));
        assert!(seen.iter().any(|name| name == "event-heartbeat-old.json"));
        assert!(seen.len() >= 20);
    }

    #[test]
    fn old_heartbeat_deserializes_without_lifetime_id() {
        let json = read_json("event-heartbeat-old.json");
        let envelope: EventEnvelope = serde_json::from_value(json).unwrap();
        assert!(envelope.daemon_lifetime_id.is_none());
        assert!(matches!(envelope.event, Event::Heartbeat));
    }

    #[test]
    fn focused_monitor_is_not_a_protocol_target() {
        let json = serde_json::json!({
            "target": { "kind": "focused_monitor" },
            "overrides": {}
        });
        assert!(serde_json::from_value::<RecordingRequest>(json).is_err());
    }

    #[test]
    fn old_snapshots_default_additive_summary_fields() {
        let json = read_json("event-snapshot.json");
        let envelope: EventEnvelope = serde_json::from_value(json).unwrap();
        match envelope.event {
            Event::Snapshot { snapshot, .. } => {
                assert!(snapshot.target_summary.is_none());
                assert!(snapshot.profile.is_none());
                assert!(!snapshot.desktop_audio);
                assert!(!snapshot.microphone);
                assert!(snapshot.webcam_summary.is_none());
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn session_phases_roundtrip() {
        for phase in omarec_core::SessionPhase::all() {
            let encoded = serde_json::to_value(phase).unwrap();
            let decoded = serde_json::from_value(encoded).unwrap();
            assert_eq!(phase, decoded);
        }
    }
}
