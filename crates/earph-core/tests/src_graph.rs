// ABOUTME: Checks strict explicit SRC graph intent through the real wire format.
// ABOUTME: Rejects ignored parameters and unknown preset names before compilation.
use earph_core::{
    dsp_nodes::SrcPreset,
    graph::{NodeKind, Transformation},
    ids::NodeId,
};

#[test]
fn src_intent_roundtrips_explicit_rate_preset_and_chunk_parameters() {
    let value = serde_json::json!({"kind":"src","input_rate_hz":48000,"preset":"quality","chunk_frames":64});
    let node: NodeKind = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(
        node,
        NodeKind::Src {
            input_rate_hz: 48000,
            preset: SrcPreset::Quality,
            chunk_frames: 64
        }
    );
    assert_eq!(serde_json::to_value(node).unwrap(), value);
    let transformation = Transformation::Src {
        node: NodeId::new("convert").unwrap(),
    };
    assert_eq!(
        serde_json::to_value(transformation).unwrap(),
        serde_json::json!({"kind":"src","node":"convert"})
    );
    for bad in [
        serde_json::json!({"kind":"src","input_rate_hz":48000,"preset":"quality","chunk_frames":64,"implicit":true}),
        serde_json::json!({"kind":"src","input_rate_hz":48000,"preset":"proprietary","chunk_frames":64}),
    ] {
        assert!(serde_json::from_value::<NodeKind>(bad).is_err());
    }
}
