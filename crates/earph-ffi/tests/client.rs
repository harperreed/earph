// ABOUTME: Exercises the foreign client's real owner IPC and retained control limits.
// ABOUTME: Verifies exact receipts, stale targets, parser failures, and independent owner lifetime.
use earph_core::EngineError;
use earph_engine::{EngineOwner, request};
use earph_ffi::{ClientError, LocalClient};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn unique() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "ef-{:x}-{:x}-{:x}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}
fn envelope(id: &str, state: Option<&Value>, payload: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":1,"request_id":id,"zone_id":"main",
        "owner_id":state.map(|s|s["owner_id"].clone()),
        "session_id":state.map(|s|s["session_id"].clone()),
        "expected_revision":state.map(|s|s["revision"].clone()),"payload":payload}))
    .unwrap()
}
struct Owner {
    runtime: PathBuf,
    join: Option<JoinHandle<Result<(), EngineError>>>,
}
impl Owner {
    fn start() -> Self {
        Self::at(PathBuf::from("/tmp").join(unique()))
    }
    fn at(runtime: PathBuf) -> Self {
        let owner = EngineOwner::bind(&runtime).unwrap();
        Self {
            runtime,
            join: Some(thread::spawn(move || owner.serve())),
        }
    }
    fn client(&self) -> Arc<LocalClient> {
        LocalClient::new(self.runtime.to_str().unwrap().to_owned()).unwrap()
    }
    fn state(&self) -> Value {
        let bytes = request(
            &self.runtime,
            &envelope(&unique(), None, json!({"method":"state.get"})),
        )
        .unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()["receipt"]["result"]["value"].clone()
    }
    fn stop(&mut self) {
        if self.join.is_none() {
            return;
        }
        let state = self.state();
        let bytes = request(
            &self.runtime,
            &envelope(&unique(), Some(&state), json!({"method":"owner.shutdown"})),
        )
        .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["receipt"]["status"],
            "applied"
        );
        self.join.take().unwrap().join().unwrap().unwrap();
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}
fn roundtrip(owner: &Owner, client: &LocalClient, command: Vec<u8>) -> Value {
    let actual = client.request(command.clone()).unwrap();
    assert_eq!(actual, request(&owner.runtime, &command).unwrap());
    serde_json::from_slice(&actual).unwrap()
}
fn code(error: ClientError) -> String {
    let ClientError::Engine { code, .. } = error;
    code
}

#[test]
fn ffi_renderer_query_and_disabled_authority_preserve_the_actual_owner_state() {
    let mut owner = Owner::start();
    let client = owner.client();
    let initial = owner.state();
    let query = envelope("ffi-renderer", None, json!({"method":"renderer.get"}));
    let actual = client.request(query.clone()).unwrap();
    assert_eq!(client.request(query).unwrap(), actual);
    let value: Value = serde_json::from_slice(&actual).unwrap();
    assert_eq!(value["receipt"]["result"]["kind"], "renderer");
    let renderer: earph_core::renderer::RendererSnapshot =
        serde_json::from_value(value["receipt"]["result"]["value"].clone()).unwrap();
    assert!(!renderer.enabled);
    assert!(renderer.controllers.is_empty());
    let denied = roundtrip(
        &owner,
        &client,
        envelope(
            "ffi-renderer-grant",
            Some(&initial),
            json!({
                "method":"renderer.grants.replace","expected_generation":0,"lifetime_ms":100,
                "grants":{"controllers":[{"id":"controller-a","peers":["192.168.1.2"],"rights":["stage"]}]}
            }),
        ),
    );
    assert_eq!(denied["receipt"]["error"]["code"], "CAPABILITY_UNSUPPORTED");
    let untrusted = roundtrip(
        &owner,
        &client,
        envelope(
            "ffi-renderer-ingress",
            Some(&initial),
            json!({
                "method":"renderer.request","controller_id":"controller-a","grant_generation":1,
                "action":{"operation":"stop","binding":null}
            }),
        ),
    );
    assert_eq!(untrusted["receipt"]["error"]["code"], "AUTH_DENIED");
    assert_eq!(owner.state(), initial);
    owner.stop();
}

