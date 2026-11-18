// ABOUTME: Verifies shared graph intent admission and wire contracts.
// ABOUTME: Covers finite scalar equality and bounded retained graph structures.
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, PcmRepresentation},
    graph::{
        FiniteGraphScalar, GraphDocument, GraphProcessingRequest, NodeKind, QuantizationPolicy,
    },
    ids::{NodeId, PreparedGraphId},
};
use serde::Deserialize;
use serde_json::json;

#[test]
fn node_identity_reports_retained_string_capacity_for_admission() {
    let mut text = String::with_capacity(8192);
    text.push_str("input");
    let bytes = text.capacity();
    let id = NodeId::new(text).unwrap();
    assert_eq!(id.as_str(), "input");
    assert_eq!(id.capacity_bytes(), bytes);
}

#[test]
fn structural_admission_bounds_spare_capacity_before_retaining_intent() {
    let mut graph = document();
    let mut text = String::with_capacity(128 * 1024);
    text.push_str("gain");
    graph.nodes[0].id = NodeId::new(text).unwrap();
    assert_eq!(
        graph.validate_structure().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
}

fn document() -> GraphDocument {
    serde_json::from_value(json!({
        "schema_version": 1,
        "nodes": [{
            "id": "gain",
            "kind": {"kind": "gain", "gain_linear": -0.5},
            "format": {
                "domain": "pcm", "sample_rate_hz": 48000, "channels": "stereo",
                "precision": "float64", "max_quantum_frames": 64
            }
        }],
        "edges": [],
        "output": "gain"
    }))
    .unwrap()
}

#[test]
fn scalar_rejects_nonfinite_values_from_constructors_and_deserializers() {
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(
            FiniteGraphScalar::new(value).unwrap_err().code,
            ErrorCode::GraphInvalid
        );
        let deserializer = serde::de::value::F64Deserializer::<serde::de::value::Error>::new(value);
        assert!(FiniteGraphScalar::deserialize(deserializer).is_err());
    }
    for json in ["null", "\"1.0\"", "1e400"] {
        assert!(serde_json::from_str::<FiniteGraphScalar>(json).is_err());
    }
}

#[test]
fn scalar_roundtrips_numbers_and_compares_signed_zero_by_bits() {
    for value in [-0.0, 0.0, -0.5, 0.9999999999999999, f64::MIN, f64::MAX] {
        let scalar = FiniteGraphScalar::new(value).unwrap();
        assert_eq!(scalar.get().to_bits(), value.to_bits());
        let encoded = serde_json::to_string(&scalar).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&encoded)
                .unwrap()
                .is_number()
        );
        assert_eq!(
            serde_json::from_str::<FiniteGraphScalar>(&encoded).unwrap(),
            scalar
        );
        assert_eq!(scalar, scalar);
    }
    assert_ne!(
        FiniteGraphScalar::new(0.0).unwrap(),
        FiniteGraphScalar::new(-0.0).unwrap()
    );
}

#[test]
fn shared_graph_types_keep_numeric_wire_shape_and_lawful_equality() {
    fn assert_eq_contract<T: Eq>() {}
    assert_eq_contract::<GraphDocument>();
    let graph = document();
    graph.validate_structure().unwrap();
    assert_eq!(graph.nodes[0].format.precision, PcmRepresentation::Float64);
    let encoded = serde_json::to_value(&graph).unwrap();
    assert_eq!(
        encoded["nodes"][0]["kind"],
        json!({"kind": "gain", "gain_linear": -0.5})
    );
    assert_eq!(encoded["nodes"][0]["format"]["domain"], "pcm");
    assert_eq!(
        serde_json::from_value::<GraphDocument>(encoded).unwrap(),
        graph
    );
}

#[test]
fn unit_graph_nodes_reject_ignored_processing_fields() {
    for kind in ["input", "output", "bypass", "sum", "meter"] {
        let valid = json!({"kind": kind});
        assert_eq!(
            serde_json::to_value(serde_json::from_value::<NodeKind>(valid.clone()).unwrap())
                .unwrap(),
            valid
        );
        assert!(
            serde_json::from_value::<NodeKind>(json!({"kind": kind, "gain_linear": 0.5})).is_err(),
            "{kind} must reject ignored processing fields"
        );
    }
}

