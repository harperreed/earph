// ABOUTME: Exercises a narrowly gated owned signal through the explicitly selected Core Audio device.
// ABOUTME: Keeps identities and raw SDK observations private while emitting scoped redacted evidence.
#![cfg(target_os = "macos")]
#[path = "support/hardware_signal.rs"]
mod hardware_signal;
use earph_core::{
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    ids::Epochs,
};
use earph_coreaudio::*;
use objc2_core_audio_types::{AudioTimeStamp, AudioTimeStampFlags};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    ptr::NonNull,
    time::{Duration, Instant},
};

type Check = Result<(), String>;
#[derive(Deserialize)]
struct SavedSelection {
    device_uid: String,
}
fn private_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| "cannot create a new private acceptance report".into())
}
fn write_report(file: &mut File, value: &Value) -> Check {
    file.write_all(
        &serde_json::to_vec_pretty(value).map_err(|_| "cannot serialize acceptance report")?,
    )
    .and_then(|_| file.sync_all())
    .map_err(|_| "cannot persist acceptance report".into())
}
fn device_clock(device: u32) -> Value {
    // SAFETY: The SDK timestamp has only numeric fields and accepts zero initialization.
    let mut stamp: AudioTimeStamp = unsafe { std::mem::zeroed() };
    stamp.mFlags = AudioTimeStampFlags::SampleHostTimeValid;
    // SAFETY: This read-only clock query uses the selected live device and owned writable SDK storage.
    let status =
        unsafe { objc2_core_audio::AudioDeviceGetCurrentTime(device, NonNull::from(&mut stamp)) };
    json!({"os_status":status,"host_time_ticks":(status==0 && stamp.mFlags.contains(AudioTimeStampFlags::HostTimeValid)).then_some(stamp.mHostTime),
        "sample_time_frames":(status==0 && stamp.mFlags.contains(AudioTimeStampFlags::SampleTimeValid) && stamp.mSampleTime.is_finite()).then_some(stamp.mSampleTime)})
}
fn checkpoint(output: &CoreAudioOutput, phase: &str, observations: &mut Vec<Value>) {
    observations.push(json!({"phase":phase,"progress":output.progress(),"callback_timing":output.callback_timing(),
        "device_clock":if output.state()==OutputState::Running {device_clock(output.report().observed.object_id)}else{Value::Null}}));
}
fn wait_for(
    output: &CoreAudioOutput,
    condition: impl Fn(OutputProgress) -> bool,
) -> Result<OutputProgress, String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let progress = output.progress();
        if progress.fault || progress.state != OutputState::Running {
            return Err("output faulted during native acceptance".into());
        }
        if condition(progress) {
            return Ok(progress);
        }
        if Instant::now() >= deadline {
            return Err("native acceptance exceeded its bounded callback deadline".into());
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn exercise(
    output: &mut CoreAudioOutput,
    fixture: &hardware_signal::SignalFixture,
    observations: &mut Vec<Value>,
) -> Check {
    let frames = fixture.block.header().frame_count as usize;
    if output
        .write(&fixture.block, 0, frames)
        .map_err(|error| error.to_string())?
        != (WriteProgress::Accepted { frames })
    {
        return Err("prepared ring did not admit the complete bounded signal".into());
    }
    checkpoint(output, "prepared", observations);
    output
        .start(SignalPermission::Granted)
        .map_err(|error| error.to_string())?;
    checkpoint(output, "started", observations);
    let pause = wait_for(output, |progress| {
        progress.callback_delivered_frames >= fixture.pause_at_frames
    })?;
    if pause.callback_delivered_frames >= fixture.quiet_until_frames {
        return Err("worker missed the prepared silent pause window".into());
    }
    checkpoint(output, "before_pause", observations);
    output.pause().map_err(|error| error.to_string())?;
    checkpoint(output, "paused", observations);
    let paused = output.progress();
    let timing = output.callback_timing();
    if timing.output_host_time_ticks.is_some() || timing.output_sample_time_frames.is_some() {
        return Err("pause retained an old SDK timestamp".into());
    }
    std::thread::sleep(Duration::from_millis(100));
    let held = output.progress();
    if held.callback_delivered_frames != paused.callback_delivered_frames
        || held.callbacks_observed != paused.callbacks_observed
        || held.fault
    {
        return Err("callback delivery advanced or faulted while paused".into());
    }
    checkpoint(output, "pause_held", observations);
    output
        .resume(SignalPermission::Granted)
        .map_err(|error| error.to_string())?;
    checkpoint(output, "resumed", observations);
    if output.callback_timing().generation <= timing.generation {
        return Err("resume did not invalidate its SDK clock generation".into());
    }
    output.finish().map_err(|error| error.to_string())?;
    let delivered = wait_for(output, |progress| {
        progress.callback_delivered_frames == frames as u64
    })?;
    checkpoint(output, "application_ring_drained", observations);
    wait_for(output, |progress| {
        progress.callbacks_observed >= delivered.callbacks_observed + 2
    })?;
    checkpoint(output, "two_following_callbacks", observations);
    Ok(())
}
fn settings(device: &DeviceSnapshot) -> Value {
    json!({"object_id":device.object_id,"uid":device.uid,"alive":device.alive,
        "nominal_rate_hz":device.nominal_rate_hz,"buffer_frames":device.buffer_frames,
        "hog_owner_pid":device.hog_owner_pid,"output_streams":device.output_streams})
}
fn run() -> Check {
    if std::env::var("EARPH_COREAUDIO_PLAYBACK_APPROVAL").as_deref() != Ok("granted") {
        return Err(
            "hardware acceptance requires explicit EARPH_COREAUDIO_PLAYBACK_APPROVAL=granted"
                .into(),
        );
    }
    let request_path = PathBuf::from(
        std::env::var_os("EARPH_COREAUDIO_PLAYBACK_REQUEST")
            .ok_or("missing private selected-device request path")?,
    );
    let report_path = PathBuf::from(
        std::env::var_os("EARPH_COREAUDIO_PLAYBACK_REPORT")
            .ok_or("missing private acceptance report path")?,
    );
    let selection: SavedSelection =
        serde_json::from_slice(&earph_harness::read_bounded(&request_path, 65536)?)
            .map_err(|_| "cannot read the saved explicit device selection")?;
    let before = inspect(&selection.device_uid).map_err(|error| error.to_string())?;
    if before.output_streams.len() != 1 {
        return Err("selected output no longer has one stream".into());
    }
    let virtual_format = &before.output_streams[0].virtual_format;
    let rate = virtual_format.sample_rate_hz;
    if !rate.is_finite() || rate <= 0.0 || rate.fract() != 0.0 || rate > f64::from(u32::MAX) {
        return Err("selected output no longer has a supported integer rate".into());
    }
    let channels = match virtual_format.channels_per_frame {
        1 => ChannelLayout::Mono,
        2 => ChannelLayout::Stereo,
        _ => return Err("selected output no longer has mono or stereo channels".into()),
    };
    let format = PcmFormat::new(rate as u32, channels, PcmRepresentation::Signed24)
        .map_err(|error| error.to_string())?;
    let fixture = hardware_signal::generate(format, before.buffer_frames, Epochs::default())?;
    let mut stimulus_payload = vec![
        0;
        format
            .payload_bytes(fixture.block.header().frame_count)
            .map_err(|error| error.to_string())?
    ];
    fixture
        .block
        .pack_le(&mut stimulus_payload)
        .map_err(|error| error.to_string())?;
    let stimulus_sha256 = earph_harness::sha256(&stimulus_payload);
    let request = OutputRequest {
        device_uid: selection.device_uid.clone(),
        format,
        buffer_frames: before.buffer_frames,
        ring_frames: 65536,
        route: PlaybackRoute::Float32Compatibility,
    };
    let mut raw_file = private_file(&report_path)?;
    let mut public_file = private_file(&report_path.with_extension("public.json"))?;
    let mut output =
        CoreAudioOutput::open(request, Epochs::default()).map_err(|error| error.to_string())?;
    let mut observations = Vec::new();
    let result = (|| {
        let fresh = inspect(&selection.device_uid).map_err(|error| error.to_string())?;
        if settings(&before) != settings(&fresh) {
            return Err("selected settings changed before hardware start".into());
        }
        exercise(&mut output, &fixture, &mut observations)
    })();
    let stop = output.stop().map_err(|error| error.to_string());
    checkpoint(&output, "stopped", &mut observations);
    let close = output.close().map_err(|error| error.to_string());
    checkpoint(&output, "closed", &mut observations);
    let after = inspect(&selection.device_uid).map_err(|error| error.to_string());
    let unchanged = after
        .as_ref()
        .is_ok_and(|after| settings(&before) == settings(after));
    let timestamps = observations.iter().any(|item| {
        !item["callback_timing"]["output_host_time_ticks"].is_null()
            && !item["callback_timing"]["output_sample_time_frames"].is_null()
    });
    let clock = observations.iter().any(|item| {
        item["device_clock"]["os_status"] == 0
            && !item["device_clock"]["sample_time_frames"].is_null()
    });
    let passed =
        result.is_ok() && stop.is_ok() && close.is_ok() && unchanged && timestamps && clock;
    let public = json!({"schema_version":1,"status":if passed {"pass"}else{"fail"},"route":"float32_compatibility",
        "source":"owned_generated_440hz_tones_with_linear_ramps","peak_level_dbfs_maximum":20.0*(1.0_f64/256.0).log10(),
        "format":format,"buffer_frames":before.buffer_frames,"generated_frames":fixture.block.header().frame_count,
        "stimulus_payload_encoding":"signed24_little_endian_interleaved","stimulus_payload_sha256":stimulus_sha256,
        "planned_operations":["start","pause","resume","finish","stop","close"],"final_progress":output.progress(),
        "valid_sdk_output_timestamps_observed":timestamps,"current_device_sample_clock_observed":clock,
        "pause_delivery_held":result.is_ok(),"settings_unchanged":unchanged,"hardware_presentation":"unknown","dac_preservation":"unverified",
        "build":earph_harness::build_provenance()});
    write_report(
        &mut raw_file,
        &json!({"public_evidence":public,"before":before,"after":after.as_ref().ok(),"output_report":output.report(),
        "observations":observations,"exercise_error":result.as_ref().err(),"stop_error":stop.as_ref().err(),"close_error":close.as_ref().err()}),
    )?;
    write_report(&mut public_file, &public)?;
    println!(
        "{}",
        serde_json::to_string(&public).map_err(|_| "cannot print redacted acceptance evidence")?
    );
    if !passed {
        return Err("native lifecycle acceptance failed; inspect its private report".into());
    }
    Ok(())
}
#[test]
#[ignore = "emits an owned signal; requires explicit playback approval, saved UID request, and private report paths"]
fn owned_signal_float32_lifecycle_acceptance() {
    if let Err(error) = run() {
        panic!("{error}");
    }
}
