// ABOUTME: Bridges the shared worker to explicitly selected Core Audio outputs.
// ABOUTME: Reports native formats and keeps callback delivery separate from audible position.
use super::*;
use earph_core::session::ObservedSampleRepresentation;
use earph_coreaudio::{OutputRequest, PlaybackRoute, StreamFormat};
mod activity;
#[cfg(target_os = "macos")]
mod native;
#[cfg(target_os = "macos")]
pub(super) use native::NativeOutput;

fn request(config: &OutputConfiguration, format: PcmFormat) -> Result<OutputRequest, EngineError> {
    config.validate()?;
    let route = match config.route {
        OutputRoute::DirectHardware => PlaybackRoute::StrictInteger,
        OutputRoute::System => PlaybackRoute::Float32Compatibility,
    };
    if route == PlaybackRoute::Float32Compatibility
        && config.fidelity == earph_core::safety::FidelityPolicy::StrictPreservation
    {
        return Err(EngineError::new(
            ErrorCode::FidelityPolicyConflict,
            "strict preservation rejects the float32 compatibility route",
        ));
    }
    Ok(OutputRequest {
        device_uid: config.endpoint.clone(),
        format,
        buffer_frames: config.period_frames,
        ring_frames: config.buffer_frames,
        route,
    })
}
/// Returns the latency that the device reports beyond the worker's restart cursor, at the client rate.
/// It adds the device latency, the safety offset, and the stream latency.
/// The AudioQueue cursor is the queue clock: a frame in the device IO buffer still lies past it,
/// so the counted frames cover that buffer already.
/// The HAL cursor is callback delivery: a delivered frame leaves the count and still waits one
/// IO buffer, so that route adds the buffer.
/// The frames are a device report, and the basis claims no audible position.
fn device_latency(
    latency: &earph_coreaudio::OutputLatency,
    request: &OutputRequest,
) -> Option<DeviceLatency> {
    let (with_io_buffer, basis) = match request.route {
        PlaybackRoute::Float32Compatibility => (
            false,
            "audio_queue_sample_time_plus_reported_device_latency_not_audible_position",
        ),
        PlaybackRoute::StrictInteger => (
            true,
            "core_audio_ioproc_delivery_plus_reported_device_latency_not_audible_position",
        ),
    };
    let frames = latency.frames_at(request.format.sample_rate_hz(), with_io_buffer)?;
    Some(DeviceLatency { frames, basis })
}
fn observed(format: &StreamFormat) -> ObservedStreamFormat {
    let representation = match (
        format.format_id == u32::from_be_bytes(*b"lpcm"),
        format.format_flags & 5,
        format.bits_per_channel,
    ) {
        (true, 1, 32) => ObservedSampleRepresentation::Float32,
        (true, 1, 64) => ObservedSampleRepresentation::Float64,
        (true, 4, 16) => ObservedSampleRepresentation::Signed16,
        (true, 4, 24) => ObservedSampleRepresentation::Signed24,
        (true, 4, 32) => ObservedSampleRepresentation::Signed32,
        _ => ObservedSampleRepresentation::Unknown,
    };
    ObservedStreamFormat {
        sample_rate_hz: format.sample_rate_hz,
        channels: format.channels_per_frame,
        representation,
        bytes_per_frame: format.bytes_per_frame,
        interleaved: format.format_flags & 32 == 0,
    }
}