#[test]
fn structural_admission_limits_typed_and_serialized_graph_sizes() {
    let mut graph = document();
    graph.nodes = vec![graph.nodes[0].clone(); 128];
    graph.validate_structure().unwrap();
    serde_json::from_value::<GraphDocument>(serde_json::to_value(&graph).unwrap()).unwrap();
    graph.nodes.push(graph.nodes[0].clone());
    assert_eq!(
        graph.validate_structure().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert!(
        serde_json::from_value::<GraphDocument>(serde_json::to_value(&graph).unwrap()).is_err()
    );
    graph = document();
    let edge = earph_core::graph::Edge {
        from: NodeId::new("in").unwrap(),
        to: NodeId::new("out").unwrap(),
        from_port: 0,
        to_port: 0,
    };
    graph.edges = vec![edge.clone(); 256];
    graph.validate_structure().unwrap();
    serde_json::from_value::<GraphDocument>(serde_json::to_value(&graph).unwrap()).unwrap();
    graph.edges.push(edge);
    assert_eq!(
        graph.validate_structure().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert!(
        serde_json::from_value::<GraphDocument>(serde_json::to_value(&graph).unwrap()).is_err()
    );
}

#[test]
fn structural_admission_requires_mono_stereo_matrix_shape() {
    for (input, output, count) in [
        (ChannelLayout::Mono, ChannelLayout::Mono, 1),
        (ChannelLayout::Mono, ChannelLayout::Stereo, 2),
        (ChannelLayout::Stereo, ChannelLayout::Mono, 2),
        (ChannelLayout::Stereo, ChannelLayout::Stereo, 4),
    ] {
        let mut graph = document();
        graph.nodes[0].format.channels = output;
        graph.nodes[0].kind = NodeKind::Matrix {
            input_channels: input,
            coefficients: vec![FiniteGraphScalar::new(0.5).unwrap(); count],
        };
        graph.validate_structure().unwrap();
        for invalid_count in [0, count + 1, 5] {
            graph.nodes[0].kind = NodeKind::Matrix {
                input_channels: input,
                coefficients: vec![FiniteGraphScalar::new(0.5).unwrap(); invalid_count],
            };
            assert_eq!(
                graph.validate_structure().unwrap_err().code,
                ErrorCode::GraphInvalid
            );
        }
        assert!(
            serde_json::from_value::<GraphDocument>(serde_json::to_value(&graph).unwrap()).is_err()
        );
    }
}

#[test]
fn graph_identities_share_opaque_id_admission() {
    for value in ["".to_owned(), "a".repeat(129), "line\nbreak".to_owned()] {
        assert_eq!(
            NodeId::new(&value).unwrap_err().code,
            ErrorCode::AssetInvalid
        );
        assert_eq!(
            PreparedGraphId::new(&value).unwrap_err().code,
            ErrorCode::AssetInvalid
        );
        assert!(serde_json::from_value::<NodeId>(json!(value)).is_err());
        assert!(serde_json::from_value::<PreparedGraphId>(json!(value)).is_err());
    }
    assert_eq!(NodeId::new("a".repeat(128)).unwrap().as_str().len(), 128);
}

#[test]
fn processing_requests_require_explicit_quantization() {
    let request = GraphProcessingRequest::Float64 {
        quantization: QuantizationPolicy::RoundTiesAwaySaturateNoDither,
    };
    assert_eq!(
        serde_json::to_value(request).unwrap(),
        json!({"kind":"float64", "quantization":"round_ties_away_saturate_no_dither"})
    );
    assert_eq!(
        serde_json::from_value::<GraphProcessingRequest>(json!({"kind":"integer_bypass"})).unwrap(),
        GraphProcessingRequest::IntegerBypass
    );
    for invalid in [
        json!({"kind":"float64"}),
        json!({"kind":"float64", "quantization":"automatic"}),
        json!({"kind":"integer_bypass", "quantization":"round_ties_away_saturate_no_dither"}),
    ] {
        assert!(serde_json::from_value::<GraphProcessingRequest>(invalid).is_err());
    }
}
