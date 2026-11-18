// ABOUTME: Reads and translates the selected running device's SDK sample and host clocks.
// ABOUTME: Checks scheduled hardware handoff without asserting DAC presentation or preservation.
use super::super::presentation::{PayloadBuffer, SampleClock, scheduled_handoff};
use super::*;
use objc2_core_audio_types::AudioTimeStampFlags;

fn timestamp() -> AudioTimeStamp {
    // SAFETY: The SDK timestamp contains only numeric fields and accepts zero initialization.
    let mut stamp: AudioTimeStamp = unsafe { std::mem::zeroed() };
    stamp.mFlags = AudioTimeStampFlags::SampleHostTimeValid;
    stamp
}
fn observed(status: i32, stamp: AudioTimeStamp) -> Result<Option<SampleClock>, CoreAudioError> {
    if status == ca::kAudioHardwareUnsupportedOperationError {
        return Ok(None);
    }
    if status != 0 {
        return Err(CoreAudioError::native(
            "cannot observe selected device SDK clock",
            status,
        ));
    }
    if !stamp
        .mFlags
        .contains(AudioTimeStampFlags::SampleHostTimeValid)
        || !stamp.mSampleTime.is_finite()
    {
        return Ok(None);
    }
    Ok(Some(SampleClock {
        sample_frames: stamp.mSampleTime,
        host_ticks: stamp.mHostTime,
    }))
}
fn current(device: u32) -> Result<Option<SampleClock>, CoreAudioError> {
    let mut stamp = timestamp();
    // SAFETY: This read-only query uses the selected running device and owned writable timestamp.
    let status = unsafe { ca::AudioDeviceGetCurrentTime(device, NonNull::from(&mut stamp)) };
    observed(status, stamp)
}
fn translate(device: u32, sample_frames: f64) -> Result<Option<SampleClock>, CoreAudioError> {
    let mut input = timestamp();
    input.mFlags = AudioTimeStampFlags::SampleTimeValid;
    input.mSampleTime = sample_frames;
    let mut output = timestamp();
    // SAFETY: The SDK reads the owned input sample time and writes only the owned output timestamp.
    let status = unsafe {
        ca::AudioDeviceTranslateTime(
            device,
            NonNull::from(&mut input),
            NonNull::from(&mut output),
        )
    };
    observed(status, output)
}
pub(super) fn complete(
    device: u32,
    payload: PayloadBuffer,
    submitted: u64,
) -> Result<Option<bool>, CoreAudioError> {
    if payload.delivered_frames != submitted {
        return Ok(None);
    }
    let Some(current) = current(device)? else {
        return Ok(None);
    };
    let Some(start) = translate(device, payload.start.sample_frames)? else {
        return Ok(None);
    };
    let Some(end) = translate(device, payload.end_sample_frames)? else {
        return Ok(None);
    };
    let Some(translated_current) = translate(device, current.sample_frames)? else {
        return Ok(None);
    };
    Ok(scheduled_handoff(
        payload,
        submitted,
        current,
        start,
        end,
        translated_current,
    ))
}
