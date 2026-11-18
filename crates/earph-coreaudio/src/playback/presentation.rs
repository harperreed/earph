// ABOUTME: Checks the SDK clock boundary for the last payload-bearing output buffer.
// ABOUTME: Rejects inconsistent clock epochs without claiming DAC presentation.
#[derive(Clone, Copy)]
pub(super) struct SampleClock {
    pub sample_frames: f64,
    pub host_ticks: u64,
}
#[derive(Clone, Copy)]
pub(super) struct PayloadBuffer {
    pub delivered_frames: u64,
    pub start: SampleClock,
    pub end_sample_frames: f64,
}
pub(super) fn scheduled_handoff(
    payload: PayloadBuffer,
    submitted: u64,
    current: SampleClock,
    translated_start: SampleClock,
    translated_end: SampleClock,
    translated_current: SampleClock,
) -> Option<bool> {
    let samples = payload.end_sample_frames - payload.start.sample_frames;
    if submitted == 0
        || payload.delivered_frames != submitted
        || !samples.is_finite()
        || !(1.0..=8192.0).contains(&samples)
        || !payload.start.sample_frames.is_finite()
        || !current.sample_frames.is_finite()
        || translated_start.sample_frames != payload.start.sample_frames
        || translated_end.sample_frames != payload.end_sample_frames
        || translated_current.sample_frames != current.sample_frames
    {
        return None;
    }
    let ticks = translated_end
        .host_ticks
        .checked_sub(translated_start.host_ticks)?;
    if ticks == 0 {
        return None;
    }
    // Permit at most one sample's host-clock rounding in the paired SDK representations.
    let tolerance = (ticks as f64 / samples).ceil() as u64;
    if payload
        .start
        .host_ticks
        .abs_diff(translated_start.host_ticks)
        > tolerance
        || current.host_ticks.abs_diff(translated_current.host_ticks) > tolerance
    {
        return None;
    }
    Some(
        current.sample_frames >= payload.end_sample_frames
            && current.host_ticks >= translated_end.host_ticks,
    )
}
#[cfg(test)]
mod tests;
