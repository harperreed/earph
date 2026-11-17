// ABOUTME: Checks silent exclusive ownership on an explicitly selected hardware PCM.
// ABOUTME: Keeps physical sample emission outside this diagnostic test.
#![cfg(target_os = "linux")]
use earph_alsa::{AlsaOutput, OpenRequest, Route};
use earph_core::{ErrorCode, audio::*, ids::Epochs};
mod support;

#[test]
#[ignore = "requires EARPH_ALSA_TEST_DEVICE naming one explicitly selected hardware subdevice"]
fn silent_hardware_ownership_rejects_busy_and_reopens_after_close() {
    let request = OpenRequest {
        schema_version: 1,
        device: std::env::var("EARPH_ALSA_TEST_DEVICE")
            .expect("select a hw: endpoint with an explicit SUBDEV"),
        route: Route::DirectHardware,
        format: PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap(),
        period_frames: 256,
        buffer_frames: 1024,
    };
    assert!(
        request.device.contains("SUBDEV="),
        "select one explicit hardware subdevice"
    );
    let mut first = AlsaOutput::open(request.clone(), Epochs::default()).unwrap();
    let error = match AlsaOutput::open(request.clone(), Epochs::default()) {
        Ok(_) => panic!("selected subdevice allowed a second exclusive open"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::DeviceUnavailable);
    assert_eq!(error.native_errno, Some(libc::EBUSY));
    assert_eq!(first.progress().unwrap().submitted_frames, 0);
    first.stop().unwrap();
    let mut reopened = AlsaOutput::open(request, Epochs::default()).unwrap();
    assert_eq!(reopened.progress().unwrap().submitted_frames, 0);
    reopened.stop().unwrap();
    let directory = support::directory();
    let record = serde_json::json!({"schema_version":1,"status":"pass","scope":"silent_open_busy_close_reopen",
        "build":earph_harness::build_provenance(),"report":reopened.report(),"busy_error":error,
        "signals_submitted":0,"physical_playback":"not_run"});
    std::fs::write(
        directory.join("hardware.json"),
        serde_json::to_vec_pretty(&record).unwrap(),
    )
    .unwrap();
    println!(
        "Silent hardware artifacts retained at {}",
        directory.display()
    );
}
