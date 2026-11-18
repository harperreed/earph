// ABOUTME: Separates compiler admission limits from resolved kernel identity.
// ABOUTME: Keeps format, quantum, policy, and graph changes visible in hashes.
use earph_core::audio::{ChannelLayout, PcmFormat, PcmRepresentation};
use earph_dsp::{CompileContext, GraphBudget, GraphDocument, ProcessingPolicy, compile};

#[test]
fn admission_budgets_do_not_change_semantic_kernel_hash() {
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,"output":"out",
        "nodes":[
            {"id":"in","kind":{"kind":"input"},"format":{"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64}},
            {"id":"out","kind":{"kind":"output"},"format":{"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64}}
        ],
        "edges":[{"from":"in","to":"out","from_port":0,"to_port":0}]
    })).unwrap();
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    let context = CompileContext {
        input_format: format,
        output_format: format,
        quantum_frames: 64,
        policy: ProcessingPolicy::AllowProcessing,
        budget: GraphBudget::default(),
    };
    let first = compile(&document, &context).unwrap();
    let restricted = CompileContext {
        budget: GraphBudget {
            max_buffer_bytes: 4096,
            max_latency_frames: 64,
            max_nodes: 4,
            max_edges: 4,
            max_quantum_frames: 64,
        },
        ..context
    };
    let second = compile(&document, &restricted).unwrap();
    assert_eq!(first.content_hash(), second.content_hash());
    assert_eq!(first.resources(), second.resources());
    let changed_quantum = compile(
        &document,
        &CompileContext {
            quantum_frames: 32,
            ..context
        },
    )
    .unwrap();
    assert_ne!(first.content_hash(), changed_quantum.content_hash());
    let strict = compile(
        &document,
        &CompileContext {
            policy: ProcessingPolicy::StrictPreservation,
            ..context
        },
    )
    .unwrap();
    assert_ne!(first.content_hash(), strict.content_hash());
    assert!(
        compile(
            &document,
            &CompileContext {
                budget: GraphBudget {
                    max_buffer_bytes: 1,
                    ..context.budget
                },
                ..context
            }
        )
        .is_err()
    );
}
