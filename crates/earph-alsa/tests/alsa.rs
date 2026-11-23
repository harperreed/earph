// ABOUTME: Exercises the real ALSA null and file plugins without physical audio.
// ABOUTME: Checks negotiation, byte preservation, approval, and owned stream cleanup.
#![cfg(target_os = "linux")]
use earph_alsa::*;
use earph_core::{ErrorCode, audio::*, ids::Epochs};
#[path = "../../earph-engine/tests/support/allocations.rs"]
mod allocations;
mod support;

fn capture_native_error(test: &str, expected: &str) -> bool {
    if std::env::var("EARPH_ALSA_NATIVE_TEST_CHILD").as_deref() == Ok(test) {
        return false;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("EARPH_ALSA_NATIVE_TEST_CHILD", test)
        .output()
        .unwrap();
    let artifacts = support::directory();
    std::fs::write(artifacts.join("native-error.stdout"), &output.stdout).unwrap();
    std::fs::write(artifacts.join("native-error.stderr"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{} / {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(expected),
        "expected native diagnostic: {expected}"
    );
    true
}

fn request(representation: PcmRepresentation) -> OpenRequest {
    OpenRequest {
        schema_version: 1,
        device: "null".into(),
        route: Route::System,
        format: PcmFormat::new(48000, ChannelLayout::Stereo, representation).unwrap(),
        period_frames: 64,
        buffer_frames: 256,
    }
}
fn block(request: &OpenRequest, frames: u64, position: u64, epochs: Epochs) -> PcmBlock {
    let (min, max) = request.format.representation().integer_bounds().unwrap();
    let samples = [min, max, -1, 0, 1];
    let mut value = PcmBlock::prepare(
        request.format,
        frames as usize,
        epochs,
        MemoryBudget::default(),
    )
    .unwrap();
    for index in 0..request.format.scalar_count(frames).unwrap() {
        value
            .set_integer_sample(index, samples[index % samples.len()])
            .unwrap();
    }
    value.set_span(epochs, 0, position, frames).unwrap();
    value
}

#[test]
fn installed_formats_are_exact_and_opening_never_starts_output() {
    for representation in [
        PcmRepresentation::Signed16,
        PcmRepresentation::Signed24,
        PcmRepresentation::Signed32,
    ] {
        for channels in [ChannelLayout::Mono, ChannelLayout::Stereo] {
            for rate in [44100, 48000, 96000] {
                let mut request = request(representation);
                request.format = PcmFormat::new(rate, channels, representation).unwrap();
                let mut output = AlsaOutput::open(request.clone(), Epochs::default()).unwrap();
                let report = output.report();
                assert_eq!(report.observed.format, request.format);
                assert_eq!(report.observed.period_frames, 64);
                assert_eq!(report.observed.buffer_frames, 256);
                assert_eq!(
                    u64::from(report.observed.rate_numerator),
                    u64::from(rate) * u64::from(report.observed.rate_denominator)
                );
                assert_eq!(report.pcm_type, "NULL");
                assert_eq!(report.evidence_level, "opened");
                assert_eq!(report.downstream_transformations, "unknown");
                assert_eq!(report.payload_preservation, "unverified");
                assert_eq!(output.state(), OutputState::Prepared);
                assert_eq!(output.progress().unwrap().submitted_frames, 0);
                output.stop().unwrap();
                assert_eq!(output.state(), OutputState::Stopped);
            }
        }
    }
}

#[test]
fn audio_storage_reports_the_live_packed_scratch_without_heap_work() {
    for (representation, width) in [
        (PcmRepresentation::Signed16, 2),
        (PcmRepresentation::Signed24, 3),
        (PcmRepresentation::Signed32, 4),
    ] {
        let mut output = AlsaOutput::open(request(representation), Epochs::default()).unwrap();
        let (bytes, counts) = allocations::tracked(|| output.audio_storage_bytes().unwrap());
        assert_eq!(bytes, 64 * 2 * width);
        assert_eq!(counts, allocations::AllocationCounts::default());
        output.stop().unwrap();
        assert_eq!(output.audio_storage_bytes().unwrap(), bytes);
    }
}

#[test]
fn fresh_native_timing_is_pending_before_write_and_retains_acceptance_after_stop() {
    let request = request(PcmRepresentation::Signed24);
    let epochs = Epochs::default();
    let mut output = AlsaOutput::open(request.clone(), epochs).unwrap();
    let before = output.timing_observation().unwrap();
    assert_eq!(before.pcm_state, NativePcmState::Prepared);
    assert_eq!(before.availability, TimingAvailability::Pending);
    assert_eq!(before.accepted_frames, 0);
    let data = block(&request, 64, 0, epochs);
    assert_eq!(
        output
            .write(&data, 0, 64, SignalPermission::Granted)
            .unwrap(),
        WriteProgress::Accepted { frames: 64 }
    );
    let (observed, allocations) = allocations::tracked(|| output.timing_observation().unwrap());
    assert_eq!(allocations, allocations::AllocationCounts::default());
    assert_eq!(observed.pcm_state, NativePcmState::Running);
    assert_eq!(observed.availability, TimingAvailability::Available);
    assert_eq!(observed.accepted_frames, 64);
    assert_eq!(observed.signed_delay_frames, 0);
    assert_eq!(observed.native_accuracy_ns, None);
    assert_eq!(observed.native_update_age_ns, None);
    assert_eq!(observed.timestamp_mode, before.timestamp_mode);
    assert_eq!(observed.timestamp_type, before.timestamp_type);
    output.stop().unwrap();
    assert!(output.timing_observation().is_err());
    assert_eq!(output.submitted_frames(), 64);
}

#[test]
fn signal_denial_and_stale_or_invalid_blocks_never_submit_frames() {
    let request = request(PcmRepresentation::Signed24);
    let epochs = Epochs {
        session: 1,
        stream: 2,
    };
    let mut output = AlsaOutput::open(request.clone(), epochs).unwrap();
    let valid = block(&request, 3, 0, epochs);
    assert_eq!(
        output
            .write(&valid, 0, 3, SignalPermission::Denied)
            .unwrap_err()
            .code,
        ErrorCode::SafetyPolicyBlocked
    );
    let stale = block(&request, 3, 0, Epochs::default());
    assert_eq!(
        output
            .write(&stale, 0, 3, SignalPermission::Granted)
            .unwrap_err()
            .code,
        ErrorCode::StaleSession
    );
    for (offset, frames) in [(0, 0), (3, 1), (usize::MAX, 2), (0, 4)] {
        assert!(
            output
                .write(&valid, offset, frames, SignalPermission::Granted)
                .is_err()
        );
    }
    let oversized = block(&request, 65, 0, epochs);
    assert_eq!(
        output
            .write(&oversized, 0, 65, SignalPermission::Granted)
            .unwrap_err()
            .code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut other = request.clone();
    other.format =
        PcmFormat::new(44100, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
    assert_eq!(
        output
            .write(
                &block(&other, 3, 0, epochs),
                0,
                3,
                SignalPermission::Granted
            )
            .unwrap_err()
            .code,
        ErrorCode::FormatUnsupported
    );
    assert_eq!(output.state(), OutputState::Prepared);
    assert_eq!(output.progress().unwrap().submitted_frames, 0);
}

#[test]
fn file_plugin_preserves_packed_boundaries_offsets_and_final_partial_blocks() {
    let directory = support::directory();
    for representation in [
        PcmRepresentation::Signed16,
        PcmRepresentation::Signed24,
        PcmRepresentation::Signed32,
    ] {
        let file = directory.join(format!("{representation:?}.pcm"));
        let mut request = request(representation);
        request.device = format!("file:{},raw", file.display());
        let mut output = AlsaOutput::open(request.clone(), Epochs::default()).unwrap();
        assert_eq!(output.report().pcm_type, "FILE");
        let first = block(&request, 64, 0, Epochs::default());
        assert_eq!(
            output
                .write(&first, 0, 7, SignalPermission::Granted)
                .unwrap(),
            WriteProgress::Accepted { frames: 7 }
        );
        assert_eq!(
            output
                .write(&first, 7, 57, SignalPermission::Granted)
                .unwrap(),
            WriteProgress::Accepted { frames: 57 }
        );
        let last = block(&request, 3, 64, Epochs::default());
        assert_eq!(
            output
                .write(&last, 0, 3, SignalPermission::Granted)
                .unwrap(),
            WriteProgress::Accepted { frames: 3 }
        );
        let progress = output.progress().unwrap();
        assert_eq!(progress.submitted_frames, 67);
        assert!(progress.estimated_consumed_frames <= 67);
        assert_eq!(progress.measurement_boundary, "alsa_client_estimate");
        let mut expected = vec![0; request.format.payload_bytes(67).unwrap()];
        let split = first.pack_le(&mut expected).unwrap();
        last.pack_le(&mut expected[split..]).unwrap();
        output.stop().unwrap();
        output.stop().unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), expected);
        assert_eq!(
            output
                .write(&last, 0, 3, SignalPermission::Granted)
                .unwrap_err()
                .code,
            ErrorCode::OutputFault
        );
    }
}

#[test]
fn dropping_an_output_closes_and_flushes_its_owned_file_handle() {
    let file = support::directory().join("drop.pcm");
    let mut request = request(PcmRepresentation::Signed16);
    request.device = format!("file:{},raw", file.display());
    for _ in 0..3 {
        let mut output = AlsaOutput::open(request.clone(), Epochs::default()).unwrap();
        let data = block(&request, 3, 0, Epochs::default());
        output
            .write(&data, 0, 3, SignalPermission::Granted)
            .unwrap();
        drop(output);
        assert_eq!(std::fs::metadata(&file).unwrap().len(), 12);
    }
}

#[test]
fn a_real_backend_write_error_is_terminal_without_recovery() {
    if capture_native_error(
        "a_real_backend_write_error_is_terminal_without_recovery",
        "/dev/full write failed",
    ) {
        return;
    }
    let mut request = request(PcmRepresentation::Signed32);
    request.device = "file:/dev/full,raw".into();
    let mut output = AlsaOutput::open(request.clone(), Epochs::default()).unwrap();
    let mut data = block(&request, 64, 0, Epochs::default());
    let error = (0..256)
        .find_map(|_| {
            data.set_span(Epochs::default(), 0, output.submitted_frames(), 64)
                .unwrap();
            output.write(&data, 0, 64, SignalPermission::Granted).err()
        })
        .expect("/dev/full must fail with actual native I/O");
    assert_eq!(error.code, ErrorCode::OutputFault);
    assert!(error.native_errno.is_some());
    assert_eq!(output.state(), OutputState::Failed);
    assert_eq!(
        output
            .write(&data, 0, 64, SignalPermission::Granted)
            .unwrap_err()
            .code,
        ErrorCode::OutputFault
    );
    assert_eq!(output.state(), OutputState::Failed);
    // A full device is no underrun, so no recovery reopens this output.
    assert!(!output.recover_underrun(Epochs::default()).unwrap());
    assert_eq!(output.state(), OutputState::Failed);
}

#[test]
fn a_healthy_output_reports_no_underrun_to_recover_and_keeps_its_stream() {
    let request = request(PcmRepresentation::Signed16);
    let mut output = AlsaOutput::open(request.clone(), Epochs::default()).unwrap();
    let data = block(&request, 64, 0, Epochs::default());
    output
        .write(&data, 0, 64, SignalPermission::Granted)
        .unwrap();
    let next_epoch = Epochs {
        session: 0,
        stream: 1,
    };
    assert!(!output.recover_underrun(next_epoch).unwrap());
    assert_eq!(output.state(), OutputState::Running);
    assert_eq!(output.submitted_frames(), 64);
    // The stream still belongs to its first epoch and continues its frame sequence.
    let more = block(&request, 64, 64, Epochs::default());
    output
        .write(&more, 0, 64, SignalPermission::Granted)
        .unwrap();
    assert_eq!(output.submitted_frames(), 128);
}

#[test]
fn missing_hardware_fails_without_opening_a_system_route() {
    if capture_native_error(
        "missing_hardware_fails_without_opening_a_system_route",
        "Cannot get card index",
    ) {
        return;
    }
    let mut request = request(PcmRepresentation::Signed16);
    request.route = Route::DirectHardware;
    request.device = "hw:CARD=EarphNonexistent,DEV=0".into();
    let error = match AlsaOutput::open(request, Epochs::default()) {
        Ok(_) => panic!("missing hardware opened"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::DeviceUnavailable);
    assert!(error.native_errno.is_some());
}

#[test]
fn suspend_reprepares_the_same_owned_capture_and_drain_finishes_once() {
    let test = "suspend_reprepares_the_same_owned_capture_and_drain_finishes_once";
    if std::env::var("EARPH_ALSA_TRANSPORT_TEST_CHILD").as_deref() != Ok(test) {
        let directory = support::directory();
        let file = directory.join("transport.pcm");
        let config = directory.join("alsa.conf");
        std::fs::write(&config, format!("pcm.null {{ type null }}\npcm.capture {{ type file slave.pcm null file \"{}\" format raw }}\n", file.display())).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env("EARPH_ALSA_TRANSPORT_TEST_CHILD", test)
            .env("EARPH_ALSA_TRANSPORT_CAPTURE", &file)
            .env("ALSA_CONFIG_PATH", config)
            .output()
            .unwrap();
        std::fs::write(directory.join("test.stdout"), &output.stdout).unwrap();
        std::fs::write(directory.join("test.stderr"), &output.stderr).unwrap();
        assert!(
            output.status.success(),
            "{} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let file = std::path::PathBuf::from(std::env::var_os("EARPH_ALSA_TRANSPORT_CAPTURE").unwrap());
    let mut request = request(PcmRepresentation::Signed24);
    request.device = "capture".into();
    let first_epoch = Epochs {
        session: 2,
        stream: 1,
    };
    let next_epoch = Epochs {
        session: 2,
        stream: 2,
    };
    let mut output = AlsaOutput::open(request.clone(), first_epoch).unwrap();
    let first = block(&request, 5, 0, first_epoch);
    output
        .write(&first, 0, 5, SignalPermission::Granted)
        .unwrap();
    let paused = output.suspend(next_epoch).unwrap();
    assert_eq!(paused.submitted_frames, 5);
    assert_eq!(output.submitted_frames(), 0);
    assert_eq!(output.state(), OutputState::Prepared);
    assert_eq!(
        output
            .write(&first, 0, 5, SignalPermission::Granted)
            .unwrap_err()
            .code,
        ErrorCode::StaleSession
    );
    let next = block(&request, 3, 0, next_epoch);
    output
        .write(&next, 0, 3, SignalPermission::Granted)
        .unwrap();
    assert_eq!(
        output.finish(SignalPermission::Denied).unwrap_err().code,
        ErrorCode::SafetyPolicyBlocked
    );
    for _ in 0..100 {
        if output.finish(SignalPermission::Granted).unwrap() == DrainProgress::Complete {
            break;
        }
        output.wait_ready(2).unwrap();
    }
    assert_eq!(output.state(), OutputState::Stopped);
    assert_eq!(
        output.finish(SignalPermission::Granted).unwrap(),
        DrainProgress::Complete
    );
    let mut expected = vec![0; request.format.payload_bytes(8).unwrap()];
    let split = first.pack_le(&mut expected).unwrap();
    next.pack_le(&mut expected[split..]).unwrap();
    assert_eq!(std::fs::read(file).unwrap(), expected);
}
