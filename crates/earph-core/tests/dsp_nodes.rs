// ABOUTME: Tests equalizer parameter limits and strict numeric wire contracts.
// ABOUTME: Keeps unsupported shelf controls and invalid rates out of prepared filters.
use earph_core::{dsp_nodes::PeqFilter, graph::FiniteGraphScalar};

#[test]
fn src_presets_keep_explicit_product_names_and_reject_unknown_choices() {
    use earph_core::dsp_nodes::SrcPreset;
    for (preset, wire) in [
        (SrcPreset::Quality, "\"quality\""),
        (SrcPreset::Responsive, "\"responsive\""),
    ] {
        assert_eq!(serde_json::to_string(&preset).unwrap(), wire);
        assert_eq!(serde_json::from_str::<SrcPreset>(wire).unwrap(), preset);
    }
    for wire in ["\"automatic\"", "\"proprietary_filter\"", "null", "{}"] {
        assert!(serde_json::from_str::<SrcPreset>(wire).is_err());
    }
}

fn scalar(value: f64) -> FiniteGraphScalar {
    FiniteGraphScalar::new(value).unwrap()
}

#[test]
fn peaking_round_trips_and_checks_inclusive_limits() {
    for (frequency, q, gain) in [(10.0, 0.1, -24.0), (21600.0, 20.0, 24.0)] {
        let filter = PeqFilter::Peaking {
            frequency_hz: scalar(frequency),
            q: scalar(q),
            gain_db: scalar(gain),
        };
        filter.validate(48000).unwrap();
        let wire = serde_json::to_vec(&filter).unwrap();
        assert_eq!(serde_json::from_slice::<PeqFilter>(&wire).unwrap(), filter);
    }
    for (rate, frequency, q, gain) in [
        (0, 10.0, 1.0, 0.0),
        (48000, 9.9, 1.0, 0.0),
        (48000, 21600.1, 1.0, 0.0),
        (48000, 1000.0, 0.09, 0.0),
        (48000, 1000.0, 20.1, 0.0),
        (48000, 1000.0, 1.0, -24.1),
        (48000, 1000.0, 1.0, 24.1),
    ] {
        let filter = PeqFilter::Peaking {
            frequency_hz: scalar(frequency),
            q: scalar(q),
            gain_db: scalar(gain),
        };
        assert!(filter.validate(rate).is_err(), "{filter:?} at {rate}");
    }
}

#[test]
fn shelves_have_fixed_slope_and_share_parameter_limits() {
    for kind in ["low_shelf", "high_shelf"] {
        let wire = serde_json::json!({"kind":kind,"frequency_hz":1000.0,"gain_db":6.0});
        let filter: PeqFilter = serde_json::from_value(wire.clone()).unwrap();
        filter.validate(48000).unwrap();
        assert_eq!(serde_json::to_value(filter).unwrap(), wire);
        for (key, value) in [("q", 1.0), ("slope", 1.0), ("extra", 0.0)] {
            let mut unsupported = wire.clone();
            unsupported[key] = value.into();
            assert!(serde_json::from_value::<PeqFilter>(unsupported).is_err());
        }
        let mut invalid = wire;
        invalid["frequency_hz"] = 22000.0.into();
        assert!(
            serde_json::from_value::<PeqFilter>(invalid)
                .unwrap()
                .validate(48000)
                .is_err()
        );
    }
    for invalid in [
        r#"{"kind":"peaking","frequency_hz":1000,"gain_db":6}"#,
        r#"{"kind":"peaking","frequency_hz":null,"q":1,"gain_db":6}"#,
        r#"{"kind":"low_shelf","frequency_hz":1e999,"gain_db":6}"#,
    ] {
        assert!(serde_json::from_str::<PeqFilter>(invalid).is_err());
    }
}

#[test]
fn q_shelves_have_distinct_strict_wire_kinds_and_inclusive_parameter_limits() {
    for kind in ["low_shelf_q", "high_shelf_q"] {
        for (frequency, q, gain) in [(10.0, 0.1, -24.0), (21600.0, 20.0, 24.0)] {
            let wire =
                serde_json::json!({"kind":kind,"frequency_hz":frequency,"q":q,"gain_db":gain});
            let filter: PeqFilter = serde_json::from_value(wire.clone()).unwrap();
            filter.validate(48000).unwrap();
            assert_eq!(serde_json::to_value(filter).unwrap(), wire);
        }
        let wire = serde_json::json!({"kind":kind,"frequency_hz":1000.0,"q":0.7,"gain_db":6.0});
        for (field, value) in [("slope", 1.0), ("bandwidth", 1.0), ("unknown", 0.0)] {
            let mut invalid = wire.clone();
            invalid[field] = value.into();
            assert!(serde_json::from_value::<PeqFilter>(invalid).is_err());
        }
        let mut missing = wire.clone();
        missing.as_object_mut().unwrap().remove("q");
        assert!(serde_json::from_value::<PeqFilter>(missing).is_err());
        for (rate, frequency, q, gain) in [
            (0, 10.0, 1.0, 0.0),
            (48000, 9.99, 1.0, 0.0),
            (48000, 21600.01, 1.0, 0.0),
            (48000, 1000.0, 0.099, 0.0),
            (48000, 1000.0, 20.01, 0.0),
            (48000, 1000.0, 1.0, -24.01),
            (48000, 1000.0, 1.0, 24.01),
        ] {
            let invalid =
                serde_json::json!({"kind":kind,"frequency_hz":frequency,"q":q,"gain_db":gain});
            assert!(
                serde_json::from_value::<PeqFilter>(invalid)
                    .unwrap()
                    .validate(rate)
                    .is_err()
            );
        }
    }
}
