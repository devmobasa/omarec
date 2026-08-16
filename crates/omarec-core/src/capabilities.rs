//! Backend-neutral capability and host facts consumed by policy.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    pub recorder_version: Option<String>,
    pub capture_device: Option<String>,
    pub encoding_device: Option<String>,
    pub capture_options: Vec<String>,
    pub monitors: Vec<String>,
    pub cameras: Vec<String>,
    pub audio_devices: Vec<String>,
    pub applications: Vec<String>,
    pub codecs: Vec<String>,
    pub portal_available: bool,
    pub ipc_available: bool,
    pub native_multi_source_available: bool,
    #[serde(default)]
    pub generation: u64,
    #[serde(default)]
    pub probed_unix_ms: Option<u64>,
    pub raw_probe: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct HostFacts {
    pub display_server: Option<String>,
    pub gpu_vendor: Option<String>,
    pub card_path: Option<String>,
    pub topology_generation: u64,
}

impl Capabilities {
    pub fn has_capture_option(&self, name: &str) -> bool {
        self.capture_options.iter().any(|option| option == name)
    }

    pub fn has_monitor(&self, name: &str) -> bool {
        self.monitors.iter().any(|monitor| monitor == name)
    }

    pub fn has_camera(&self, path: &str) -> bool {
        self.cameras.iter().any(|camera| camera == path)
    }

    pub fn has_audio_device(&self, id: &str) -> bool {
        self.audio_devices.iter().any(|device| device == id)
    }

    pub fn has_application(&self, name: &str) -> bool {
        self.applications
            .iter()
            .any(|application| application == name)
    }
}
