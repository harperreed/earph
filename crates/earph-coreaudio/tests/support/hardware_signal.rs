// ABOUTME: Prepares owned signed24 test tones below minus48 dBFS with linear ramps.
// ABOUTME: Keeps the pause window silent and the generated payload within the prepared ring.
use earph_core::{
    audio::{MemoryBudget, PcmBlock, PcmFormat, PcmRepresentation},
    ids::Epochs,
};

pub struct SignalFixture {
    pub block: PcmBlock,
    pub pause_at_frames: u64,
    pub quiet_until_frames: u64,
}
pub fn generate(
    format: PcmFormat,
    buffer_frames: u32,
    epochs: Epochs,
) -> Result<SignalFixture, &'static str> {
    if format.representation() != PcmRepresentation::Signed24
        || !(1..=8192).contains(&buffer_frames)
    {
        return Err("acceptance requires signed24 and a bounded current callback period");
    }
    let target = u64::from(format.sample_rate_hz()) * 3 / 4;
    let periods = target
        .div_ceil(u64::from(buffer_frames))
        .clamp(8, 65536 / u64::from(buffer_frames));
    let frames = periods * u64::from(buffer_frames);
    if frames > u64::from(format.sample_rate_hz()) * 2 {
        return Err("current buffer period requires an excessively long acceptance signal");
    }
    let mut block = PcmBlock::prepare(format, frames as usize, epochs, MemoryBudget::default())
        .map_err(|_| "cannot prepare owned acceptance signal")?;
    for frame in 0..frames as usize {
        let position = frame as f64 / frames as f64;
        let envelope = [(0.04, 0.22), (0.66, 0.84)]
            .into_iter()
            .map(|(start, end)| {
                ((position - start) / 0.03)
                    .min((end - position) / 0.03)
                    .clamp(0.0, 1.0)
            })
            .fold(0.0_f64, f64::max);
        let phase =
            std::f64::consts::TAU * 440.0 * frame as f64 / f64::from(format.sample_rate_hz());
        // Signed24 full scale is 8388608. A peak of 32768 is at most minus48.16 dBFS.
        let sample = (phase.sin() * 32768.0 * envelope).round() as i64;
        for channel in 0..format.channels().count() {
            block
                .set_integer_sample(frame * format.channels().count() + channel, sample)
                .map_err(|_| "cannot populate owned acceptance signal")?;
        }
    }
    Ok(SignalFixture {
        block,
        pause_at_frames: frames * 2 / 5,
        quiet_until_frames: frames * 3 / 5,
    })
}
