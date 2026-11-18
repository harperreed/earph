// ABOUTME: Checks strict filter graph intent before it reaches preparation.
// ABOUTME: Keeps correction identity and authored geometry explicit on the wire.
use earph_core::{dsp_nodes::FirAssetReference, graph::NodeKind};
use serde_json::json;

fn reference() -> serde_json::Value {
    json!({"sha256":"a".repeat(64),"sample_rate_hz":48000,"channels":"stereo","taps_per_channel":3,"purpose":"correction"})
}

#[test]
fn accepts_only_explicit_correction_references_and_filter_controls() {
    let asset: FirAssetReference = serde_json::from_value(reference()).unwrap();
    asset.validate().unwrap();
    let value = json!({"kind":"convolution","asset":reference()});
    assert_eq!(
        serde_json::to_value(serde_json::from_value::<NodeKind>(value.clone()).unwrap()).unwrap(),
        value
    );
    let peq =
        json!({"kind":"peq","filter":{"kind":"low_shelf","frequency_hz":100.0,"gain_db":-3.0}});
    assert!(serde_json::from_value::<NodeKind>(peq.clone()).is_ok());
    let mut extra = peq;
    extra["filter"]["q"] = json!(1.0);
    assert!(serde_json::from_value::<NodeKind>(extra).is_err());
    for (key, invalid) in [
        ("sha256", json!("../asset")),
        ("sha256", json!("A".repeat(64))),
        ("purpose", json!("measurement")),
        ("taps_per_channel", json!(0)),
        ("taps_per_channel", json!(262145)),
        ("sample_rate_hz", json!(0)),
    ] {
        let mut value = reference();
        value[key] = invalid;
        assert!(
            serde_json::from_value::<FirAssetReference>(value).is_err(),
            "{key}"
        );
    }
    let mut extra = reference();
    extra["path"] = json!("/tmp/taps.wav");
    assert!(serde_json::from_value::<FirAssetReference>(extra).is_err());
}
