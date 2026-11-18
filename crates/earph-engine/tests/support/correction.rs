// ABOUTME: Measures retrieved correction graphs with the actual floating DSP consumer.
// ABOUTME: Supplies independent response values for the owned shelf and peaking fixtures.
use earph_core::{
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    graph::GraphDocument,
    ids::Epochs,
};

pub fn measured_gain(graph: &GraphDocument, channel: usize, frequency: f64) -> f64 {
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    let plan = earph_dsp::compile(
        graph,
        &earph_dsp::CompileContext {
            input_format: format,
            output_format: format,
            quantum_frames: 64,
            policy: earph_dsp::ProcessingPolicy::AllowProcessing,
            budget: Default::default(),
        },
    )
    .unwrap();
    let mut prepared = earph_dsp::PreparedGraph::prepare(plan, Epochs::default()).unwrap();
    let (mut input_power, mut output_power) = (0.0, 0.0);
    for start in (0..9600).step_by(64) {
        let mut input = [0.0; 128];
        for frame in 0..64 {
            input[2 * frame + channel] =
                0.1 * (std::f64::consts::TAU * frequency * (start + frame) as f64 / 48000.0).sin();
        }
        let mut output = [0.0; 128];
        prepared.process(&input, &mut output).unwrap();
        if start >= 4800 {
            for frame in 0..64 {
                input_power += input[2 * frame + channel].powi(2);
                output_power += output[2 * frame + channel].powi(2);
                assert!(output[2 * frame + 1 - channel].abs() < 1e-15);
            }
        }
    }
    10.0 * (output_power / input_power).log10()
}

pub fn shelf_response(frequency: f64) -> f64 {
    let mut result = -18.0;
    for (kind, center, q, gain) in [
        ("low", 105.0, 0.7, 6.0),
        ("peak", 1000.0, 1.0, -6.0),
        ("high", 8000.0, 1.2, -3.0),
    ] {
        let a = 10.0_f64.powf(gain / 40.0);
        let t = (std::f64::consts::PI * frequency / 48000.0).tan()
            / (std::f64::consts::PI * center / 48000.0).tan();
        let (nr, ni, dr, di) = match kind {
            "low" => (
                a * (a - t * t),
                a * a.sqrt() * t / q,
                1.0 - a * t * t,
                a.sqrt() * t / q,
            ),
            "high" => (
                a * (1.0 - a * t * t),
                a * a.sqrt() * t / q,
                a - t * t,
                a.sqrt() * t / q,
            ),
            _ => (1.0 - t * t, a * t / q, 1.0 - t * t, t / (a * q)),
        };
        result += 20.0 * (nr.hypot(ni) / dr.hypot(di)).log10();
    }
    result
}