fn backend_observation(
    report: &earph_coreaudio::OutputReport,
    audio_storage_bytes: u64,
) -> Result<BackendObservation, EngineError> {
    use sha2::{Digest, Sha256};
    let uid = &report.observed.uid;
    if uid.is_empty() || uid.len() > 1024 || !report.observed.alive {
        return Err(EngineError::new(
            ErrorCode::DeviceUnavailable,
            "opened Core Audio device has no bounded native identity",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(b"earph-native-device/core-audio/1\0");
    hash.update((uid.len() as u64).to_le_bytes());
    hash.update(uid.as_bytes());
    let digest: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let stream = report
        .observed
        .output_streams
        .first()
        .ok_or(EngineError::new(
            ErrorCode::DeviceUnavailable,
            "opened Core Audio device has no observed output stream",
        ))?;
    Ok(BackendObservation {
        backend: OutputBackend::CoreAudio,
        device_identity_sha256: Some(digest.try_into()?),
        audio_storage_bytes,
        client_format: observed(
            report
                .queue_client_format
                .as_ref()
                .unwrap_or(&stream.virtual_format),
        ),
        device_format: Some(observed(&stream.physical)),
        measurement_boundary: if report.queue_client_format.is_some() {
            "audio_queue_client_and_observed_device_formats"
        } else {
            "core_audio_observed_stream_formats"
        },
        preservation: if report.requested.route == PlaybackRoute::Float32Compatibility {
            PreservationState::Modified
        } else {
            PreservationState::Unverified
        },
    })
}

const FAULT_DIAGNOSTICS: usize = 32;
struct FaultReporter {
    sender: std::sync::OnceLock<std::sync::mpsc::SyncSender<FaultDiagnostic>>,
    initialization: std::sync::Mutex<()>,
}
impl FaultReporter {
    const fn new() -> Self {
        Self {
            sender: std::sync::OnceLock::new(),
            initialization: std::sync::Mutex::new(()),
        }
    }
    fn get(&self) -> Option<&std::sync::mpsc::SyncSender<FaultDiagnostic>> {
        self.sender.get()
    }
    fn get_or_start(
        &self,
        start: impl FnOnce() -> std::io::Result<std::sync::mpsc::SyncSender<FaultDiagnostic>>,
    ) -> std::io::Result<&std::sync::mpsc::SyncSender<FaultDiagnostic>> {
        if let Some(sender) = self.get() {
            return Ok(sender);
        }
        let _initialization = self
            .initialization
            .lock()
            .map_err(|_| std::io::Error::other("native diagnostics initialization lock failed"))?;
        if let Some(sender) = self.get() {
            return Ok(sender);
        }
        let sender = start()?;
        Ok(self.sender.get_or_init(|| sender))
    }
}
#[derive(Debug, Clone, Copy)]
struct FaultDiagnostic {
    operation: &'static str,
    error: Option<earph_coreaudio::CoreAudioError>,
    progress: Option<earph_coreaudio::OutputProgress>,
}
fn spawn_fault_reporter(
    mut writer: impl std::io::Write + Send + 'static,
) -> std::io::Result<std::sync::mpsc::SyncSender<FaultDiagnostic>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel::<FaultDiagnostic>(FAULT_DIAGNOSTICS);
    // This process-owned writer may block on stderr. Playback never waits for it or joins it.
    std::thread::Builder::new()
        .name("earph-native-diagnostics".into())
        .spawn(move || {
            for record in receiver {
                if write_fault_diagnostic(
                    &mut writer,
                    record.operation,
                    record.error,
                    record.progress,
                )
                .is_err()
                {
                    break;
                }
            }
        })?;
    Ok(sender)
}

fn write_fault_diagnostic(
    writer: &mut impl std::io::Write,
    operation: &'static str,
    error: Option<earph_coreaudio::CoreAudioError>,
    progress: Option<earph_coreaudio::OutputProgress>,
) -> std::io::Result<()> {
    let record = serde_json::json!({"backend_fault": {
        "backend":"core_audio", "operation":operation, "error":error, "progress":progress,
    }});
    let mut bytes = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{
        audio::{ChannelLayout, PcmRepresentation},
        ids::{DeviceId, ProfileId},
        safety::{FidelityPolicy, VolumeMode},
    };
    #[test]
    fn reporter_recovers_after_start_failure_and_keeps_its_successful_writer() {
        use std::{io::Read, os::unix::net::UnixStream};
        let reporter = FaultReporter::new();
        assert!(
            reporter
                .get_or_start(|| Err(std::io::Error::other("controlled start failure")))
                .is_err()
        );
        assert!(reporter.get().is_none());
        let (writer, mut reader) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let sender = reporter
            .get_or_start(|| spawn_fault_reporter(writer))
            .unwrap();
        sender
            .try_send(FaultDiagnostic {
                operation: "recovered",
                error: None,
                progress: None,
            })
            .unwrap();
        assert!(reporter.get().is_some());
        reporter
            .get_or_start(|| panic!("successful reporter must be retained"))
            .unwrap();
        drop(reporter);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["backend_fault"]["operation"], "recovered");
    }
    #[test]
    fn concurrent_reporter_start_owns_one_real_writer() {
        use std::{
            io::Read,
            os::unix::net::UnixStream,
            sync::{Arc, Mutex},
        };
        let reporter = Arc::new(FaultReporter::new());
        let (writer, mut reader) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let writer = Arc::new(Mutex::new(Some(writer)));
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let reporter = reporter.clone();
                let writer = writer.clone();
                std::thread::spawn(move || {
                    let sender = reporter
                        .get_or_start(|| {
                            spawn_fault_reporter(
                                writer.lock().unwrap().take().expect("writer started twice"),
                            )
                        })
                        .unwrap();
                    sender
                        .try_send(FaultDiagnostic {
                            operation: "concurrent",
                            error: None,
                            progress: None,
                        })
                        .unwrap();
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        drop(reporter);
        let mut bytes = String::new();
        reader.read_to_string(&mut bytes).unwrap();
        let records: Vec<serde_json::Value> = bytes
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(records.len(), 8);
        assert!(
            records
                .iter()
                .all(|record| record["backend_fault"]["operation"] == "concurrent")
        );
    }
    fn config() -> OutputConfiguration {
        OutputConfiguration {
            backend: OutputBackend::CoreAudio,
            route: OutputRoute::System,
            endpoint: "explicit-test-uid".into(),
            device_id: DeviceId::new("test").unwrap(),
            profile_id: ProfileId::new("test").unwrap(),
            period_frames: 512,
            buffer_frames: 4096,
            volume_mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
        }
    }
    fn format() -> PcmFormat {
        PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap()
    }
    #[test]
    fn opened_report_identity_uses_observed_uid_and_rejects_unbounded_identity() {
        let stream = serde_json::json!({"sample_rate_hz":48000.0,"format_id":u32::from_be_bytes(*b"lpcm"),"format_flags":12,"bytes_per_packet":6,"frames_per_packet":1,"bytes_per_frame":6,"channels_per_frame":2,"bits_per_channel":24});
        // A typed snapshot is model input, not a native-device qualification receipt.
        let observed: earph_coreaudio::DeviceSnapshot = serde_json::from_value(serde_json::json!({
            "object_id":42,"uid":"observed-test-uid","name":"Test device","alive":true,
            "nominal_rate_hz":48000.0,"nominal_rate_settable":false,"buffer_frames":512,
            "buffer_frame_range":{"minimum":64.0,"maximum":1024.0},"hog_owner_pid":null,"hog_mode_settable":null,
            "output_streams":[{"object_id":43,"physical":stream,"virtual_format":stream,"physical_settable":false,"virtual_settable":false,"available_physical":[],"available_virtual":[]}]
        })).unwrap();
        let mut report = earph_coreaudio::OutputReport {
            requested: request(&config(), format()).unwrap(),
            observed,
            evidence_level: "registered_not_started",
            payload_preservation: "unverified",
            downstream_transformations: "unknown",
            queue_client_format: None,
        };
        let first = backend_observation(&report, 256).unwrap();
        assert_eq!(
            first.device_identity_sha256.unwrap().as_str(),
            "2623c24c4ddc8f681bd127f754e73154f3d8b6f7571cfe09f65d0a360f890165"
        );
        let serialized = serde_json::to_value(&first).unwrap();
        assert!(serialized.get("uid").is_none());
        assert_eq!(
            serialized["device_identity_sha256"],
            "2623c24c4ddc8f681bd127f754e73154f3d8b6f7571cfe09f65d0a360f890165"
        );
        report.requested.device_uid = "another-requested-uid".into();
        report.observed.name = "Another display name".into();
        report.observed.nominal_rate_hz = 96000.0;
        assert_eq!(
            backend_observation(&report, 512)
                .unwrap()
                .device_identity_sha256,
            first.device_identity_sha256
        );
        report.observed.uid = "another-observed-uid".into();
        assert_ne!(
            backend_observation(&report, 512)
                .unwrap()
                .device_identity_sha256,
            first.device_identity_sha256
        );
        for invalid in [String::new(), "x".repeat(1025)] {
            report.observed.uid = invalid;
            assert_eq!(
                backend_observation(&report, 512).unwrap_err().code,
                ErrorCode::DeviceUnavailable
            );
        }
    }
    #[test]
    fn explicit_uid_and_buffer_bounds_reach_the_native_request_without_a_default_route() {
        let native = request(&config(), format()).unwrap();
        assert_eq!(native.device_uid, "explicit-test-uid");
        assert_eq!(native.buffer_frames, 512);
        assert_eq!(native.ring_frames, 4096);
        assert_eq!(native.route, PlaybackRoute::Float32Compatibility);
        let mut direct = config();
        direct.route = OutputRoute::DirectHardware;
        assert_eq!(
            request(&direct, format()).unwrap().route,
            PlaybackRoute::StrictInteger
        );
    }
    #[test]
    fn reported_device_latency_uses_the_client_rate_and_names_its_route() {
        let reported = earph_coreaudio::OutputLatency {
            nominal_rate_hz: 48000.0,
            device_frames: Some(300),
            safety_offset_frames: Some(100),
            stream_frames: Some(80),
            io_buffer_frames: 512,
        };
        let system = request(&config(), format()).unwrap();
        // The queue clock already counts the frames in the device IO buffer.
        assert_eq!(
            device_latency(&reported, &system),
            Some(DeviceLatency {
                frames: 480,
                basis: "audio_queue_sample_time_plus_reported_device_latency_not_audible_position",
            })
        );
        // A 96 kHz device counts twice the frames for the same time.
        let fast = earph_coreaudio::OutputLatency {
            nominal_rate_hz: 96000.0,
            ..reported
        };
        assert_eq!(device_latency(&fast, &system).unwrap().frames, 240);
        let mut direct = config();
        direct.route = OutputRoute::DirectHardware;
        let direct = request(&direct, format()).unwrap();
        // Callback delivery stops before the device IO buffer.
        assert_eq!(
            device_latency(&reported, &direct),
            Some(DeviceLatency {
                frames: 992,
                basis: "core_audio_ioproc_delivery_plus_reported_device_latency_not_audible_position",
            })
        );
        let unknown = earph_coreaudio::OutputLatency {
            device_frames: None,
            safety_offset_frames: None,
            stream_frames: None,
            ..reported
        };
        assert_eq!(device_latency(&unknown, &system), None);
        assert_eq!(device_latency(&unknown, &direct), None);
    }
    #[test]
    fn strict_policy_rejects_a_known_float_conversion_before_native_open() {
        let mut selected = config();
        selected.fidelity = FidelityPolicy::StrictPreservation;
        assert_eq!(
            request(&selected, format()).unwrap_err().code,
            ErrorCode::FidelityPolicyConflict
        );
        selected.route = OutputRoute::DirectHardware;
        assert!(request(&selected, format()).is_ok());
        selected.endpoint.clear();
        assert_eq!(
            request(&selected, format()).unwrap_err().code,
            ErrorCode::AssetInvalid
        );
    }
    #[test]
    fn observed_formats_preserve_the_difference_between_float_clients_and_padded_integer_devices() {
        let mut native = StreamFormat {
            sample_rate_hz: 48000.0,
            format_id: u32::from_be_bytes(*b"lpcm"),
            format_flags: 9,
            bytes_per_packet: 8,
            frames_per_packet: 1,
            bytes_per_frame: 8,
            channels_per_frame: 2,
            bits_per_channel: 32,
        };
        let client = observed(&native);
        assert_eq!(client.representation, ObservedSampleRepresentation::Float32);
        assert_eq!(client.sample_rate_hz, 48000.0);
        assert_eq!(client.channels, 2);
        assert!(client.interleaved);
        native.format_flags = 20;
        native.bits_per_channel = 24;
        let physical = observed(&native);
        assert_eq!(
            physical.representation,
            ObservedSampleRepresentation::Signed24
        );
        assert_eq!(physical.bytes_per_frame, 8);
        native.format_flags |= 32;
        assert!(!observed(&native).interleaved);
        native.format_id = 0;
        assert_eq!(
            observed(&native).representation,
            ObservedSampleRepresentation::Unknown
        );
    }

    #[test]
    fn blocked_fault_writer_cannot_block_bounded_diagnostic_submission() {
        use std::io::{Read, Write};
        use std::sync::mpsc;
        struct ObservedWriter {
            stream: std::os::unix::net::UnixStream,
            entered: Option<mpsc::SyncSender<()>>,
        }
        impl Write for ObservedWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if let Some(entered) = self.entered.take() {
                    entered.send(()).unwrap();
                }
                self.stream.write(bytes)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.stream.flush()
            }
        }
        let (mut writer, mut reader) = std::os::unix::net::UnixStream::pair().unwrap();
        writer.set_nonblocking(true).unwrap();
        loop {
            match writer.write(&[b'\n'; 8192]) {
                Ok(count) => assert!(count > 0),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("diagnostic socket fill failed: {error}"),
            }
        }
        writer.set_nonblocking(false).unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let reporter = spawn_fault_reporter(ObservedWriter {
            stream: writer,
            entered: Some(entered_tx),
        })
        .unwrap();
        let record = FaultDiagnostic {
            operation: "write",
            error: None,
            progress: None,
        };
        reporter.try_send(record).unwrap();
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        for _ in 0..FAULT_DIAGNOSTICS {
            reporter.try_send(record).unwrap();
        }
        let full = reporter.try_send(record);
        // Drain the real socket and wait for writer EOF before checking the bounded overflow.
        drop(reporter);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert!(matches!(full, Err(mpsc::TrySendError::Full(_))));
        let records: Vec<serde_json::Value> = bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(records.len(), FAULT_DIAGNOSTICS + 1);
        assert!(
            records
                .iter()
                .all(|record| record["backend_fault"]["operation"] == "write")
        );
    }

    #[test]
    fn native_fault_diagnostic_retains_first_cause_and_actual_counter_boundaries() {
        let progress = earph_coreaudio::OutputProgress {
            state: earph_coreaudio::OutputState::Failed,
            submitted_frames: 2993,
            callback_delivered_frames: 2560,
            queue_acquired_frames: None,
            queue_running_observed: None,
            queue_clock_status: None,
            queue_clock_ready: None,
            queue_clock_sample_time_bits: None,
            queue_clock_sample_time_finite: None,
            queue_clock_flags: None,
            queue_clock_discontinuity: None,
            queue_clock_observed_enqueued_frames: None,
            queue_enqueued_frames: None,
            queue_clock_last_valid_frame: None,
            queue_schedule: None,
            queue_retired_frames: None,
            queued_frames: 433,
            callbacks_observed: 6,
            measurement_boundary: "core_audio_ioproc_delivery",
            audible_position_frames: None,
            backend_delay_frames: None,
            fault: true,
            fault_detail: Some(earph_coreaudio::OutputFaultDetail {
                reason: earph_coreaudio::OutputFaultReason::RingUnderrun,
                requested_frames: Some(512),
                queued_frames: Some(433),
            }),
        };
        let failure = earph_coreaudio::CoreAudioError::new(
            ErrorCode::OutputFault,
            "output is terminal or finished; open a new stream explicitly",
        );
        let mut bytes = Vec::new();
        write_fault_diagnostic(&mut bytes, "write", Some(failure), Some(progress)).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let fault = &document["backend_fault"];
        assert_eq!(fault["backend"], "core_audio");
        assert_eq!(fault["operation"], "write");
        assert_eq!(fault["error"]["code"], "OUTPUT_FAULT");
        assert_eq!(fault["progress"]["fault_detail"]["reason"], "ring_underrun");
        assert_eq!(fault["progress"]["fault_detail"]["requested_frames"], 512);
        assert_eq!(fault["progress"]["fault_detail"]["queued_frames"], 433);
        assert_eq!(fault["progress"]["submitted_frames"], 2993);
        assert_eq!(fault["progress"]["callback_delivered_frames"], 2560);
        assert!(fault["progress"]["audible_position_frames"].is_null());
        assert!(!fault.as_object().unwrap().contains_key("device_uid"));
        assert!(!fault.as_object().unwrap().contains_key("endpoint"));
        assert_eq!(bytes.last(), Some(&b'\n'));
    }

    #[test]
    fn native_fault_diagnostic_preserves_errors_without_an_open_output() {
        let error = earph_coreaudio::CoreAudioError {
            code: ErrorCode::OutputFault,
            message: "native open failed",
            os_status: Some(-50),
        };
        let mut bytes = Vec::new();
        write_fault_diagnostic(&mut bytes, "open", Some(error), None).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["backend_fault"]["error"]["os_status"], -50);
        assert!(value["backend_fault"]["progress"].is_null());
        let (mut writer, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(peer);
        assert!(write_fault_diagnostic(&mut writer, "open", Some(error), None).is_err());
    }

    #[test]
    fn native_fault_diagnostic_keeps_stderr_records_separate() {
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "output::coreaudio::tests::native_fault_diagnostic_stderr_child",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(child.status.success(), "{child:?}");
        let records: Vec<serde_json::Value> = child
            .stderr
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                serde_json::from_slice(line).expect("stderr record must remain complete JSON")
            })
            .collect();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0]["backend_fault"]["operation"], "open");
        assert_eq!(records[1]["other_writer"], true);
    }

    #[test]
    #[ignore = "Runs only in the parent's isolated stderr subprocess"]
    fn native_fault_diagnostic_stderr_child() {
        use std::io::{self, Write};
        struct InterleavedStderr {
            wrote: bool,
        }
        impl Write for InterleavedStderr {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let count = io::stderr().write(bytes)?;
                // Schedule a real competing writer after the producer's first write.
                if !self.wrote {
                    self.wrote = true;
                    std::thread::spawn(|| io::stderr().write_all(b"{\"other_writer\":true}\n"))
                        .join()
                        .unwrap()?;
                }
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                io::stderr().flush()
            }
        }
        write_fault_diagnostic(&mut InterleavedStderr { wrote: false }, "open", None, None)
            .unwrap();
    }
}

