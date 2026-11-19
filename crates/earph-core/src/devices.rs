// ABOUTME: Defines bounded read-only device discovery observations and query intent.
// ABOUTME: Keeps advertised facts separate from opened-output proof and signal authority.
use crate::{
    EngineError, ErrorCode,
    control::OutputBackend,
    ids::{OwnerId, RequestId},
};
use serde::{Deserialize, Serialize};

pub const MAX_DEVICES: usize = 256;
pub const MAX_DEVICE_TEXT_BYTES: usize = 4096;
pub const MAX_DEVICE_RESULT_BYTES: usize = 256 * 1024;
pub const MAX_RETAINED_DEVICE_BYTES: usize = 2 * 1024 * 1024;
pub const DEVICE_QUERY_TIMEOUT_MS: u64 = 5000;
pub const DEVICE_BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "device query exceeds bounded storage",
);
pub const DEVICE_INVALID: EngineError = EngineError::new(
    ErrorCode::AssetInvalid,
    "invalid device observation or query",
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceQuery {
    pub backend: OutputBackend,
    pub endpoint: Option<String>,
}
impl DeviceQuery {
    pub fn validate(&self) -> Result<(), EngineError> {
        if let Some(endpoint) = &self.endpoint {
            validate_endpoint(endpoint)?;
        }
        Ok(())
    }
}
pub fn validate_endpoint(endpoint: &str) -> Result<(), EngineError> {
    if endpoint.is_empty()
        || endpoint.len() > 256
        || endpoint.trim() != endpoint
        || endpoint.chars().any(char::is_control)
    {
        Err(DEVICE_INVALID)
    } else {
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceQueryJob {
    pub request_id: RequestId,
    pub owner_id: OwnerId,
    pub query: DeviceQuery,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceCapabilities {
    pub backends: Vec<OutputBackend>,
    pub methods: [&'static str; 2],
    pub asynchronous_replay: bool,
    pub max_concurrent_queries: usize,
    pub max_entries: usize,
    pub max_text_bytes: usize,
    pub max_result_bytes: usize,
    pub max_retained_result_bytes: usize,
    pub query_timeout_ms: u64,
}
impl DeviceCapabilities {
    pub fn new(backends: Vec<OutputBackend>) -> Self {
        Self {
            backends,
            methods: ["devices.list", "devices.inspect"],
            asynchronous_replay: true,
            max_concurrent_queries: 1,
            max_entries: MAX_DEVICES,
            max_text_bytes: MAX_DEVICE_TEXT_BYTES,
            max_result_bytes: MAX_DEVICE_RESULT_BYTES,
            max_retained_result_bytes: MAX_RETAINED_DEVICE_BYTES,
            query_timeout_ms: DEVICE_QUERY_TIMEOUT_MS,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceReadBoundary {
    AlsaPcmHints,
    CoreAudioProperties,
}
impl From<OutputBackend> for DeviceReadBoundary {
    fn from(backend: OutputBackend) -> Self {
        match backend {
            OutputBackend::Alsa => Self::AlsaPcmHints,
            OutputBackend::CoreAudio => Self::CoreAudioProperties,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceQueryError {
    pub code: ErrorCode,
    pub message: String,
    pub native_status: Option<i32>,
    pub native_object_id: Option<u32>,
}
impl From<EngineError> for DeviceQueryError {
    fn from(error: EngineError) -> Self {
        Self {
            code: error.code,
            message: error.message.into(),
            native_status: None,
            native_object_id: None,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSelectionIntent {
    pub backend: OutputBackend,
    pub endpoint: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSummary {
    pub endpoint: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub selection: Option<DeviceSelectionIntent>,
    pub selection_error: Option<ErrorCode>,
}
impl DeviceSummary {
    pub fn advertised(
        backend: OutputBackend,
        endpoint: String,
        display_name: Option<String>,
        description: Option<String>,
        alive: Option<bool>,
    ) -> Self {
        let selection_error = if alive == Some(false) {
            Some(ErrorCode::DeviceUnavailable)
        } else {
            validate_endpoint(&endpoint).err().map(|e| e.code)
        };
        let selection = selection_error.is_none().then(|| DeviceSelectionIntent {
            backend,
            endpoint: endpoint.clone(),
        });
        Self {
            endpoint,
            display_name,
            description,
            selection,
            selection_error,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceFormat {
    pub sample_rate_hz: f64,
    pub format_id: u32,
    pub format_flags: u32,
    pub bytes_per_packet: u32,
    pub frames_per_packet: u32,
    pub bytes_per_frame: u32,
    pub channels_per_frame: u32,
    pub bits_per_channel: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceValueRange {
    pub minimum: f64,
    pub maximum: f64,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceRangedFormat {
    pub format: DeviceFormat,
    pub sample_rates: DeviceValueRange,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceStream {
    pub object_id: u32,
    pub physical: DeviceFormat,
    pub virtual_format: DeviceFormat,
    pub physical_settable: bool,
    pub virtual_settable: bool,
    pub available_physical: Vec<DeviceRangedFormat>,
    pub available_virtual: Vec<DeviceRangedFormat>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceProperties {
    pub object_id: u32,
    pub uid: String,
    pub name: String,
    pub alive: bool,
    pub nominal_rate_hz: f64,
    pub nominal_rate_settable: bool,
    pub buffer_frames: u32,
    pub buffer_frame_range: DeviceValueRange,
    pub hog_owner_pid: Option<i32>,
    pub hog_mode_settable: Option<bool>,
    pub output_streams: Vec<DeviceStream>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeviceObservation {
    List {
        devices: Vec<DeviceSummary>,
        complete: bool,
        failures: Vec<DeviceQueryError>,
    },
    Inspection {
        endpoint: String,
        advertisement: Option<DeviceSummary>,
        current_properties: Option<Box<DeviceProperties>>,
        complete: bool,
        failures: Vec<DeviceQueryError>,
    },
    Failed {
        error: DeviceQueryError,
        cleanup_confirmed: bool,
    },
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceQueryResult {
    pub owner_id: OwnerId,
    pub backend: OutputBackend,
    pub completed_monotonic_ns: u64,
    pub read_boundary: DeviceReadBoundary,
    pub observation: DeviceObservation,
    #[serde(skip)]
    pub(crate) retained_bytes: usize,
}
impl DeviceObservation {
    /// Validates helper data before it can enter the owner's bounded receipt history.
    pub fn retained_capacity_bytes(&self, query: &DeviceQuery) -> Result<usize, EngineError> {
        let mut bytes = size_of::<Self>();
        let mut text = |value: &str, capacity: usize| -> Result<(), EngineError> {
            if value.len() > MAX_DEVICE_TEXT_BYTES || capacity > MAX_DEVICE_TEXT_BYTES {
                return Err(DEVICE_BUDGET);
            }
            bytes = bytes.checked_add(capacity).ok_or(DEVICE_BUDGET)?;
            Ok(())
        };
        fn summary(
            value: &DeviceSummary,
            backend: OutputBackend,
            text: &mut impl FnMut(&str, usize) -> Result<(), EngineError>,
        ) -> Result<(), EngineError> {
            text(&value.endpoint, value.endpoint.capacity())?;
            for value in [&value.display_name, &value.description]
                .into_iter()
                .flatten()
            {
                text(value, value.capacity())?;
            }
            if let Some(selection) = &value.selection {
                validate_endpoint(&selection.endpoint)?;
                if selection.backend != backend
                    || selection.endpoint != value.endpoint
                    || value.selection_error.is_some()
                {
                    return Err(DEVICE_INVALID);
                }
                text(&selection.endpoint, selection.endpoint.capacity())?;
            } else if value.selection_error.is_none() {
                return Err(DEVICE_INVALID);
            }
            Ok(())
        }
        let (failures, extra) = match self {
            Self::List {
                devices,
                complete,
                failures,
            } => {
                if devices.len() > MAX_DEVICES {
                    return Err(DEVICE_BUDGET);
                }
                if query.endpoint.is_some() || *complete != failures.is_empty() {
                    return Err(DEVICE_INVALID);
                }
                if devices.windows(2).any(|v| v[0].endpoint >= v[1].endpoint) {
                    return Err(DEVICE_INVALID);
                }
                for device in devices {
                    summary(device, query.backend, &mut text)?;
                }
                (
                    failures.as_slice(),
                    devices.capacity() * size_of::<DeviceSummary>()
                        + failures.capacity() * size_of::<DeviceQueryError>(),
                )
            }
            Self::Inspection {
                endpoint,
                advertisement,
                current_properties,
                complete,
                failures,
            } => {
                if query.endpoint.as_ref() != Some(endpoint) || *complete != failures.is_empty() {
                    return Err(DEVICE_INVALID);
                }
                text(endpoint, endpoint.capacity())?;
                if let Some(advertisement) = advertisement {
                    if advertisement.endpoint != *endpoint {
                        return Err(DEVICE_INVALID);
                    }
                    summary(advertisement, query.backend, &mut text)?;
                }
                let mut extra = failures.capacity() * size_of::<DeviceQueryError>();
                if let Some(properties) = current_properties {
                    extra += size_of::<DeviceProperties>();
                    if query.backend != OutputBackend::CoreAudio
                        || properties.uid != *endpoint
                        || !properties.nominal_rate_hz.is_finite()
                    {
                        return Err(DEVICE_INVALID);
                    }
                    text(&properties.uid, properties.uid.capacity())?;
                    text(&properties.name, properties.name.capacity())?;
                    if properties.output_streams.len() > 16 {
                        return Err(DEVICE_BUDGET);
                    }
                    extra += properties.output_streams.capacity() * size_of::<DeviceStream>();
                    validate_range(&properties.buffer_frame_range)?;
                    for stream in &properties.output_streams {
                        for format in [&stream.physical, &stream.virtual_format] {
                            validate_format(format)?;
                        }
                        for formats in [&stream.available_physical, &stream.available_virtual] {
                            if formats.len() > 256 {
                                return Err(DEVICE_BUDGET);
                            }
                            extra += formats.capacity() * size_of::<DeviceRangedFormat>();
                            for format in formats {
                                validate_format(&format.format)?;
                                validate_range(&format.sample_rates)?;
                            }
                        }
                    }
                }
                (failures.as_slice(), extra)
            }
            Self::Failed {
                error,
                cleanup_confirmed,
            } => {
                if !cleanup_confirmed {
                    return Err(DEVICE_INVALID);
                }
                (std::slice::from_ref(error), 0)
            }
        };
        if failures.len() > MAX_DEVICES {
            return Err(DEVICE_BUDGET);
        }
        for error in failures {
            text(&error.message, error.message.capacity())?;
        }
        bytes = bytes.checked_add(extra).ok_or(DEVICE_BUDGET)?;
        if bytes > MAX_DEVICE_RESULT_BYTES
            || serde_json::to_vec(self).map_err(|_| DEVICE_INVALID)?.len() > MAX_DEVICE_RESULT_BYTES
        {
            return Err(DEVICE_BUDGET);
        }
        Ok(bytes)
    }
}
fn validate_format(format: &DeviceFormat) -> Result<(), EngineError> {
    if format.sample_rate_hz.is_finite() {
        Ok(())
    } else {
        Err(DEVICE_INVALID)
    }
}
fn validate_range(range: &DeviceValueRange) -> Result<(), EngineError> {
    if range.minimum.is_finite() && range.maximum.is_finite() && range.minimum <= range.maximum {
        Ok(())
    } else {
        Err(DEVICE_INVALID)
    }
}