#[test]
fn ffi_zero_finite_exit_and_factory_defaults_share_the_actual_owner_gain_ledger() {
    let mut owner = Owner::start();
    let client = owner.client();
    let output = json!({"method":"output.select","output":{
        "backend":if cfg!(target_os="macos") {"core_audio"} else {"alsa"},
        "route":"system","endpoint":"unopened-u2-fixture","device_id":"dac","profile_id":"profile",
        "period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"
    }});
    for (id, payload) in [
        ("ffi-output", output),
        ("ffi-zero", json!({"method":"volume.silence"})),
    ] {
        let reply = roundtrip(&owner, &client, envelope(id, Some(&owner.state()), payload));
        assert_eq!(reply["receipt"]["status"], "applied", "{reply}");
    }
    let zero = owner.state();
    assert_eq!(zero["software_gain_zero"], true);
    assert_eq!(zero["software_gain_db"], -40.0);
    for method in ["signal_path.get", "events.get"] {
        let payload = if method == "events.get" {
            json!({"method":method,"cursor":null})
        } else {
            json!({"method":method})
        };
        let reply = roundtrip(&owner, &client, envelope(&unique(), None, payload));
        let report = &reply["receipt"]["result"]["value"]["report"];
        let setting = if method == "events.get" {
            &report["snapshot"]
        } else {
            &report["requested"]
        };
        assert_eq!(setting["software_gain_zero"], true, "{reply}");
    }
    let reply = roundtrip(
        &owner,
        &client,
        envelope(
            "ffi-finite",
            Some(&zero),
            json!({"method":"volume.set","millidecibels":-60000,"ramp_frames":4}),
        ),
    );
    assert_eq!(reply["receipt"]["status"], "applied");
    assert_eq!(owner.state()["software_gain_zero"], false);
    assert_eq!(owner.state()["muted"], true);
    let reply = roundtrip(
        &owner,
        &client,
        envelope(
            "ffi-preset",
            Some(&owner.state()),
            json!({"method":"volume.factory_defaults"}),
        ),
    );
    assert_eq!(reply["receipt"]["status"], "applied", "{reply}");
    assert_eq!(owner.state()["software_gain_db"], -40.0);
    assert_eq!(owner.state()["gain_ramp_frames"], 0);
    assert_eq!(owner.state()["software_gain_zero"], false);
    owner.stop();
}

