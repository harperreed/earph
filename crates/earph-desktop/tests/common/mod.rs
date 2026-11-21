// ABOUTME: Hosts the real engine in owned test resources without starting any source.
// ABOUTME: Preserves runtime files and explicitly shuts down only the test-owned owner.
#![allow(dead_code)]
pub mod process;
pub mod proxy;
pub mod widgets;
use earph_core::{
    EngineError,
    control::Command,
    ids::{RequestId, ZoneId},
};
use earph_desktop::{OwnerClient, Reply, Request, StateView};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    thread::{self, JoinHandle},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "ed-{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
pub fn query(payload: Command) -> Request {
    Request::query(
        RequestId::new(unique()).unwrap(),
        ZoneId::new("main").unwrap(),
        payload,
    )
    .unwrap()
}
pub struct Owner {
    pub runtime: PathBuf,
    join: Option<JoinHandle<Result<(), EngineError>>>,
}
impl Owner {
    pub fn start() -> Self {
        Self::at(PathBuf::from("/tmp").join(unique()))
    }
    pub fn at(runtime: PathBuf) -> Self {
        let owner = earph_engine::EngineOwner::bind(&runtime).unwrap();
        Self {
            runtime,
            join: Some(thread::spawn(move || owner.serve())),
        }
    }
    pub fn client(&self) -> OwnerClient {
        OwnerClient::attach(self.runtime.to_str().unwrap().to_owned()).unwrap()
    }
    pub fn state(&self) -> StateView {
        let Reply::Receipt(receipt) = self.client().request(&query(Command::State)).unwrap() else {
            panic!("state receipt")
        };
        receipt.state().unwrap().clone()
    }
    pub fn stop(&mut self) {
        if self.join.is_none() {
            return;
        }
        let state = self.state();
        let command = Request::mutation(
            RequestId::new(unique()).unwrap(),
            &state,
            Command::OwnerShutdown,
        )
        .unwrap();
        self.client().request(&command).unwrap();
        self.join.take().unwrap().join().unwrap().unwrap();
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn profile_payload() -> Command {
    let port = serde_json::json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
    serde_json::from_value(serde_json::json!({"method":"profiles.create","id":"desktop-profile",
        "draft":{"name":"Owned desktop intent","source_policy":"local_file","fidelity":"compatible_conversion",
        "output":{"backend":"alsa","route":"system","device_id":"offline","manual_setup":"headphones"},
        "volume":{"mode":"software","software_cap_millidecibels":-20000,"hardware_cap_millidecibels":null},
        "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]},
        "graph":{"graph":{"schema_version":1,"output":"out","nodes":[{"id":"in","kind":{"kind":"input"},"format":port},{"id":"out","kind":{"kind":"output"},"format":port}],"edges":[{"from":"in","to":"out","from_port":0,"to_port":0}]},"processing":{"kind":"integer_bypass"}}})).unwrap()
}
