// ABOUTME: Reproduces nonblocking hardware drain completion with zero-valued PCM only.
// ABOUTME: Requires explicit approval and keeps reports private without changing mixer settings.
#![cfg(target_os = "linux")]
use earph_alsa::{
    AlsaOutput, DrainProgress, OpenRequest, OutputState, SignalPermission, WriteProgress,
};
use earph_core::{
    audio::{ChannelLayout, MemoryBudget, PcmBlock, PcmRepresentation},
    ids::Epochs,
};
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    time::{Duration, Instant},
};

fn exercise(request: OpenRequest) -> Result<serde_json::Value, String> {
    if request.route != earph_alsa::Route::DirectHardware
        || request.format.sample_rate_hz() != 48000
        || request.format.channels() != ChannelLayout::Stereo
        || request.format.representation() != PcmRepresentation::Signed16
    {
        return Err(
            "the diagnostic requires the approved direct 48kHz stereo signed16 request".into(),
        );
    }
    let mut output =
        AlsaOutput::open(request.clone(), Epochs::default()).map_err(|e| e.to_string())?;
    let mut block = PcmBlock::prepare(
        request.format,
        request.period_frames as usize,
        Epochs::default(),
        MemoryBudget::default(),
    )
    .map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut submitted = 0;
    let mut sequence = 0;
    while submitted < 48000 {
        let frames = (48000 - submitted).min(u64::from(request.period_frames));
        block
            .set_span(Epochs::default(), sequence, submitted, frames)
            .map_err(|e| e.to_string())?;
        let mut offset = 0;
        while offset < frames as usize {
            if Instant::now() >= deadline {
                return Err("zero-valued hardware submission exceeded its deadline".into());
            }
            match output
                .write(
                    &block,
                    offset,
                    frames as usize - offset,
                    SignalPermission::Granted,
                )
                .map_err(|e| e.to_string())?
            {
                WriteProgress::Accepted { frames } => offset += frames,
                WriteProgress::WouldBlock => {
                    output.wait_ready(2).map_err(|e| e.to_string())?;
                }
            }
        }
        submitted += frames;
        sequence += 1;
    }
    let mut pending = 0;
    loop {
        if Instant::now() >= deadline {
            return Err("zero-valued hardware drain exceeded its deadline".into());
        }
        match output
            .finish(SignalPermission::Granted)
            .map_err(|e| e.to_string())?
        {
            DrainProgress::Complete => break,
            DrainProgress::WouldBlock => {
                pending += 1;
                if pending == 1 {
                    block
                        .set_span(Epochs::default(), sequence, 48000, 1)
                        .map_err(|e| e.to_string())?;
                    if output.state() != OutputState::Draining
                        || output
                            .write(&block, 0, 1, SignalPermission::Granted)
                            .is_ok()
                        || output.state() != OutputState::Draining
                        || output.submitted_frames() != 48000
                    {
                        return Err("draining accepted more PCM or lost its lifecycle".into());
                    }
                }
                output.wait_ready(2).map_err(|e| e.to_string())?;
            }
        }
    }
    if pending == 0 {
        return Err("hardware did not exercise asynchronous drain".into());
    }
    if output.state() != OutputState::Stopped || output.submitted_frames() != 48000 {
        return Err("drain did not retain the submitted count and stopped state".into());
    }
    Ok(
        json!({"submitted_frames":48000,"pending_drain_observations":pending,
        "state":"stopped","samples":"all_zero","digital_preservation":"unverified"}),
    )
}

#[test]
#[ignore = "requires explicit hardware approval and an approved private direct-output request"]
fn nonblocking_zero_pcm_drain_completes_without_an_io_readiness_fault() {
    assert_eq!(
        std::env::var("EARPH_ALSA_DRAIN_APPROVAL").as_deref(),
        Ok("granted"),
        "explicit hardware approval is required"
    );
    let request: OpenRequest = serde_json::from_slice(
        &fs::read(
            std::env::var_os("EARPH_ALSA_DRAIN_REQUEST")
                .expect("private output request is required"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut report = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(
            std::env::var_os("EARPH_ALSA_DRAIN_REPORT")
                .expect("new private report path is required"),
        )
        .unwrap();
    let result = exercise(request);
    let record = json!({"schema_version":1,"status":if result.is_ok(){"pass"}else{"fail"},
        "scope":"zero_valued_pcm_nonblocking_hardware_drain","outcome":result.as_ref().ok(),"error":result.as_ref().err(),
        "mixer_changes_requested":false,"fallback_attempted":false,"build":earph_harness::build_provenance()});
    report
        .write_all(&serde_json::to_vec_pretty(&record).unwrap())
        .unwrap();
    report.sync_all().unwrap();
    assert!(result.is_ok(), "{}", result.unwrap_err());
}