#[test]
fn real_owner_commands_keep_exact_receipts_dedupe_and_stale_rejections() {
    let owner = Owner::start();
    let client = owner.client();
    let initial = owner.state();
    for method in [
        json!({"method":"state.get"}),
        json!({"method":"capabilities.get"}),
    ] {
        assert_eq!(
            roundtrip(&owner, &client, envelope(&unique(), None, method))["receipt"]["status"],
            "applied"
        );
    }
    let append = envelope(
        "append",
        Some(&initial),
        json!({"method":"queue.append","path":"owned-雪.wav"}),
    );
    let first = roundtrip(&owner, &client, append.clone());
    assert_eq!(first["receipt"]["status"], "applied");
    assert_eq!(first["receipt"]["revision"], 1);
    assert_eq!(
        client.request(append).unwrap(),
        serde_json::to_vec(&first).unwrap()
    );
    let stale = roundtrip(
        &owner,
        &client,
        envelope(
            "stale",
            Some(&initial),
            json!({"method":"queue.append","path":"other.wav"}),
        ),
    );
    assert_eq!(stale["receipt"]["error"]["code"], "STALE_REVISION");
    let page = roundtrip(
        &owner,
        &client,
        envelope(
            "queue",
            None,
            json!({"method":"queue.get","offset":0,"limit":16}),
        ),
    );
    assert_eq!(
        page["receipt"]["result"]["value"]["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        page["receipt"]["result"]["value"]["entries"][0]["path"],
        "owned-雪.wav"
    );
}

#[test]
fn malformed_bytes_and_unsupported_methods_reach_the_shared_owner_parser() {
    let owner = Owner::start();
    let client = owner.client();
    for command in [vec![0xff], b"{".to_vec(), b"null".to_vec()] {
        assert_eq!(
            roundtrip(&owner, &client, command)["error"]["code"],
            "ASSET_INVALID"
        );
    }
    let value = roundtrip(
        &owner,
        &client,
        envelope(
            "unsupported",
            Some(&owner.state()),
            json!({"method":"future.operation"}),
        ),
    );
    assert_eq!(value["error"]["code"], "CAPABILITY_UNSUPPORTED");
}

#[test]
fn frame_preflight_rejects_invalid_frames_before_absent_owner_lookup() {
    let runtime = PathBuf::from("/tmp").join(unique());
    let client = LocalClient::new(runtime.to_str().unwrap().to_owned()).unwrap();
    assert_eq!(
        code(client.request(Vec::new()).unwrap_err()),
        "RESOURCE_BUDGET_EXCEEDED"
    );
    assert_eq!(
        code(
            client
                .request(vec![b' '; earph_engine::control_frame_limit() + 1])
                .unwrap_err()
        ),
        "RESOURCE_BUDGET_EXCEEDED"
    );
    assert!(!runtime.exists());
}

#[test]
fn exact_maximum_frame_passes_through_real_ipc() {
    let owner = Owner::start();
    let client = owner.client();
    let mut command = envelope("maximum", None, json!({"method":"state.get"}));
    command.resize(earph_engine::control_frame_limit(), b' ');
    assert_eq!(
        roundtrip(&owner, &client, command)["receipt"]["status"],
        "applied"
    );
}

#[test]
fn bounded_client_storage_and_disposal_leave_owner_and_safety_unchanged() {
    let owner = Owner::start();
    let before = owner.state();
    let mut path = String::with_capacity(4 * 1024 * 1024);
    path.push_str(owner.runtime.to_str().unwrap());
    let client = LocalClient::new(path).unwrap();
    let weak = Arc::downgrade(&client);
    let mut bytes = client
        .request(envelope("copy", None, json!({"method":"state.get"})))
        .unwrap();
    bytes.fill(0);
    drop(client);
    assert!(weak.upgrade().is_none());
    let after = owner.state();
    for key in [
        "owner_id",
        "revision",
        "muted",
        "software_gain_db",
        "output",
        "output_consented",
        "volume_policy",
        "shutdown_requested",
    ] {
        assert!(
            before.as_object().unwrap().contains_key(key),
            "missing {key}"
        );
        assert!(
            after.as_object().unwrap().contains_key(key),
            "missing {key}"
        );
        assert_eq!(after[key], before[key], "{key}");
    }
    assert_eq!(after["muted"], true);
    assert_eq!(after["shutdown_requested"], false);
    let fresh = owner.client();
    assert_eq!(
        roundtrip(
            &owner,
            &fresh,
            envelope("fresh", None, json!({"method":"state.get"}))
        )["receipt"]["status"],
        "applied"
    );
    assert_eq!(
        code(LocalClient::new("é".repeat(31)).err().unwrap()),
        "RESOURCE_BUDGET_EXCEEDED"
    );
    assert_eq!(
        code(LocalClient::new(String::new()).err().unwrap()),
        "ASSET_INVALID"
    );
}

#[test]
fn a_client_cannot_retarget_an_old_owner_envelope_after_restart() {
    let mut owner = Owner::start();
    let path = owner.runtime.clone();
    let old = owner.state();
    let client = owner.client();
    let stale = envelope(
        "old-owner",
        Some(&old),
        json!({"method":"queue.append","path":"old.wav"}),
    );
    owner.stop();
    let restarted = Owner::at(path);
    assert_ne!(old["owner_id"], restarted.state()["owner_id"]);
    assert_eq!(
        roundtrip(&restarted, &client, stale)["receipt"]["error"]["code"],
        "STALE_SESSION"
    );
    assert_eq!(restarted.state()["muted"], true);
}

#[test]
fn client_transport_failures_keep_the_core_code_and_static_message() {
    let mut owner = Owner::start();
    let runtime = owner.runtime.clone();
    let client = owner.client();
    owner.stop();
    let command = envelope("disconnected", None, json!({"method":"state.get"}));
    let expected = request(&runtime, &command).unwrap_err();
    let ClientError::Engine { code, detail } = client.request(command).unwrap_err();
    assert_eq!(serde_json::to_value(expected.code).unwrap(), code);
    assert_eq!(detail, expected.message);
    assert_eq!(code, "DEVICE_UNAVAILABLE");
}

#[test]
fn accepted_profile_work_is_not_relabelled_as_terminal_by_the_binding() {
    let owner = Owner::start();
    let client = owner.client();
    let before = owner.state();
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
    let command = envelope(
        "profile",
        Some(&before),
        json!({"method":"profiles.create","id":"ffi-profile",
        "draft":{"name":"Owned FFI profile","source_policy":"local_file","fidelity":"compatible_conversion",
        "output":{"backend":"alsa","route":"system","device_id":"offline","manual_setup":"headphones"},
        "volume":{"mode":"software","software_cap_millidecibels":-20000,"hardware_cap_millidecibels":null},
        "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]},
        "graph":{"graph":{"schema_version":1,"output":"out","nodes":[{"id":"in","kind":{"kind":"input"},"format":port},{"id":"out","kind":{"kind":"output"},"format":port}],"edges":[{"from":"in","to":"out","from_port":0,"to_port":0}]},"processing":{"kind":"integer_bypass"}}}),
    );
    let first: Value = serde_json::from_slice(&client.request(command.clone()).unwrap()).unwrap();
    assert_eq!(first["receipt"]["status"], "accepted");
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let reply: Value =
            serde_json::from_slice(&client.request(command.clone()).unwrap()).unwrap();
        if reply["receipt"]["status"] != "accepted" {
            assert_eq!(reply["receipt"]["status"], "applied", "{reply}");
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
    let after = owner.state();
    for key in [
        "muted",
        "software_gain_db",
        "output",
        "output_consented",
        "volume_policy",
    ] {
        assert!(
            before.as_object().unwrap().contains_key(key),
            "missing {key}"
        );
        assert!(
            after.as_object().unwrap().contains_key(key),
            "missing {key}"
        );
        assert_eq!(before[key], after[key], "{key}");
    }
}

#[test]
fn integrated_owner_fields_and_package_failures_cross_the_same_byte_boundary() {
    let owner = Owner::start();
    let client = owner.client();
    let state = roundtrip(
        &owner,
        &client,
        envelope("t12-state", None, json!({"method":"state.get"})),
    );
    let state = &state["receipt"]["result"]["value"];
    for field in ["local_volume_policy", "configured_profile"] {
        assert!(
            state.as_object().unwrap().contains_key(field),
            "missing {field}"
        );
    }
    assert!(state["configured_profile"].is_null());
    assert!(
        state["local_volume_policy"]
            .as_object()
            .unwrap()
            .contains_key("hardware_cap_db")
    );
    let caps = roundtrip(
        &owner,
        &client,
        envelope(
            "t12-capabilities",
            None,
            json!({"method":"capabilities.get"}),
        ),
    );
    let profiles = &caps["receipt"]["result"]["value"]["profiles"];
    for field in ["exact_association_backends", "application", "package"] {
        assert!(
            profiles.as_object().unwrap().contains_key(field),
            "missing {field}"
        );
    }
    assert_eq!(
        profiles["application"]["volume_modes"],
        json!(["software", "fixed"])
    );
    assert_eq!(
        profiles["application"]["fixed_requires_current_arming"],
        true
    );
    assert_eq!(profiles["package"]["format"], "directory");
    assert_eq!(profiles["package"]["automatic_privacy_proof"], false);
    let command = envelope(
        "t12-inspect-missing",
        Some(state),
        json!({"method":"profiles.package_inspect", "directory": owner.runtime.join("absent-package")}),
    );
    let first: Value = serde_json::from_slice(&client.request(command.clone()).unwrap()).unwrap();
    assert_eq!(first["receipt"]["status"], "accepted");
    let deadline = Instant::now() + Duration::from_secs(8);
    let terminal = loop {
        let bytes = client.request(command.clone()).unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        if value["receipt"]["status"] != "accepted" {
            break bytes;
        }
        assert!(
            Instant::now() < deadline,
            "package inspection never settled"
        );
        thread::sleep(Duration::from_millis(1));
    };
    assert_eq!(terminal, request(&owner.runtime, &command).unwrap());
    assert_eq!(terminal, client.request(command).unwrap());
    let terminal: Value = serde_json::from_slice(&terminal).unwrap();
    assert_eq!(terminal["receipt"]["status"], "failed");
    assert_eq!(
        terminal["receipt"]["result"]["kind"],
        "profile_package_issue"
    );
    let issue = &terminal["receipt"]["result"]["value"];
    for field in ["reason", "object", "filesystem_state"] {
        assert!(
            issue.as_object().unwrap().contains_key(field),
            "missing package issue {field}"
        );
    }
    let after = owner.state();
    assert_eq!(after["output"], state["output"]);
    assert_eq!(after["configured_profile"], state["configured_profile"]);
    assert_eq!(after["muted"], true);
    assert_eq!(after["output_consented"], false);
}

#[test]
fn ffi_staging_replays_exact_redacted_receipts_without_starting_playback() {
    let mut owner = Owner::start();
    let client = owner.client();
    let initial = owner.state();
    let stage = envelope(
        "ffi-stage-uri",
        Some(&initial),
        json!({"method":"sources.stage_uri","uri":"http://example.invalid/?token=private-fixture","metadata":"private metadata"}),
    );
    let first = client.request(stage.clone()).unwrap();
    let receipt: Value = serde_json::from_slice(&first).unwrap();
    assert_eq!(receipt["receipt"]["status"], "applied");
    assert_eq!(receipt["receipt"]["result"]["kind"], "sources");
    assert_eq!(
        receipt["receipt"]["result"]["value"]["staged"]["media_id"],
        "staged-1"
    );
    assert_eq!(client.request(stage).unwrap(), first);
    assert!(!String::from_utf8(first).unwrap().contains("private"));
    let queried = roundtrip(
        &owner,
        &client,
        envelope("ffi-sources", None, json!({"method":"sources.get"})),
    );
    assert_eq!(queried["receipt"]["result"], receipt["receipt"]["result"]);
    let mut expected = initial;
    expected["revision"] = json!(1);
    expected["event_sequence"] = json!(1);
    assert_eq!(owner.state(), expected);
    owner.stop();
}

#[test]
fn ffi_diagnostic_reads_share_actual_owner_producer_and_strict_decoder() {
    let mut owner = Owner::start();
    let client = owner.client();
    let initial = owner.state();
    for method in [
        "signal_path.get",
        "metrics.get",
        "events.get",
        "diagnostics.preview",
    ] {
        let payload = if method == "events.get" {
            json!({"method":method,"cursor":null})
        } else {
            json!({"method":method})
        };
        let command = envelope(&format!("ffi-{method}"), None, payload);
        let bytes = client.request(command.clone()).unwrap();
        assert_eq!(client.request(command).unwrap(), bytes);
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["receipt"]["status"], "applied");
        let body = serde_json::to_vec(&value["receipt"]["result"]["value"]).unwrap();
        if method == "signal_path.get" {
            let read = earph_core::diagnostics::SignalPathRead::decode(&body).unwrap();
            assert_eq!(
                serde_json::to_value(&read.binding.owner_id).unwrap(),
                initial["owner_id"]
            );
            assert!(read.report.observed.is_none());
        } else if method == "metrics.get" {
            let read = earph_core::diagnostics::MetricsRead::decode(&body).unwrap();
            assert!(read.report.cpu_load.value.is_none());
        } else if method == "events.get" {
            let read = earph_core::diagnostics::EventsRead::decode(&body).unwrap();
            assert!(read.report.events.is_empty());
            assert!(!read.report.complete_audit_log);
        } else {
            let read = earph_core::diagnostics::PreviewRead::decode(&body).unwrap();
            let bundle =
                earph_core::diagnostics::DiagnosticBundle::decode(read.report.bytes.as_bytes())
                    .unwrap();
            assert_eq!(bundle.running_build, *earph_engine::build_provenance());
            assert!(bundle.running_build.record.is_some());
        }
    }
    assert_eq!(owner.state(), initial);
    owner.stop();
}

#[test]
fn foreign_network_control_keeps_lease_replay_private_and_performs_no_fetch() {
    use std::{
        fs,
        io::ErrorKind,
        net::{IpAddr, TcpListener, UdpSocket},
        os::unix::fs::DirBuilderExt,
    };

    let parent = PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache/efnet");
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&parent)
        .unwrap();
    let runtime = parent.join(unique());
    assert!(runtime.as_os_str().len() <= 60);
    let owner = Owner::at(runtime);
    let client = owner.client();
    let network = || {
        let reply = roundtrip(
            &owner,
            &client,
            envelope(&unique(), None, json!({"method":"sources.network.get"})),
        );
        assert_eq!(reply["receipt"]["status"], "applied");
        assert_eq!(reply["receipt"]["result"]["kind"], "network");
        reply["receipt"]["result"]["value"].clone()
    };
    let initial = network();
    assert_eq!(initial["enabled"], true);
    assert_eq!(initial["grant_generation"], 0);
    assert_eq!(initial["origin_count"], 0);
    assert!(initial["active"].is_null());
    assert!(initial["lifecycle"].is_null());

    let route = UdpSocket::bind("0.0.0.0:0").unwrap();
    // UDP connect selects an owned interface without sending a datagram.
    route.connect("192.0.2.1:9").unwrap();
    let address = route.local_addr().unwrap().ip();
    assert!(matches!(address, IpAddr::V4(ip) if ip.is_private()));
    let listener = TcpListener::bind((address, 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = format!("http://{}/", listener.local_addr().unwrap());
    let grant = envelope(
        "ffi-network-grant",
        Some(&owner.state()),
        json!({"method":"sources.grants.replace","expected_generation":0,
            "grants":{"origins":[{"origin":origin,"pins":[address],"roots_der":[]}]},
            "lifetime_ms":60000}),
    );
    let granted = client.request(grant.clone()).unwrap();
    assert_eq!(granted, request(&owner.runtime, &grant).unwrap());
    let granted_reply: Value = serde_json::from_slice(&granted).unwrap();
    assert_eq!(granted_reply["receipt"]["status"], "applied");
    assert_eq!(network()["grant_generation"], 1);
    assert_eq!(network()["origin_count"], 1);
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);

    let uri = format!("{origin}owned.flac?proof=opaque-ffi-source");
    let stage = roundtrip(
        &owner,
        &client,
        envelope(
            "ffi-network-stage",
            Some(&owner.state()),
            json!({"method":"sources.stage_uri","uri":uri,"metadata":"opaque controller metadata"}),
        ),
    );
    assert_eq!(stage["receipt"]["status"], "applied");
    let staged = roundtrip(
        &owner,
        &client,
        envelope(&unique(), None, json!({"method":"sources.get"})),
    );
    assert_eq!(
        staged["receipt"]["result"]["value"]["staged"]["admission"],
        "unadmitted"
    );
    assert!(
        !serde_json::to_string(&staged)
            .unwrap()
            .contains("opaque-ffi-source")
    );
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    let state = owner.state();
    assert!(state["source"].is_null());
    assert!(state["output"].is_null());
    assert_eq!(state["muted"], true);

    let revoked = roundtrip(
        &owner,
        &client,
        envelope(
            "ffi-network-revoke",
            Some(&state),
            json!({"method":"sources.grants.revoke","expected_generation":1}),
        ),
    );
    assert_eq!(revoked["receipt"]["status"], "applied");
    assert_eq!(network()["grant_generation"], 2);
    assert_eq!(network()["origin_count"], 0);
    assert!(network()["expires_monotonic_ns"].is_null());
    assert_eq!(client.request(grant).unwrap(), granted);
    assert_eq!(network()["grant_generation"], 2);
    assert_eq!(network()["origin_count"], 0);
    assert_eq!(listener.accept().unwrap_err().kind(), ErrorKind::WouldBlock);
    drop(client);
    assert_eq!(owner.state()["transport"], "idle");
}
