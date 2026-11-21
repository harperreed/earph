// ABOUTME: Requires the complete device discovery wire structure, including explicit unknowns.
// ABOUTME: Keeps advertised observations separate from opened playback identity and authority.
use super::nullable;
use earph_core::{
    ErrorCode,
    control::OutputBackend,
    devices::{DeviceReadBoundary, DeviceSelectionIntent, DeviceStream, DeviceValueRange},
    ids::OwnerId,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceCapabilitiesView {
    pub backends: Vec<OutputBackend>,
    pub methods: [String; 2],
    pub asynchronous_replay: bool,
    pub max_concurrent_queries: u64,
    pub max_entries: u64,
    pub max_text_bytes: u64,
    pub max_result_bytes: u64,
    pub max_retained_result_bytes: u64,
    pub query_timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceQueryErrorView {
    pub code: ErrorCode,
    pub message: String,
    #[serde(deserialize_with = "nullable")]
    pub native_status: Option<i32>,
    #[serde(deserialize_with = "nullable")]
    pub native_object_id: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSummaryView {
    pub endpoint: String,
    #[serde(deserialize_with = "nullable")]
    pub display_name: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub description: Option<String>,
    #[serde(deserialize_with = "nullable")]
    pub selection: Option<DeviceSelectionIntent>,
    #[serde(deserialize_with = "nullable")]
    pub selection_error: Option<ErrorCode>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DevicePropertiesView {
    pub object_id: u32,
    pub uid: String,
    pub name: String,
    pub alive: bool,
    pub nominal_rate_hz: f64,
    pub nominal_rate_settable: bool,
    pub buffer_frames: u32,
    pub buffer_frame_range: DeviceValueRange,
    #[serde(deserialize_with = "nullable")]
    pub hog_owner_pid: Option<i32>,
    #[serde(deserialize_with = "nullable")]
    pub hog_mode_settable: Option<bool>,
    pub output_streams: Vec<DeviceStream>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceObservationView {
    List {
        devices: Vec<DeviceSummaryView>,
        complete: bool,
        failures: Vec<DeviceQueryErrorView>,
    },
    Inspection {
        endpoint: String,
        #[serde(deserialize_with = "nullable")]
        advertisement: Option<DeviceSummaryView>,
        #[serde(deserialize_with = "nullable")]
        current_properties: Option<Box<DevicePropertiesView>>,
        complete: bool,
        failures: Vec<DeviceQueryErrorView>,
    },
    Failed {
        error: DeviceQueryErrorView,
        cleanup_confirmed: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceQueryResultView {
    pub owner_id: OwnerId,
    pub backend: OutputBackend,
    pub completed_monotonic_ns: u64,
    pub read_boundary: DeviceReadBoundary,
    pub observation: DeviceObservationView,
}
