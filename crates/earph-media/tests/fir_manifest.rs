// ABOUTME: Verifies FIR provenance manifests through real WAV decoding and graph consumers.
// ABOUTME: Preserves explicit preamp, exact layout, normalization, and unknown-rights metadata.
use earph_core::{
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    graph::FiniteGraphScalar,
    ids::Epochs,
};
use earph_media::{
    FirCorrectionManifest,
    correction_text::{ImportContext, ImportMetadata},
    decode_fir_wav, read_fir_manifest,
};

fn wave() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend(52_u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend(384000_u32.to_le_bytes());
    bytes.extend(8_u16.to_le_bytes());
    bytes.extend(64_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(0.5_f64.to_le_bytes());
    bytes.extend(0.0_f64.to_le_bytes());
    bytes
}
#[test]
fn manifest_roundtrip_preserves_authored_taps_and_preamp_in_actual_processing() {
    let bytes = wave();
    let correction = decode_fir_wav(&bytes).unwrap();
    let manifest = FirCorrectionManifest::new(
        correction.reference.clone(),
        bytes.len(),
        ImportContext {
            sample_rate_hz: 48000,
            channels: ChannelLayout::Mono,
            quantum_frames: 64,
        },
        ImportMetadata::default(),
        FiniteGraphScalar::new(-6.0).unwrap(),
    )
    .unwrap();
    let encoded = serde_json::to_vec(&manifest).unwrap();
    assert_eq!(read_fir_manifest(&encoded).unwrap(), manifest);
    assert_eq!(manifest.asset.taps_per_channel, 2);
    assert_eq!(manifest.channel_map, [0]);
    assert!(manifest.metadata.redistribution_license.is_none());
    let graph = manifest.graph().unwrap();
    let format = PcmFormat::new(48000, ChannelLayout::Mono, PcmRepresentation::Float64).unwrap();
    let asset = earph_dsp::FirCoefficients::new(correction.reference, correction.taps).unwrap();
    let plan = earph_dsp::compile_with_assets(
        &graph,
        &earph_dsp::CompileContext {
            input_format: format,
            output_format: format,
            quantum_frames: 64,
            policy: earph_dsp::ProcessingPolicy::AllowProcessing,
            budget: Default::default(),
        },
        &[asset],
    )
    .unwrap();
    let latency = plan.latency_frames() as usize;
    let mut prepared = earph_dsp::PreparedGraph::prepare(plan, Epochs::default()).unwrap();
    let mut input = [0.0; 64];
    input[0] = 0.1;
    let mut output = [0.0; 64];
    let mut rendered = Vec::new();
    for _ in 0..=latency / 64 {
        prepared.process(&input, &mut output).unwrap();
        rendered.extend(output);
        input.fill(0.0);
    }
    assert!(rendered[..latency].iter().all(|v| v.abs() < 1e-12));
    assert!((rendered[latency] - 0.02505936168136361).abs() < 1e-12);
}