#[cfg(test)]
mod system_format_tests {
    use super::*;
    use earph_core::{
        audio::{ChannelLayout, PcmRepresentation},
        ids::{DeviceId, ProfileId},
        safety::{FidelityPolicy, VolumeMode},
    };
    #[test]
    fn queue_client_observation_preserves_source_rate_instead_of_physical_rate() {
        let physical = serde_json::json!({"sample_rate_hz":48000.0,"format_id":u32::from_be_bytes(*b"lpcm"),"format_flags":9,"bytes_per_packet":8,"frames_per_packet":1,"bytes_per_frame":8,"channels_per_frame":2,"bits_per_channel":32});
        let snapshot = serde_json::from_value(serde_json::json!({"object_id":42,"uid":"test","name":"fixture","alive":true,"nominal_rate_hz":48000.0,"nominal_rate_settable":false,"buffer_frames":512,"buffer_frame_range":{"minimum":64.0,"maximum":1024.0},"hog_owner_pid":null,"hog_mode_settable":null,"output_streams":[{"object_id":43,"physical":physical,"virtual_format":physical,"physical_settable":false,"virtual_settable":false,"available_physical":[],"available_virtual":[]}]})).unwrap();
        let config = OutputConfiguration {
            backend: OutputBackend::CoreAudio,
            route: OutputRoute::System,
            endpoint: "test".into(),
            device_id: DeviceId::new("test").unwrap(),
            profile_id: ProfileId::new("test").unwrap(),
            period_frames: 64,
            buffer_frames: 256,
            volume_mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
        };
        let format =
            PcmFormat::new(44100, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
        let mut client: StreamFormat = serde_json::from_value(physical).unwrap();
        client.sample_rate_hz = 44100.0;
        let report = earph_coreaudio::OutputReport {
            requested: request(&config, format).unwrap(),
            observed: snapshot,
            evidence_level: "prepared_queue_client_readback",
            payload_preservation: "modified_integer_to_float32",
            downstream_transformations: "os_conversion_unverified",
            queue_client_format: Some(client),
        };
        let observation = backend_observation(&report, 4096).unwrap();
        assert_eq!(observation.client_format.sample_rate_hz, 44100.0);
        assert_eq!(
            observation.client_format.representation,
            ObservedSampleRepresentation::Float32
        );
        assert_eq!(observation.device_format.unwrap().sample_rate_hz, 48000.0);
        assert_eq!(observation.preservation, PreservationState::Modified);
        assert_eq!(observation.audio_storage_bytes, 4096);
    }
}
