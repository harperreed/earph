// ABOUTME: Defines shared real graph intents for service and executable scenarios.
// ABOUTME: Keeps serial rate-domain fixtures consistent across their actual consumers.
use serde_json::{Value, json};

pub fn src_roundtrip() -> Value {
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":512});
    let mut middle = port.clone();
    middle["sample_rate_hz"] = json!(44100);
    json!({
        "schema_version":1,"output":"out",
        "nodes":[
            {"id":"in","kind":{"kind":"input"},"format":port},
            {"id":"down","kind":{"kind":"src","input_rate_hz":48000,"preset":"responsive","chunk_frames":64},"format":middle},
            {"id":"up","kind":{"kind":"src","input_rate_hz":44100,"preset":"responsive","chunk_frames":64},"format":port},
            {"id":"out","kind":{"kind":"output"},"format":port}
        ],
        "edges":[
            {"from":"in","to":"down","from_port":0,"to_port":0},
            {"from":"down","to":"up","from_port":0,"to_port":0},
            {"from":"up","to":"out","from_port":0,"to_port":0}
        ]
    })
}
