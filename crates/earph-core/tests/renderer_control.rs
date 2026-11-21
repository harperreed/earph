// ABOUTME: Exercises renderer grants and source ownership through canonical Session transactions.
// ABOUTME: Proves untrusted staging and scoped controls cannot steal unrelated playback.
use earph_core::{control::*, ids::*, session::Session};
use serde_json::{Value, json};

fn owner() -> Session {
    Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("renderer-owner").unwrap(),
        ControlLimits::default(),
    )
}

fn command(s: &Session, id: &str, payload: Value) -> CommandEnvelope {
    let state = s.snapshot();
    decode_command(
        &serde_json::to_vec(&json!({
            "schema_version":1,"request_id":id,"zone_id":"main",
            "owner_id":state.owner_id,"session_id":state.session_id,
            "expected_revision":state.revision,"payload":payload
        }))
        .unwrap(),
        &ControlLimits::default(),
    )
    .expect("renderer controls must use the shared strict command decoder")
}

fn grants(generation: u64, lifetime_ms: u64) -> Value {
    json!({"method":"renderer.grants.replace","expected_generation":generation,
    "lifetime_ms":lifetime_ms,"grants":{"controllers":[{
        "id":"controller-a","peers":["192.168.1.2"],"playback":null,"rights":["stage","transport"]
    }]}})
}

#[test]
fn renderer_query_is_redacted_and_default_owner_refuses_control_authority() {
    let mut s = owner();
    let query = command(&s, "renderer-query", json!({"method":"renderer.get"}));
    let reply = serde_json::to_value(s.submit(query)).unwrap();
    assert_eq!(reply["status"], "applied");
    assert_eq!(reply["result"]["kind"], "renderer");
    assert_eq!(reply["result"]["value"]["enabled"], false);
    assert_eq!(reply["result"]["value"]["grant_generation"], 0);
    assert_eq!(reply["result"]["value"]["controller_count"], 0);
    assert_eq!(reply["result"]["value"]["claim"], Value::Null);
    let grant = command(&s, "disabled-grant", grants(0, 100));
    let rejected = s.submit(grant);
    assert_eq!(rejected.status, ReceiptStatus::Rejected);
    assert_eq!(
        rejected.error.unwrap().code,
        earph_core::ErrorCode::CapabilityUnsupported
    );
    assert!(
        !serde_json::to_string(&s.checkpoint())
            .unwrap()
            .contains("controller-a")
    );
    assert!(
        serde_json::to_value(s.snapshot())
            .unwrap()
            .get("renderer")
            .is_none()
    );
}

fn submit_at(s: &mut Session, id: &str, payload: Value, now: u64) -> Receipt {
    let c = command(s, id, payload);
    s.submit_with_network_effects(
        c,
        now,
        |_| unreachable!(),
        |_, _| unreachable!(),
        |_| Ok(()),
    )
}

fn renderer(s: &mut Session, id: &str, now: u64) -> Value {
    serde_json::to_value(submit_at(s, id, json!({"method":"renderer.get"}), now)).unwrap()["result"]
        ["value"]
        .clone()
}

#[test]
fn local_grants_are_volatile_generation_fenced_and_replay_never_renews_expiry() {
    let mut s = owner();
    s.enable_renderer_control();
    let grant = command(&s, "grant", grants(0, 1));
    let first = s.submit_with_network_effects(
        grant.clone(),
        10,
        |_| unreachable!(),
        |_, _| unreachable!(),
        |_| Ok(()),
    );
    assert_eq!(first.status, ReceiptStatus::Applied);
    assert_eq!(
        renderer(&mut s, "query-grant", 10)["controllers"],
        json!([
            {"id":"controller-a","generation":1,"expires_monotonic_ns":1_000_010}
        ])
    );
    assert_eq!(
        s.submit_with_network_effects(
            grant.clone(),
            500,
            |_| unreachable!(),
            |_, _| unreachable!(),
            |_| Ok(())
        ),
        first
    );
    assert_eq!(
        renderer(&mut s, "query-expired", 1_000_010)["controller_count"],
        0
    );
    assert_eq!(
        s.submit_with_network_effects(
            grant,
            1_000_010,
            |_| unreachable!(),
            |_, _| unreachable!(),
            |_| Ok(())
        ),
        first
    );
    assert_eq!(
        renderer(&mut s, "query-no-renewal", 1_000_010)["controller_count"],
        0
    );
    let checkpoint = serde_json::to_vec(&s.checkpoint()).unwrap();
    let checkpoint_text = String::from_utf8(checkpoint.clone()).unwrap();
    assert!(!checkpoint_text.contains("controller-a"));
    assert!(!checkpoint_text.contains("192.168"));
    let mut restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("new-owner").unwrap(),
        ControlLimits::default(),
        serde_json::from_slice(&checkpoint).unwrap(),
    )
    .unwrap();
    let restored = renderer(&mut restored, "restored", 0);
    assert_eq!(restored["enabled"], false);
    assert_eq!(restored["grant_generation"], 0);
    assert_eq!(restored["controller_count"], 0);
}

fn action(controller: &str, action: Value) -> Value {
    json!({"method":"renderer.request","controller_id":controller,"grant_generation":1,"action":action})
}

#[test]
fn wire_peer_cannot_supply_trusted_renderer_ingress() {
    let mut s = owner();
    s.enable_renderer_control();
    assert_eq!(
        submit_at(&mut s, "grant", grants(0, 100), 0).status,
        ReceiptStatus::Applied
    );
    let payload = action(
        "controller-a",
        json!({"operation":"stage_uri","expected_staging_generation":0,"uri":"http://private.example/a","metadata":"private"}),
    );
    let c = command(&s, "untrusted", payload.clone());
    let before = s.snapshot();
    let rejected = s.submit(c);
    assert_eq!(rejected.status, ReceiptStatus::Rejected);
    assert_eq!(
        rejected.error.unwrap().code,
        earph_core::ErrorCode::AuthDenied
    );
    assert_eq!(s.snapshot(), before);
    for field in ["observed_peer", "trusted", "authorized"] {
        let mut forged = payload.clone();
        forged[field] = json!("192.168.1.2");
        let state = s.snapshot();
        assert!(decode_command(&serde_json::to_vec(&json!({
            "schema_version":1,"request_id":"forged","zone_id":"main","owner_id":state.owner_id,
            "session_id":state.session_id,"expected_revision":state.revision,"payload":forged
        })).unwrap(), &ControlLimits::default()).is_err());
    }
}

fn supported() -> Session {
    supported_with_rendering_control(false)
}
fn supported_with_rendering_control(rendering_control: bool) -> Session {
    supported_with_mode(rendering_control, "software")
}
fn supported_with_mode(rendering_control: bool, mode: &str) -> Session {
    let mut s = owner();
    s.enable_local_playback();
    s.enable_network_playback();
    s.enable_renderer_control();
    let mut document = grants(0, 1000);
    document["grants"]["controllers"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id":"controller-b","peers":["192.168.1.3"],"playback":null,"rights":["stage","transport","takeover"]
        }));
    if rendering_control {
        for controller in document["grants"]["controllers"].as_array_mut().unwrap() {
            controller["rights"]
                .as_array_mut()
                .unwrap()
                .push(json!("rendering_control"));
        }
    }
    for (id, payload) in [
        (
            "output",
            json!({"method":"output.select","output":{
                "backend":"alsa","route":"system","endpoint":"unopened-fixture","device_id":"dac","profile_id":"profile",
                "period_frames":64,"buffer_frames":256,"volume_mode":mode,"fidelity":"compatible_conversion"
            }}),
        ),
        (
            "consent",
            json!({"method":"output.consent","device_id":"dac","profile_id":"profile"}),
        ),
        (
            "queue",
            json!({"method":"queue.append","path":"unopened.wav"}),
        ),
    ] {
        assert_eq!(
            submit_at(&mut s, id, payload, 0).status,
            ReceiptStatus::Applied
        );
    }
    let playback = playback_authorization(&s);
    for controller in document["grants"]["controllers"].as_array_mut().unwrap() {
        controller["playback"] = playback.clone();
    }
    assert_eq!(
        submit_at(&mut s, "controllers", document, 0).status,
        ReceiptStatus::Applied
    );
    s
}

fn playback_authorization(s: &Session) -> Value {
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64});
    json!({"intent":{"graph":{"schema_version":1,"nodes":[
        {"id":"in","kind":{"kind":"input"},"format":port},
        {"id":"out","kind":{"kind":"output"},"format":port}],
        "edges":[{"from":"in","to":"out","from_port":0,"to_port":0}],"output":"out"},
        "processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}},
        "output":s.snapshot().output,"expected_device_epoch":s.snapshot().device_epoch})
}

fn renderer_request(
    s: &mut Session,
    id: &str,
    controller: &str,
    value: Value,
    now: u64,
) -> Receipt {
    let c = command(s, id, action(controller, value));
    s.submit_renderer_with_network_effects(
        c,
        if controller == "controller-a" {
            "192.168.1.2"
        } else {
            "192.168.1.3"
        }
        .parse()
        .unwrap(),
        now,
        |_, _| panic!("metadata and Stop must not admit a media fetch"),
        |_| Ok(()),
    )
}

fn stage(generation: u64, uri: &str, metadata: &str) -> Value {
    json!({"operation":"stage_uri","expected_staging_generation":generation,"uri":uri,"metadata":metadata})
}

#[test]
fn renderer_staging_and_initial_stop_preserve_unrelated_local_playback() {
    let mut s = supported();
    let mut start = command(
        &s,
        "local-start",
        json!({"method":"sessions.start","source":"local_file","device_id":"dac","takeover":false,"seekable":true}),
    );
    start.session_id = Some(SessionId::new("local-session").unwrap());
    assert_eq!(s.submit(start.clone()).status, ReceiptStatus::Accepted);
    let before = s.snapshot();
    let queue = s.queue_snapshot();
    let first = renderer_request(
        &mut s,
        "stage-a",
        "controller-a",
        stage(
            0,
            "http://private.example/no-fetch?secret=a",
            "private-metadata",
        ),
        0,
    );
    assert_eq!(first.status, ReceiptStatus::Applied);
    assert_eq!(
        renderer_request(
            &mut s,
            "ma-initial-stop",
            "controller-a",
            json!({"operation":"stop","binding":null}),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    let mut expected = before;
    expected.revision += 1;
    expected.event_sequence += 1;
    assert_eq!(s.snapshot(), expected);
    assert_eq!(s.queue_snapshot(), queue);
    assert_eq!(s.submit(start).status, ReceiptStatus::Accepted);
    let snapshot = renderer(&mut s, "stage-query", 0);
    assert_eq!(
        snapshot["staged"],
        json!({"controller_id":"controller-a","grant_generation":1,"media_id":"staged-1","generation":1})
    );
    for bytes in [
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&s.checkpoint()).unwrap(),
        format!(
            "{:?}",
            s.staged_uri_descriptor(&StagedMediaId::new("staged-1").unwrap(), 1)
                .unwrap()
        ),
    ] {
        assert!(!bytes.contains("secret=a"));
        assert!(!bytes.contains("private-metadata"));
    }
    let c = command(
        &s,
        "stage-repeat",
        action(
            "controller-a",
            stage(
                1,
                "http://private.example/no-fetch?secret=a",
                "private-metadata",
            ),
        ),
    );
    let repeated = s.submit_renderer_with_network_effects(
        c.clone(),
        "192.168.1.2".parse().unwrap(),
        0,
        |_, _| unreachable!(),
        |_| Ok(()),
    );
    assert_eq!(repeated.status, ReceiptStatus::Applied);
    assert_eq!(s.snapshot(), expected);
    let wrong_peer = s.submit_renderer_with_network_effects(
        c.clone(),
        "192.168.1.4".parse().unwrap(),
        0,
        |_, _| unreachable!(),
        |_| Ok(()),
    );
    assert_eq!(
        wrong_peer.error.unwrap().code,
        earph_core::ErrorCode::AuthDenied
    );
    assert_eq!(
        s.submit(c).error.unwrap().code,
        earph_core::ErrorCode::AuthDenied
    );
    assert_eq!(s.snapshot(), expected);
    assert_eq!(
        renderer_request(&mut s, "clear", "controller-a", stage(1, "", ""), 0).status,
        ReceiptStatus::Applied
    );
    assert_eq!(renderer(&mut s, "cleared", 0)["staged"], Value::Null);
}

fn media_grants(s: &mut Session) {
    media_grants_with_lifetime(s, 60000);
}

fn media_grants_with_lifetime(s: &mut Session, lifetime_ms: u64) {
    let c = command(
        s,
        "media-grants",
        json!({"method":"sources.grants.replace","expected_generation":0,"lifetime_ms":lifetime_ms,
        "grants":{"origins":[{"origin":"http://private.example:80","pins":["192.168.1.2"],"roots_der":[]}]}}),
    );
    assert_eq!(
        s.submit_with_network_effects(c, 0, |_| unreachable!(), |_, _| Ok(()), |_| Ok(()))
            .status,
        ReceiptStatus::Applied
    );
}

fn play_command(s: &Session, controller: &str, id: &str, generation: u64) -> CommandEnvelope {
    let mut c = command(
        s,
        id,
        action(
            controller,
            json!({"operation":"play","media_id":format!("staged-{generation}"),"staging_generation":generation,"grant_generation":1,"device_id":"dac"}),
        ),
    );
    c.session_id = Some(SessionId::new(id).unwrap());
    c
}

fn play(s: &mut Session, controller: &str, id: &str, generation: u64) -> Receipt {
    let c = play_command(s, controller, id, generation);
    s.submit_renderer_with_network_effects(
        c,
        if controller == "controller-a" {
            "192.168.1.2"
        } else {
            "192.168.1.3"
        }
        .parse()
        .unwrap(),
        0,
        |candidate, lowered| {
            assert!(matches!(
                lowered.payload,
                Command::NetworkStartHttpFlac { .. }
            ));
            let source = candidate.network_source().unwrap();
            assert!(candidate.network_authority_valid(&source.binding, 0));
            Ok(())
        },
        |_| Ok(()),
    )
}

fn retirement_ack(s: &Session) -> earph_core::network_source::NetworkRetirementAck {
    let closing = s.closing_network_source().unwrap();
    earph_core::network_source::NetworkRetirementAck {
        binding: closing.context.binding.clone(),
        source_epochs: closing.context.source_epochs,
        output_epochs: closing.output_epochs,
        graph_epoch: closing.graph_epoch,
        device_epoch: closing.device_epoch,
        observation: None,
    }
}

fn retire(s: &mut Session) {
    s.complete_network_retirement(retirement_ack(s), 0).unwrap();
}

#[test]
fn another_controllers_stage_survives_exact_stop_revoke_and_retirement() {
    for revoke in [false, true] {
        let mut s = supported();
        media_grants(&mut s);
        assert_eq!(
            renderer_request(
                &mut s,
                "a-stage",
                "controller-a",
                stage(0, "http://private.example/a", "a"),
                0
            )
            .status,
            ReceiptStatus::Applied
        );
        assert_eq!(
            play(&mut s, "controller-a", "a-play", 1).status,
            ReceiptStatus::Accepted
        );
        let a = s.network_source().unwrap().binding.clone();
        assert_eq!(
            renderer_request(
                &mut s,
                "b-stage",
                "controller-b",
                stage(1, "http://private.example/b", "b"),
                0
            )
            .status,
            ReceiptStatus::Applied
        );
        assert_eq!(s.network_source().unwrap().binding, a);
        let before = renderer(&mut s, "before-stop", 0);
        assert_eq!(before["active"]["controller_id"], "controller-a");
        assert_eq!(before["staged"]["controller_id"], "controller-b");
        let r = if revoke {
            submit_at(
                &mut s,
                "revoke-a",
                json!({"method":"renderer.grants.revoke","expected_generation":1,"controller_id":"controller-a"}),
                0,
            )
        } else {
            renderer_request(
                &mut s,
                "stop-a",
                "controller-a",
                json!({"operation":"stop","binding":a}),
                0,
            )
        };
        assert_eq!(r.status, ReceiptStatus::Accepted);
        let retiring = renderer(&mut s, "retiring", 0);
        assert_eq!(retiring["closing"]["controller_id"], "controller-a");
        assert_eq!(retiring["active"], Value::Null);
        assert_eq!(retiring["staged"], before["staged"]);
        let a_ack = retirement_ack(&s);
        let mut stale = a_ack.clone();
        stale.binding.source_generation += 1;
        assert_eq!(
            s.complete_network_retirement(stale, 0).unwrap_err().code,
            earph_core::ErrorCode::StaleSession
        );
        s.complete_network_retirement(a_ack.clone(), 0).unwrap();
        let after_a = s.snapshot();
        assert_eq!(
            s.complete_network_retirement(a_ack, 0).unwrap_err().code,
            earph_core::ErrorCode::StaleSession
        );
        assert_eq!(s.snapshot(), after_a);
        let after = renderer(&mut s, "retired", 0);
        assert_eq!(after["closing"], Value::Null);
        assert_eq!(after["staged"], before["staged"]);
        assert_eq!(
            play(&mut s, "controller-b", "b-play", 2).status,
            ReceiptStatus::Accepted
        );
    }
}

#[test]
fn controller_expiry_retires_only_its_exact_source_on_the_existing_clock() {
    let mut s = supported();
    media_grants(&mut s);
    assert_eq!(
        renderer_request(
            &mut s,
            "stage",
            "controller-a",
            stage(0, "http://private.example/a", ""),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    assert_eq!(
        play(&mut s, "controller-a", "play", 1).status,
        ReceiptStatus::Accepted
    );
    assert_eq!(
        s.network_source().unwrap().expires_monotonic_ns,
        1_000_000_000
    );
    s.expire_network_authority(1_000_000_000).unwrap();
    assert!(s.network_source().is_none());
    assert!(s.closing_network_source().is_some());
    assert!(s.snapshot().muted);
    assert_eq!(s.renderer_snapshot().controller_count, 0);
    assert!(
        s.network_grants().is_some(),
        "controller expiry must not revoke separate media grants"
    );
    let c = command(
        &s,
        "expired",
        action("controller-a", stage(1, "http://private.example/b", "")),
    );
    let r = s.submit_renderer_with_network_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        1_000_000_000,
        |_, _| unreachable!(),
        |_| Ok(()),
    );
    assert_eq!(r.error.unwrap().code, earph_core::ErrorCode::AuthDenied);
}

#[test]
fn explicit_takeover_preserves_the_replacements_ownership_after_outgoing_retirement() {
    let mut s = supported();
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "a-stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "a-play", 1).status,
        ReceiptStatus::Accepted
    );
    let a = s.network_source().unwrap().binding.clone();
    renderer_request(
        &mut s,
        "b-stage",
        "controller-b",
        stage(1, "http://private.example/b", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-b", "b-takeover", 2).status,
        ReceiptStatus::Accepted
    );
    let b = s.network_source().unwrap().binding.clone();
    assert_ne!(a, b);
    let before = s.renderer_snapshot();
    assert_eq!(
        before.active.as_ref().unwrap().controller_id.as_str(),
        "controller-b"
    );
    assert_eq!(
        before.closing.as_ref().unwrap().controller_id.as_str(),
        "controller-a"
    );
    retire(&mut s);
    assert_eq!(s.renderer_snapshot().active, before.active);
    assert_eq!(s.renderer_snapshot().staged, before.staged);
    assert!(s.renderer_snapshot().closing.is_none());
    let snapshot = s.snapshot();
    let stale = renderer_request(
        &mut s,
        "old-a-stop",
        "controller-a",
        json!({"operation":"stop","binding":a}),
        0,
    );
    assert_eq!(
        stale.error.unwrap().code,
        earph_core::ErrorCode::StaleSession
    );
    assert_eq!(s.snapshot(), snapshot);
    assert_eq!(s.network_source().unwrap().binding, b);
}

#[test]
fn local_staging_invalidates_only_association_and_missing_media_grants_never_accept_play() {
    let mut s = supported();
    renderer_request(
        &mut s,
        "a-stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    let before = s.snapshot();
    let rejected = play(&mut s, "controller-a", "no-origin-grant", 1);
    assert_eq!(rejected.status, ReceiptStatus::Rejected);
    assert_eq!(
        rejected.error.unwrap().code,
        earph_core::ErrorCode::AuthDenied
    );
    assert_eq!(s.snapshot(), before);
    media_grants(&mut s);
    assert_eq!(
        play(&mut s, "controller-a", "a-play", 1).status,
        ReceiptStatus::Accepted
    );
    let owned = s.renderer_snapshot().active;
    let local = command(
        &s,
        "local-stage",
        json!({"method":"sources.stage_uri","uri":"untrusted:local","metadata":"local"}),
    );
    assert_eq!(s.submit(local).status, ReceiptStatus::Applied);
    assert!(s.renderer_snapshot().staged.is_none());
    assert_eq!(s.renderer_snapshot().active, owned);
    let before = s.snapshot();
    assert_eq!(
        play(&mut s, "controller-a", "stale-stage-play", 1)
            .error
            .unwrap()
            .code,
        earph_core::ErrorCode::StaleSession
    );
    assert_eq!(s.snapshot(), before);
}

#[test]
fn revision_owner_session_and_staging_races_preserve_the_first_association() {
    let mut s = supported();
    let a = command(
        &s,
        "a-stage",
        action("controller-a", stage(0, "http://private.example/a", "")),
    );
    let b = command(
        &s,
        "b-stage",
        action("controller-b", stage(0, "http://private.example/b", "")),
    );
    assert_eq!(
        s.submit_renderer_with_network_effects(
            a,
            "192.168.1.2".parse().unwrap(),
            0,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Applied
    );
    let before = s.snapshot();
    let stable = s.renderer_snapshot().staged;
    assert_eq!(
        s.submit_renderer_with_network_effects(
            b,
            "192.168.1.3".parse().unwrap(),
            0,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .error
        .unwrap()
        .code,
        earph_core::ErrorCode::StaleRevision
    );
    for fault in ["owner", "session", "staging", "generation"] {
        let mut c = command(
            &s,
            fault,
            action("controller-a", stage(1, "http://private.example/c", "")),
        );
        let expected = match fault {
            "owner" => {
                c.owner_id = Some(OwnerId::new("old-owner").unwrap());
                earph_core::ErrorCode::StaleSession
            }
            "session" => {
                c.session_id = Some(SessionId::new("old-session").unwrap());
                earph_core::ErrorCode::StaleSession
            }
            "staging" => {
                if let Command::RendererRequest {
                    action:
                        earph_core::renderer::RendererAction::StageUri {
                            expected_staging_generation,
                            ..
                        },
                    ..
                } = &mut c.payload
                {
                    *expected_staging_generation = 0;
                }
                earph_core::ErrorCode::StaleSession
            }
            _ => {
                if let Command::RendererRequest {
                    grant_generation, ..
                } = &mut c.payload
                {
                    *grant_generation = 0;
                }
                earph_core::ErrorCode::AuthDenied
            }
        };
        assert_eq!(
            s.submit_renderer_with_network_effects(
                c,
                "192.168.1.2".parse().unwrap(),
                0,
                |_, _| unreachable!(),
                |_| Ok(())
            )
            .error
            .unwrap()
            .code,
            expected
        );
    }
    assert_eq!(s.snapshot(), before);
    assert_eq!(s.renderer_snapshot().staged, stable);
}

#[test]
fn failed_commit_and_fetch_policy_leave_staging_and_source_authority_unchanged() {
    let mut s = supported();
    let before = s.snapshot();
    let c = command(
        &s,
        "commit-fails",
        action(
            "controller-a",
            stage(0, "http://private.example/a", "private"),
        ),
    );
    let r = s.submit_renderer_with_network_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        0,
        |_, _| unreachable!(),
        |_| {
            Err(earph_core::EngineError::new(
                earph_core::ErrorCode::OutputFault,
                "fixture commit refusal",
            ))
        },
    );
    assert_eq!(r.status, ReceiptStatus::Failed);
    assert_eq!(s.snapshot(), before);
    assert!(s.renderer_snapshot().staged.is_none());
    renderer_request(
        &mut s,
        "stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    media_grants(&mut s);
    let before = s.snapshot();
    let stage = s.renderer_snapshot().staged;
    let mut c = command(
        &s,
        "policy-fails",
        action(
            "controller-a",
            json!({"operation":"play","media_id":"staged-1","staging_generation":1,"grant_generation":1,"device_id":"dac"}),
        ),
    );
    c.session_id = Some(SessionId::new("refused-play").unwrap());
    let r = s.submit_renderer_with_network_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        0,
        |candidate, actual| {
            assert!(candidate.network_source().is_some());
            assert!(matches!(
                actual.payload,
                Command::NetworkStartHttpFlac { .. }
            ));
            Err(earph_core::EngineError::new(
                earph_core::ErrorCode::AuthDenied,
                "fixture policy refusal",
            ))
        },
        |_| panic!("rejected policy cannot reach commit"),
    );
    assert_eq!(r.status, ReceiptStatus::Rejected);
    assert_eq!(s.snapshot(), before);
    assert_eq!(s.renderer_snapshot().staged, stage);
    assert!(s.network_source().is_none());
    assert!(s.renderer_snapshot().active.is_none());
}

#[test]
fn renderer_metadata_pressure_keeps_exact_zero_presets_and_stop_available() {
    let mut s = supported_with_rendering_control(true);
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "a-stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "a-play", 1).status,
        ReceiptStatus::Accepted
    );
    let binding = s.network_source().unwrap().binding.clone();
    let mut refused = false;
    for (index, generation) in (1..=512).enumerate() {
        let r = renderer_request(
            &mut s,
            &format!("metadata-{index}"),
            "controller-b",
            stage(
                generation,
                &format!("http://private.example/{index}"),
                &"x".repeat(65536),
            ),
            0,
        );
        if r.status == ReceiptStatus::Rejected {
            assert_eq!(
                r.error.unwrap().code,
                earph_core::ErrorCode::ResourceBudgetExceeded
            );
            refused = true;
            break;
        }
        assert_eq!(r.status, ReceiptStatus::Applied);
    }
    assert!(
        refused,
        "renderer metadata must consume the existing shared budget"
    );
    assert!(s.network_retained_bytes().unwrap() <= 16 * 1024 * 1024);
    let staged = s.renderer_snapshot().staged;
    assert_eq!(
        renderer_request(
            &mut s,
            "safety-zero",
            "controller-a",
            json!({"operation":"set_volume","binding":binding,"volume":0}),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    assert!(s.snapshot().software_gain_zero);
    assert_eq!(
        renderer_request(
            &mut s,
            "safety-preset",
            "controller-a",
            json!({"operation":"factory_defaults","binding":binding}),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    assert!(!s.snapshot().software_gain_zero);
    assert_eq!(s.renderer_snapshot().staged, staged);
    let stop = renderer_request(
        &mut s,
        "safety-stop",
        "controller-a",
        json!({"operation":"stop","binding":binding}),
        0,
    );
    assert_eq!(stop.status, ReceiptStatus::Accepted);
    assert_eq!(s.renderer_snapshot().staged, staged);
    retire(&mut s);
}

fn local_safety_during_deferred_renderer_cleanup(shutdown: bool) {
    for revoke in [false, true] {
        let mut s = supported();
        media_grants(&mut s);
        renderer_request(
            &mut s,
            "a-stage",
            "controller-a",
            stage(0, "http://private.example/a", ""),
            0,
        );
        assert_eq!(
            play(&mut s, "controller-a", "a-play", 1).status,
            ReceiptStatus::Accepted
        );
        renderer_request(
            &mut s,
            "b-stage",
            "controller-b",
            stage(1, "http://private.example/b", ""),
            0,
        );
        assert_eq!(
            play(&mut s, "controller-b", "b-play", 2).status,
            ReceiptStatus::Accepted
        );
        let b = s.network_source().unwrap().binding.clone();
        let remote = command(
            &s,
            "remote-stop",
            if revoke {
                json!({"method":"renderer.grants.revoke","expected_generation":1,"controller_id":"controller-b"})
            } else {
                action("controller-b", json!({"operation":"stop","binding":b}))
            },
        );
        let replay_remote = |s: &mut Session| {
            if revoke {
                s.submit(remote.clone())
            } else {
                s.submit_renderer_with_network_effects(
                    remote.clone(),
                    "192.168.1.3".parse().unwrap(),
                    0,
                    |_, _| unreachable!(),
                    |_| Ok(()),
                )
            }
        };
        assert_eq!(replay_remote(&mut s).status, ReceiptStatus::Accepted);
        let local = command(
            &s,
            "local-safety",
            serde_json::to_value(if shutdown {
                Command::OwnerShutdown
            } else {
                Command::Stop
            })
            .unwrap(),
        );
        assert_eq!(s.submit(local.clone()).status, ReceiptStatus::Accepted);
        s.expire_network_authority(0).unwrap();
        let a_ack = retirement_ack(&s);
        let mut stale = a_ack.clone();
        stale.binding.source_generation += 1;
        assert_eq!(
            s.complete_network_retirement(stale, 0).unwrap_err().code,
            earph_core::ErrorCode::StaleSession
        );
        if shutdown {
            assert_eq!(
                renderer_request(
                    &mut s,
                    "shutdown-admission",
                    "controller-a",
                    stage(2, "http://private.example/denied", ""),
                    0
                )
                .status,
                ReceiptStatus::Rejected
            );
        }
        s.complete_network_retirement(a_ack.clone(), 0).unwrap();
        let after_a = s.snapshot();
        assert_eq!(
            s.complete_network_retirement(a_ack, 0).unwrap_err().code,
            earph_core::ErrorCode::StaleSession
        );
        assert_eq!(s.snapshot(), after_a);
        assert_eq!(
            s.submit(local.clone()).status,
            ReceiptStatus::Accepted,
            "A's acknowledgement cannot settle B's local safety command"
        );
        assert_eq!(replay_remote(&mut s).status, ReceiptStatus::Cancelled);
        assert_eq!(s.renderer_snapshot().closing.as_ref().unwrap().binding, b);
        assert_eq!(
            s.renderer_snapshot()
                .closing
                .as_ref()
                .unwrap()
                .retirement_request
                .as_ref()
                .unwrap()
                .as_str(),
            "local-safety"
        );
        s.expire_network_authority(0).unwrap();
        let b_ack = retirement_ack(&s);
        s.complete_network_retirement(b_ack.clone(), 0).unwrap();
        let after_b = s.snapshot();
        assert_eq!(
            s.complete_network_retirement(b_ack, 0).unwrap_err().code,
            earph_core::ErrorCode::StaleSession
        );
        assert_eq!(s.snapshot(), after_b);
        assert_eq!(s.submit(local).status, ReceiptStatus::Applied);
        assert_eq!(s.snapshot().shutdown_requested, shutdown);
        assert!(s.renderer_snapshot().active.is_none());
        assert!(s.renderer_snapshot().closing.is_none());
        s.expire_network_authority(0).unwrap();
        assert_eq!(
            submit_at(&mut s, "after-safety", json!({"method":"renderer.get"}), 0).status,
            ReceiptStatus::Applied
        );
    }
}

#[test]
fn ordinary_stop_survives_deferred_renderer_cleanup() {
    local_safety_during_deferred_renderer_cleanup(false);
}

#[test]
fn owner_shutdown_survives_deferred_renderer_cleanup() {
    local_safety_during_deferred_renderer_cleanup(true);
}

fn renderer_takeover(media_lifetime: u64) -> (Session, CommandEnvelope) {
    let mut s = supported();
    media_grants_with_lifetime(&mut s, media_lifetime);
    renderer_request(
        &mut s,
        "a-stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "a-play", 1).status,
        ReceiptStatus::Accepted
    );
    renderer_request(
        &mut s,
        "b-stage",
        "controller-b",
        stage(1, "http://private.example/b", ""),
        0,
    );
    let b_play = play_command(&s, "controller-b", "b-play", 2);
    assert_eq!(
        play(&mut s, "controller-b", "b-play", 2).status,
        ReceiptStatus::Accepted
    );
    (s, b_play)
}

#[test]
fn waiting_renderer_graph_authority_survives_only_exact_outgoing_retirement() {
    let (mut s, _) = renderer_takeover(60000);
    let binding = s.renderer_graph_continuation().unwrap();
    let outgoing = retirement_ack(&s);
    assert_ne!(outgoing.binding, binding.source);
    assert!(!s.renderer_graph_binding_valid(&binding, 0));
    s.expire_network_authority(1).unwrap();
    assert_eq!(s.renderer_graph_continuation().as_ref(), Some(&binding));
    let prepare = renderer_prepare(&s, "waiting-prepare");
    assert_eq!(
        s.submit_renderer_graph_with_effects(prepare, binding.clone(), 1, |_| {
            panic!("waiting source must not prepare a graph")
        })
        .status,
        ReceiptStatus::Rejected
    );
    let commit = command(
        &s,
        "waiting-commit",
        json!({"method":"graph.commit","token":"not-prepared","graph_hash":"c".repeat(64)}),
    );
    assert_eq!(
        s.submit_renderer_graph_with_effects(commit, binding.clone(), 1, |_| {
            panic!("waiting source must not commit a graph")
        })
        .status,
        ReceiptStatus::Rejected
    );
    let mut stale = outgoing.clone();
    stale.binding.source_generation += 1;
    assert!(s.complete_network_retirement(stale, 2).is_err());
    assert_eq!(s.renderer_graph_continuation().as_ref(), Some(&binding));
    assert!(!s.renderer_graph_binding_valid(&binding, 2));
    s.complete_network_retirement(outgoing, 3).unwrap();
    assert_eq!(s.renderer_graph_continuation().as_ref(), Some(&binding));
    assert!(s.renderer_graph_binding_valid(&binding, 3));
    measured_renderer_ready(&mut s, 4);
    let prepare = renderer_prepare(&s, "joined-prepare");
    assert_eq!(
        s.submit_renderer_graph_with_effects(prepare, binding, 4, |_| Ok(()))
            .status,
        ReceiptStatus::Accepted
    );
    assert!(s.snapshot().muted);
}

#[test]
fn waiting_renderer_graph_authority_still_invalidates_on_stop_revoke_and_expiry() {
    for fence in ["stop", "revoke", "controller-expiry", "media-expiry"] {
        let (mut s, _) = renderer_takeover(if fence == "media-expiry" { 1 } else { 60000 });
        let binding = s.renderer_graph_continuation().unwrap();
        s.expire_network_authority(1).unwrap();
        assert_eq!(s.renderer_graph_continuation().as_ref(), Some(&binding));
        match fence {
            "stop" => {
                assert_eq!(
                    renderer_request(
                        &mut s,
                        "waiting-stop",
                        "controller-b",
                        json!({"operation":"stop","binding":binding.source}),
                        2
                    )
                    .status,
                    ReceiptStatus::Accepted
                );
            }
            "revoke" => {
                assert_eq!(submit_at(&mut s, "waiting-revoke", json!({"method":"renderer.grants.revoke","expected_generation":1,"controller_id":"controller-b"}), 2).status, ReceiptStatus::Accepted);
            }
            "controller-expiry" => s.expire_network_authority(1_000_000_000).unwrap(),
            "media-expiry" => s.expire_network_authority(1_000_000).unwrap(),
            _ => unreachable!(),
        }
        assert!(s.renderer_graph_continuation().is_none(), "{fence}");
        assert!(!s.renderer_graph_binding_valid(&binding, 2));
        assert!(s.snapshot().muted);
        assert!(s.graph_job().is_none());
        assert!(s.graph_activation().is_none());
        assert!(s.graph_inspection().prepared.is_none());
    }
}

#[test]
fn origin_revoke_and_replace_preserve_deferred_renderer_ownership() {
    for replace in [false, true] {
        let (mut s, _) = renderer_takeover(60000);
        let b = s.network_source().unwrap().binding.clone();
        assert_eq!(
            renderer_request(
                &mut s,
                "remote-stop",
                "controller-b",
                json!({"operation":"stop","binding":b}),
                0
            )
            .status,
            ReceiptStatus::Accepted
        );
        let payload = if replace {
            json!({"method":"sources.grants.replace","expected_generation":1,"lifetime_ms":60000,
            "grants":{"origins":[{"origin":"http://private.example:80","pins":["192.168.1.2"],"roots_der":[]}]}})
        } else {
            json!({"method":"sources.grants.revoke","expected_generation":1})
        };
        let c = command(&s, "origin-safety", payload);
        assert_eq!(
            s.submit_with_network_effects(
                c.clone(),
                0,
                |_| unreachable!(),
                |_, _| Ok(()),
                |_| Ok(())
            )
            .status,
            ReceiptStatus::Accepted
        );
        assert!(!s.network_authority_valid(&b, 0));
        assert!(s.network_source().is_none());
        retire(&mut s);
        assert_eq!(s.submit(c.clone()).status, ReceiptStatus::Accepted);
        assert_eq!(
            s.renderer_snapshot()
                .closing
                .as_ref()
                .unwrap()
                .retirement_request
                .as_ref()
                .unwrap()
                .as_str(),
            "origin-safety"
        );
        retire(&mut s);
        assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
        assert!(s.renderer_snapshot().active.is_none());
        assert!(s.renderer_snapshot().closing.is_none());
        s.expire_network_authority(0).unwrap();
    }
}

#[test]
fn origin_expiry_during_explicit_renderer_cleanup_keeps_its_receipt_pending() {
    let (mut s, _) = renderer_takeover(1);
    let b = s.network_source().unwrap().binding.clone();
    let c = command(
        &s,
        "remote-stop",
        action("controller-b", json!({"operation":"stop","binding":b})),
    );
    assert_eq!(
        s.submit_renderer_with_network_effects(
            c.clone(),
            "192.168.1.3".parse().unwrap(),
            0,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Accepted
    );
    let now = 1_000_000;
    s.expire_network_authority(now).unwrap();
    assert!(s.network_grants().is_none());
    assert_eq!(
        s.submit_renderer_with_network_effects(
            c.clone(),
            "192.168.1.3".parse().unwrap(),
            now,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Accepted
    );
    s.complete_network_retirement(retirement_ack(&s), now)
        .unwrap();
    assert_eq!(
        s.submit_renderer_with_network_effects(
            c.clone(),
            "192.168.1.3".parse().unwrap(),
            now,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Accepted
    );
    s.complete_network_retirement(retirement_ack(&s), now)
        .unwrap();
    assert_eq!(
        s.submit_renderer_with_network_effects(
            c,
            "192.168.1.3".parse().unwrap(),
            now,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Applied
    );
    s.expire_network_authority(now).unwrap();
}

#[test]
fn automatic_origin_expiry_retires_without_inventing_a_cleanup_request() {
    let (mut s, b_play) = renderer_takeover(1);
    let now = 1_000_000;
    s.expire_network_authority(now).unwrap();
    assert!(s.network_source().is_none());
    assert!(
        s.renderer_snapshot()
            .active
            .as_ref()
            .unwrap()
            .retirement_request
            .is_none()
    );
    assert_eq!(
        s.submit_renderer_with_network_effects(
            b_play.clone(),
            "192.168.1.3".parse().unwrap(),
            now,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Failed
    );
    let failed = s.snapshot();
    s.expire_network_authority(now).unwrap();
    assert_eq!(
        s.snapshot(),
        failed,
        "repeated expiry must not churn epochs or receipts"
    );
    s.complete_network_retirement(retirement_ack(&s), now)
        .unwrap();
    assert!(
        s.renderer_snapshot()
            .closing
            .as_ref()
            .unwrap()
            .retirement_request
            .is_none()
    );
    s.complete_network_retirement(retirement_ack(&s), now)
        .unwrap();
    assert_eq!(
        s.submit_renderer_with_network_effects(
            b_play,
            "192.168.1.3".parse().unwrap(),
            now,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Failed
    );
    assert!(s.renderer_snapshot().active.is_none());
    assert!(s.renderer_snapshot().closing.is_none());
    s.expire_network_authority(now).unwrap();
}

#[test]
fn redacted_renderer_projection_rejects_semantic_corruption() {
    let mut s = supported();
    renderer_request(
        &mut s,
        "stage",
        "controller-a",
        stage(0, "http://private.example/a", "private"),
        0,
    );
    let original = serde_json::to_value(s.renderer_snapshot()).unwrap();
    let parsed: earph_core::renderer::RendererSnapshot =
        serde_json::from_value(original.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), original);
    for (field, value) in [
        ("controller_count", json!(0)),
        ("grant_generation", json!(0)),
        ("enabled", json!(false)),
        ("retained_bytes", json!(16 * 1024 * 1024 + 1)),
    ] {
        let mut bad = original.clone();
        bad[field] = value;
        assert!(
            serde_json::from_value::<earph_core::renderer::RendererSnapshot>(bad).is_err(),
            "invalid {field}"
        );
    }
    for field in [
        "staged",
        "active",
        "closing",
        "controllers",
        "retained_bytes",
    ] {
        let mut bad = original.clone();
        bad.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<earph_core::renderer::RendererSnapshot>(bad).is_err(),
            "missing {field}"
        );
    }
    let mut bad = original.clone();
    bad["staged"]["generation"] = json!(0);
    assert!(serde_json::from_value::<earph_core::renderer::RendererSnapshot>(bad).is_err());
    let mut bad = original;
    bad["controllers"][0]["generation"] = json!(0);
    assert!(serde_json::from_value::<earph_core::renderer::RendererSnapshot>(bad).is_err());
}

#[test]
fn rights_lifetime_and_capacity_failures_do_not_replace_or_reserve_authority() {
    let mut s = supported();
    let before = s.snapshot();
    for (id, payload) in [
        ("stale", grants(0, 1)),
        ("zero-lifetime", grants(1, 0)),
        ("excess-lifetime", grants(1, 3_600_001)),
        (
            "empty",
            json!({"method":"renderer.grants.replace","expected_generation":1,"lifetime_ms":1,"grants":{"controllers":[]}}),
        ),
    ] {
        assert_eq!(
            submit_at(&mut s, id, payload, 0).status,
            ReceiptStatus::Rejected
        );
        assert_eq!(s.snapshot(), before);
    }
    let mut stage_only = grants(1, 3_600_000);
    stage_only["grants"]["controllers"][0]["rights"] = json!(["stage"]);
    assert_eq!(
        submit_at(&mut s, "stage-only", stage_only, 0).status,
        ReceiptStatus::Applied
    );
    let mut c = command(
        &s,
        "transport-denied",
        action("controller-a", json!({"operation":"stop","binding":null})),
    );
    if let Command::RendererRequest {
        grant_generation, ..
    } = &mut c.payload
    {
        *grant_generation = 2;
    }
    assert_eq!(
        s.submit_renderer_with_network_effects(
            c,
            "192.168.1.2".parse().unwrap(),
            0,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .error
        .unwrap()
        .code,
        earph_core::ErrorCode::AuthDenied
    );
    let mut c = command(&s, "oversized", action("controller-a", stage(0, "x", "")));
    if let Command::RendererRequest {
        grant_generation,
        action: earph_core::renderer::RendererAction::StageUri { uri, .. },
        ..
    } = &mut c.payload
    {
        *grant_generation = 2;
        *uri = String::with_capacity(8193);
        uri.push('x');
    }
    let rejected = s.submit_renderer_with_network_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        0,
        |_, _| unreachable!(),
        |_| Ok(()),
    );
    assert_eq!(
        rejected.error.unwrap().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
    let mut c = command(&s, "oversized", action("controller-a", stage(0, "x", "")));
    if let Command::RendererRequest {
        grant_generation, ..
    } = &mut c.payload
    {
        *grant_generation = 2;
    }
    assert_eq!(
        s.submit_renderer_with_network_effects(
            c,
            "192.168.1.2".parse().unwrap(),
            0,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Applied
    );
}

#[test]
fn stop_and_revoke_deny_execution_until_both_exact_retirements_complete() {
    for revoke in [false, true] {
        let mut s = supported();
        media_grants(&mut s);
        renderer_request(
            &mut s,
            "a-stage",
            "controller-a",
            stage(0, "http://private.example/a", ""),
            0,
        );
        assert_eq!(
            play(&mut s, "controller-a", "a-play", 1).status,
            ReceiptStatus::Accepted
        );
        renderer_request(
            &mut s,
            "b-stage",
            "controller-b",
            stage(1, "http://private.example/b", ""),
            0,
        );
        assert_eq!(
            play(&mut s, "controller-b", "b-play", 2).status,
            ReceiptStatus::Accepted
        );
        let b = s.network_source().unwrap().binding.clone();
        let payload = if revoke {
            json!({"method":"renderer.grants.revoke","expected_generation":1,"controller_id":"controller-b"})
        } else {
            action("controller-b", json!({"operation":"stop","binding":b}))
        };
        let c = command(&s, "b-retirement", payload);
        let submit = |s: &mut Session| {
            if revoke {
                s.submit(c.clone())
            } else {
                s.submit_renderer_with_network_effects(
                    c.clone(),
                    "192.168.1.3".parse().unwrap(),
                    0,
                    |_, _| unreachable!(),
                    |_| Ok(()),
                )
            }
        };
        assert_eq!(submit(&mut s).status, ReceiptStatus::Accepted);
        assert!(
            s.network_source().is_none(),
            "pending cleanup cannot supply an executable source"
        );
        assert!(!s.network_authority_valid(&b, 0));
        assert_eq!(
            s.network_snapshot().lifecycle,
            Some(earph_core::network_source::NetworkLifecycle::Retiring)
        );
        let pending = serde_json::to_value(s.renderer_snapshot()).unwrap();
        assert_eq!(pending["active"]["retirement_request"], "b-retirement");
        assert_eq!(pending["closing"]["controller_id"], "controller-a");
        if revoke {
            assert_eq!(pending["controller_count"], 1);
        }
        assert_eq!(submit(&mut s).status, ReceiptStatus::Accepted);
        assert_eq!(
            renderer_request(
                &mut s,
                "replacement-stage",
                "controller-a",
                stage(2, "http://private.example/replacement", "replacement"),
                0
            )
            .status,
            ReceiptStatus::Applied
        );
        let staged = s.renderer_snapshot().staged;
        retire(&mut s);
        let pending = serde_json::to_value(s.renderer_snapshot()).unwrap();
        assert_eq!(pending["active"], Value::Null);
        assert_eq!(
            pending["closing"]["binding"],
            serde_json::to_value(&b).unwrap()
        );
        assert_eq!(pending["closing"]["retirement_request"], "b-retirement");
        assert_eq!(submit(&mut s).status, ReceiptStatus::Accepted);
        assert_eq!(s.renderer_snapshot().staged, staged);
        retire(&mut s);
        assert_eq!(submit(&mut s).status, ReceiptStatus::Applied);
        assert!(s.renderer_snapshot().active.is_none());
        assert!(s.renderer_snapshot().closing.is_none());
        assert_eq!(s.renderer_snapshot().staged, staged);
        assert!(s.network_retained_bytes().unwrap() <= 16 * 1024 * 1024);
    }
}

#[test]
fn play_for_the_same_owned_session_does_not_readmit_a_source_or_renew_authority() {
    let mut s = supported();
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "play", 1).status,
        ReceiptStatus::Accepted
    );
    let before = s.snapshot();
    let binding = s.network_source().unwrap().binding.clone();
    let expiry = s.network_source().unwrap().expires_monotonic_ns;
    let c = command(
        &s,
        "repeat-play",
        action(
            "controller-a",
            json!({"operation":"play","media_id":"staged-1","staging_generation":1,"grant_generation":1,"device_id":"dac"}),
        ),
    );
    let repeated = s.submit_renderer_with_network_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        10,
        |_, _| panic!("repeat Play must not admit another fetch"),
        |_| panic!("repeat Play has no mutation to commit"),
    );
    assert_eq!(repeated.status, ReceiptStatus::Applied);
    assert_eq!(s.snapshot(), before);
    assert_eq!(s.network_source().unwrap().binding, binding);
    assert_eq!(s.network_source().unwrap().expires_monotonic_ns, expiry);
}

#[test]
fn stage_preserves_an_explicit_unmuted_gain_without_output_io() {
    let mut s = owner();
    s.enable_renderer_control();
    for (id, payload) in [
        ("grant", grants(0, 100)),
        (
            "gain",
            json!({"method":"volume.set","millidecibels":-60000,"ramp_frames":64}),
        ),
        ("unmute", json!({"method":"mute.set","muted":false})),
    ] {
        assert_eq!(
            submit_at(&mut s, id, payload, 0).status,
            ReceiptStatus::Applied
        );
    }
    let before = s.snapshot();
    assert!(!before.muted);
    assert_eq!(before.software_gain_db, -60.0);
    assert_eq!(
        renderer_request(
            &mut s,
            "stage",
            "controller-a",
            stage(0, "untrusted:no-fetch", ""),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    let mut expected = before;
    expected.revision += 1;
    expected.event_sequence += 1;
    assert_eq!(s.snapshot(), expected);
}

#[test]
fn renderer_zero_requires_current_authority_and_the_exact_active_source() {
    let mut s = supported_with_rendering_control(true);
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "stage-zero",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "play-zero", 1).status,
        ReceiptStatus::Accepted
    );
    let binding = s.network_source().unwrap().binding.clone();
    let receipt = renderer_request(
        &mut s,
        "remote-zero",
        "controller-a",
        json!({"operation":"set_volume","binding":binding,"volume":0}),
        0,
    );
    assert_eq!(receipt.status, ReceiptStatus::Applied, "{receipt:?}");
    assert_eq!(
        serde_json::to_value(s.snapshot()).unwrap()["software_gain_zero"],
        true
    );
    let before = s.snapshot();
    let receipt = renderer_request(
        &mut s,
        "other-zero",
        "controller-b",
        json!({"operation":"set_volume","binding":binding,"volume":1}),
        0,
    );
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(s.snapshot(), before);
    let mut stale = binding.clone();
    stale.source_generation += 1;
    assert_eq!(
        renderer_request(
            &mut s,
            "stale-zero",
            "controller-a",
            json!({"operation":"set_volume","binding":stale,"volume":0}),
            0
        )
        .status,
        ReceiptStatus::Rejected
    );
    assert_eq!(s.snapshot(), before);
}

#[test]
fn a_transport_grant_cannot_authorize_renderer_gain_or_presets() {
    let mut s = supported();
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "stage-no-gain",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "play-no-gain", 1).status,
        ReceiptStatus::Accepted
    );
    let binding = s.network_source().unwrap().binding.clone();
    let before = s.snapshot();
    for (id, operation) in [
        (
            "unauthorized-gain",
            json!({"operation":"set_volume","binding":binding,"volume":0}),
        ),
        (
            "unauthorized-preset",
            json!({"operation":"factory_defaults","binding":binding}),
        ),
    ] {
        assert_eq!(
            renderer_request(&mut s, id, "controller-a", operation, 0)
                .error
                .unwrap()
                .code,
            earph_core::ErrorCode::AuthDenied
        );
        assert_eq!(s.snapshot(), before);
    }
}

#[test]
fn renderer_db_zero_cap_failure_and_preset_keep_one_atomic_gain_ledger() {
    let mut s = supported_with_rendering_control(true);
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "stage-level",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "play-level", 1).status,
        ReceiptStatus::Accepted
    );
    let binding = s.network_source().unwrap().binding.clone();
    let expiry = s.network_source().unwrap().expires_monotonic_ns;
    assert_eq!(
        renderer_request(
            &mut s,
            "db-silence",
            "controller-a",
            json!({"operation":"set_volume_db","binding":binding,"volume_db":-32767}),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    assert!(s.snapshot().software_gain_zero);
    assert_eq!(submit_at(&mut s, "remote-cap", json!({"method":"volume.caps.set","software_millidecibels":-60000,"hardware_millidecibels":null}), 0).status, ReceiptStatus::Applied);
    let before = s.snapshot();
    let refused = renderer_request(
        &mut s,
        "loud-wire",
        "controller-a",
        json!({"operation":"set_volume","binding":binding,"volume":30700}),
        0,
    );
    assert_eq!(
        refused.error.unwrap().code,
        earph_core::ErrorCode::SafetyPolicyBlocked
    );
    assert_eq!(s.snapshot(), before);
    assert_eq!(
        renderer_request(
            &mut s,
            "db-tie",
            "controller-a",
            json!({"operation":"set_volume_db","binding":binding,"volume_db":-15360}),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    assert_eq!(s.snapshot().software_gain_db, -60.004);
    assert!(!s.snapshot().software_gain_zero);
    assert!(s.snapshot().muted);
    let before = s.snapshot();
    assert_eq!(
        renderer_request(
            &mut s,
            "factory-preset",
            "controller-a",
            json!({"operation":"factory_defaults","binding":binding}),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    assert_eq!(s.snapshot().revision, before.revision + 1);
    assert_eq!(s.snapshot().software_gain_db, -60.0);
    assert_eq!(s.snapshot().epochs, before.epochs);
    assert_eq!(s.network_source().unwrap().binding, binding);
    assert_eq!(s.network_source().unwrap().expires_monotonic_ns, expiry);
}

#[test]
fn reported_remote_setting_is_idempotent_after_current_authority_checks() {
    let mut s = supported_with_rendering_control(true);
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "stage-grid",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "play-grid", 1).status,
        ReceiptStatus::Accepted
    );
    let binding = s.network_source().unwrap().binding.clone();
    assert_eq!(submit_at(&mut s, "fine-cap", json!({"method":"volume.caps.set","software_millidecibels":-40001,"hardware_millidecibels":null}), 0).status, ReceiptStatus::Applied);
    // The reported -40 dB wire setting exceeds this exact local cap by 0.001 dB.
    let before = s.snapshot();
    assert_eq!(before.software_gain_db, -40.001);
    let expiry = s.network_source().unwrap().expires_monotonic_ns;
    let c = command(
        &s,
        "repeat-grid",
        action(
            "controller-a",
            json!({"operation":"set_volume","binding":binding,"volume":20467}),
        ),
    );
    let receipt = s.submit_renderer_with_network_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        10,
        |_, _| panic!("gain must not admit a fetch"),
        |_| panic!("reported value needs no commit"),
    );
    assert_eq!(receipt.status, ReceiptStatus::Applied);
    assert_eq!(s.snapshot(), before);
    assert_eq!(s.network_source().unwrap().expires_monotonic_ns, expiry);
    let c = command(
        &s,
        "wrong-peer-grid",
        action(
            "controller-a",
            json!({"operation":"set_volume","binding":binding,"volume":20467}),
        ),
    );
    let receipt = s.submit_renderer_with_network_effects(
        c,
        "192.168.1.99".parse().unwrap(),
        10,
        |_, _| unreachable!(),
        |_| unreachable!(),
    );
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(s.snapshot(), before);
}

fn owned_fixed_renderer() -> Session {
    let mut s = supported_with_mode(true, "fixed");
    for (id, payload) in [
        (
            "fixed-unity",
            json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}),
        ),
        (
            "fixed-consent",
            json!({"method":"output.consent","device_id":"dac","profile_id":"profile"}),
        ),
    ] {
        assert_eq!(
            submit_at(&mut s, id, payload, 0).status,
            ReceiptStatus::Applied
        );
    }
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "fixed-stage",
        "controller-a",
        stage(0, "http://private.example/fixed", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "fixed-play", 1).status,
        ReceiptStatus::Accepted
    );
    assert!(s.snapshot().muted);
    assert_eq!(s.snapshot().software_gain_db, 0.0);
    s
}

#[test]
fn owned_fixed_renderer_rejects_reported_finite_gain_before_idempotence() {
    let mut s = owned_fixed_renderer();
    let binding = s.network_source().unwrap().binding.clone();
    let before = s.snapshot();
    for (id, action) in [
        (
            "same-volume",
            json!({"operation":"set_volume","binding":binding,"volume":30700}),
        ),
        (
            "same-db",
            json!({"operation":"set_volume_db","binding":binding,"volume_db":0}),
        ),
    ] {
        assert_eq!(
            renderer_request(&mut s, id, "controller-a", action, 0).status,
            ReceiptStatus::Rejected
        );
        assert_eq!(s.snapshot(), before);
    }
    assert_eq!(
        renderer_request(
            &mut s,
            "fixed-factory",
            "controller-a",
            json!({"operation":"factory_defaults","binding":binding}),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    assert_eq!(s.snapshot().software_gain_db, 0.0);
    assert!(s.snapshot().muted);
}

#[test]
fn owned_fixed_renderer_rejects_different_finite_gain_while_muted() {
    let mut s = owned_fixed_renderer();
    let binding = s.network_source().unwrap().binding.clone();
    let before = s.snapshot();
    for (id, action) in [
        (
            "different-volume",
            json!({"operation":"set_volume","binding":binding,"volume":30699}),
        ),
        (
            "different-db",
            json!({"operation":"set_volume_db","binding":binding,"volume_db":-1}),
        ),
    ] {
        assert_eq!(
            renderer_request(&mut s, id, "controller-a", action, 0).status,
            ReceiptStatus::Rejected
        );
        assert_eq!(s.snapshot(), before);
    }
    assert_eq!(
        submit_at(
            &mut s,
            "local-finite-retains-contract",
            json!({"method":"volume.set","millidecibels":-4,"ramp_frames":0}),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    assert_eq!(s.snapshot().software_gain_db, -0.004);
    assert!(s.snapshot().muted);
}

#[test]
fn controller_and_media_grant_generations_remain_separate_authorities() {
    let mut s = supported();
    let mut replacement = grants(1, 1000);
    replacement["grants"]["controllers"][0]["playback"] = playback_authorization(&s);
    assert_eq!(
        submit_at(&mut s, "replace-controller", replacement, 0).status,
        ReceiptStatus::Applied
    );
    media_grants(&mut s);
    let mut c = command(
        &s,
        "stage",
        action("controller-a", stage(0, "http://private.example/a", "")),
    );
    if let Command::RendererRequest {
        grant_generation, ..
    } = &mut c.payload
    {
        *grant_generation = 2;
    }
    assert_eq!(
        s.submit_renderer_with_network_effects(
            c,
            "192.168.1.2".parse().unwrap(),
            0,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Applied
    );
    let mut c = command(
        &s,
        "play",
        action(
            "controller-a",
            json!({"operation":"play","media_id":"staged-1","staging_generation":1,"grant_generation":1,"device_id":"dac"}),
        ),
    );
    c.session_id = Some(SessionId::new("source-session").unwrap());
    if let Command::RendererRequest {
        grant_generation, ..
    } = &mut c.payload
    {
        *grant_generation = 2;
    }
    assert_eq!(
        s.submit_renderer_with_network_effects(
            c,
            "192.168.1.2".parse().unwrap(),
            0,
            |_, _| Ok(()),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Accepted
    );
    let owner = s.renderer_snapshot().active.unwrap();
    assert_eq!(owner.grant_generation, 2);
    assert_eq!(owner.binding.grant_generation, 1);
}

#[test]
fn u4_renderer_staging_rejects_double_xml_expansion_before_commit() {
    let mut s = supported();
    let before = s.snapshot();
    let receipt = renderer_request(
        &mut s,
        "oversized-event",
        "controller-a",
        stage(0, "http://private.example/a", &"\"".repeat(65_536)),
        0,
    );
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(
        receipt.error.unwrap().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(s.snapshot(), before);
    assert!(s.renderer_snapshot().staged.is_none());
}

#[test]
fn u4_renderer_grants_require_an_explicit_nullable_playback_authorization() {
    let document = json!({"controllers":[{
        "id":"controller-a","peers":["192.168.1.2"],"rights":["stage"]
    }]});
    assert!(
        serde_json::from_value::<earph_core::renderer::RendererGrantDocument>(document).is_err()
    );
}

#[test]
fn u4_callback_grants_use_distinct_local_authority() {
    let mut s = owner();
    s.enable_renderer_control();
    let mut g = grants(0, 60000);
    g["grants"]["controllers"][0]["playback"] = Value::Null;
    g["grants"]["controllers"][0]["rights"] = json!(["stage", "events"]);
    assert_eq!(
        submit_at(&mut s, "u4-controller", g, 0).status,
        ReceiptStatus::Applied
    );
    let callback = json!({"method":"renderer.callbacks.replace","controller_id":"controller-a",
        "expected_controller_generation":1,"expected_generation":0,"lifetime_ms":60000,
        "grants":{"origins":[{"origin":"http://private.example:80","pins":["192.168.1.2"]}]}});
    let receipt = submit_at(&mut s, "u4-callback", callback, 0);
    assert_eq!(receipt.status, ReceiptStatus::Applied, "{receipt:?}");
    assert!(s.network_snapshot().grant_generation == 0);
    assert!(
        !serde_json::to_string(&s.checkpoint())
            .unwrap()
            .contains("private.example")
    );
}

#[test]
fn u4_absent_playback_authorization_cannot_start_a_source() {
    let mut s = supported();
    let replacement = grants(1, 60000);
    assert_eq!(
        submit_at(&mut s, "null-playback", replacement, 0).status,
        ReceiptStatus::Applied
    );
    media_grants(&mut s);
    let mut staged = action("controller-a", stage(0, "http://private.example/a", ""));
    staged["grant_generation"] = json!(2);
    let c = command(&s, "null-stage", staged);
    assert_eq!(
        s.submit_renderer_with_network_effects(
            c,
            "192.168.1.2".parse().unwrap(),
            0,
            |_, _| unreachable!(),
            |_| Ok(())
        )
        .status,
        ReceiptStatus::Applied
    );
    let mut c = play_command(&s, "controller-a", "null-play", 1);
    if let Command::RendererRequest {
        grant_generation, ..
    } = &mut c.payload
    {
        *grant_generation = 2;
    }
    let before = s.snapshot();
    let receipt = s.submit_renderer_with_network_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        0,
        |_, _| panic!("no fetch without a local pin"),
        |_| Ok(()),
    );
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(
        receipt.error.unwrap().code,
        earph_core::ErrorCode::AuthDenied
    );
    assert_eq!(s.snapshot(), before);
    assert!(s.network_source().is_none());
}

#[test]
fn u4_play_captures_immutable_authored_graph_and_stop_cancels_it() {
    let mut s = supported();
    media_grants(&mut s);
    assert_eq!(
        renderer_request(
            &mut s,
            "pinned-stage",
            "controller-a",
            stage(0, "http://private.example/a", ""),
            0
        )
        .status,
        ReceiptStatus::Applied
    );
    assert_eq!(
        play(&mut s, "controller-a", "pinned-play", 1).status,
        ReceiptStatus::Accepted
    );
    let binding = s.renderer_graph_continuation().unwrap();
    let expected: earph_core::renderer::RendererPlaybackAuthorization =
        serde_json::from_value(playback_authorization(&s)).unwrap();
    assert_eq!(binding.authorization.as_ref(), &expected);
    assert_eq!(binding.authored_sha256, expected.authored_sha256().unwrap());
    assert!(s.renderer_graph_binding_valid(&binding, 0));
    assert!(s.snapshot().muted);
    let source = s.network_source().unwrap();
    assert_eq!(
        renderer_request(
            &mut s,
            "pinned-stop",
            "controller-a",
            json!({"operation":"stop","binding":source.binding}),
            0
        )
        .status,
        ReceiptStatus::Accepted
    );
    assert!(!s.renderer_graph_binding_valid(&binding, 0));
    assert!(s.renderer_graph_continuation().is_none());
}

#[test]
fn u4_early_renderer_unmute_is_refused_without_deferred_intent() {
    let mut s = supported_with_rendering_control(true);
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "early-stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "early-play", 1).status,
        ReceiptStatus::Accepted
    );
    let binding = s.network_source().unwrap().binding.clone();
    let receipt = renderer_request(
        &mut s,
        "early-unmute",
        "controller-a",
        json!({"operation":"set_mute","binding":binding,"muted":false}),
        0,
    );
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert!(s.snapshot().muted);
    assert!(s.renderer_graph_continuation().is_some());
}

fn measured_renderer_ready(s: &mut Session, now: u64) {
    use earph_core::{audio::*, network_source::*};
    let source = s.network_source().unwrap();
    let state = s.snapshot();
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
    let observation = NetworkPlaybackObservation {
        binding: source.binding.clone(),
        source_epochs: source.source_epochs,
        output_epochs: state.epochs,
        graph_epoch: state.graph_epoch,
        device_epoch: state.device_epoch,
        lifecycle: NetworkLifecycle::Ready,
        format,
        output_format: format,
        total_frames: None,
        source_position_frames: 0,
        audible_source_position_frames: None,
        submitted_frames: 0,
        consumed_frames: None,
        backend_delay_frames: None,
        consumption_basis: "unknown",
        input_peak: None,
        output_peak: None,
        modified: false,
        backend: None,
        output_activity: None,
        processing: None,
        replacement_boundary: None,
    };
    s.advance_network(NetworkEvent {
        binding: source.binding.clone(),
        source_epochs: source.source_epochs,
        output_epochs: state.epochs,
        graph_epoch: state.graph_epoch,
        device_epoch: state.device_epoch,
        monotonic_ns: now,
        kind: NetworkEventKind::Prepared { observation },
    })
    .unwrap();
}
fn renderer_prepare(s: &Session, id: &str) -> CommandEnvelope {
    let auth = s
        .renderer_graph_continuation()
        .unwrap()
        .authorization
        .clone();
    command(
        s,
        id,
        json!({"method":"graph.prepare","graph":auth.intent.graph,"processing":auth.intent.processing}),
    )
}
fn renderer_preview(s: &Session) -> std::sync::Arc<earph_core::graph_control::GraphPreview> {
    use earph_core::{audio::*, graph::PreservationDecision, graph_control::*};
    let job = s.graph_job().unwrap();
    let native = s.snapshot().network_playback.unwrap().format;
    let float = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    std::sync::Arc::new(GraphPreview {
        diagnostic_nodes: None,
        profile_application: None,
        backend: None,
        graph_hash: "c".repeat(64),
        kernel_hash: "d".repeat(64),
        implementation_version: "unit-contract".into(),
        binding: job.binding,
        source_format: native,
        native_output_format: native,
        processing_input_format: float,
        processing_output_format: float,
        quantum_frames: 64,
        timing: GraphTiming::SameRate {
            latency_frames: 0,
            tail_frames: 0,
            tail_policy: GraphTailPolicy::AuthoredFiniteSupport,
            terminal_output_bounds: vec![0.0; 2],
            node_tails: vec![],
        },
        preservation: PreservationDecision::ProcessFloat64,
        transformations: vec![],
        resources: GraphResidentUsage::default(),
        channel_peak_bounds: vec![1.0; 2],
        required_headroom_db: 0.0,
        transition: GraphTransition::MutedRebuffer,
    })
}
#[test]
fn u4_measured_readiness_prepares_only_the_pinned_intent_and_commits_its_fresh_token() {
    let mut s = supported();
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "ready-stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    assert_eq!(
        play(&mut s, "controller-a", "ready-play", 1).status,
        ReceiptStatus::Accepted
    );
    let binding = s.renderer_graph_continuation().unwrap();
    let before_measurement = renderer_prepare(&s, "before-measurement");
    assert_eq!(
        s.submit_renderer_graph_with_effects(before_measurement, binding.clone(), 0, |_| Ok(()))
            .status,
        ReceiptStatus::Rejected
    );
    measured_renderer_ready(&mut s, 10);
    let mut different = renderer_prepare(&s, "different-intent");
    if let Command::GraphPrepare { graph, .. } = &mut different.payload {
        std::sync::Arc::make_mut(graph).output = NodeId::new("different").unwrap();
    }
    assert_eq!(
        s.submit_renderer_graph_with_effects(different, binding.clone(), 10, |_| Ok(()))
            .status,
        ReceiptStatus::Rejected
    );
    let prepare = renderer_prepare(&s, "fresh-measured");
    assert_eq!(
        s.submit_renderer_graph_with_effects(prepare.clone(), binding.clone(), 10, |_| Ok(()))
            .status,
        ReceiptStatus::Accepted
    );
    let job = s.graph_job().unwrap();
    assert_eq!(job.renderer_application.as_ref(), Some(&binding));
    assert_eq!(job.intent, binding.authorization.intent);
    let mut failed = s.clone();
    let preparation_error =
        earph_core::EngineError::new(earph_core::ErrorCode::AssetInvalid, "preparation failed");
    assert_eq!(
        failed.complete_graph_preparation(
            &prepare.request_id,
            &job.binding,
            20,
            Err(preparation_error.into()),
        ),
        Err(preparation_error)
    );
    assert!(failed.renderer_graph_binding_valid(&binding, 21));
    let failed_state = serde_json::to_value(failed.snapshot()).unwrap();
    let terminal =
        failed.submit_renderer_graph_with_effects(prepare.clone(), binding.clone(), 21, |_| {
            panic!("terminal replay must not persist")
        });
    assert_eq!(terminal.status, ReceiptStatus::Failed);
    assert_eq!(terminal.error, Some(preparation_error));
    assert!(failed.graph_job().is_none());
    assert_eq!(
        serde_json::to_value(failed.snapshot()).unwrap(),
        failed_state
    );
    let preview = renderer_preview(&s);
    s.complete_graph_preparation(&prepare.request_id, &job.binding, 20, Ok(preview))
        .unwrap();
    let ready = s.graph_inspection().prepared.unwrap();
    assert_eq!(ready.expires_monotonic_ns, 30_000_000_020);
    let commit = command(
        &s,
        "fresh-commit",
        json!({"method":"graph.commit","token":ready.token,"graph_hash":ready.preview.graph_hash}),
    );
    assert_eq!(
        s.submit_graph(commit.clone(), 21).status,
        ReceiptStatus::Rejected
    );
    for (id, bad_token) in [("wrong-token", true), ("wrong-hash", false)] {
        let mut invalid = commit.clone();
        invalid.request_id = RequestId::new(id).unwrap();
        if let Command::GraphCommit { token, graph_hash } = &mut invalid.payload {
            if bad_token {
                *token = PreparedGraphId::new("graph-unprepared").unwrap();
            } else {
                *graph_hash = "0".repeat(64);
            }
        }
        let rejected = s.submit_renderer_graph_with_effects(invalid, binding.clone(), 21, |_| {
            panic!("invalid fresh commit must not persist")
        });
        assert_eq!(rejected.status, ReceiptStatus::Rejected);
        assert_eq!(
            rejected.error.unwrap().code,
            earph_core::ErrorCode::StaleSession
        );
        assert!(s.graph_inspection().prepared.is_some());
    }
    let accepted =
        s.submit_renderer_graph_with_effects(commit.clone(), binding.clone(), 21, |_| Ok(()));
    assert_eq!(accepted.status, ReceiptStatus::Accepted);
    assert_eq!(
        s.graph_activation().unwrap().renderer_application.as_ref(),
        Some(&binding)
    );
    assert!(s.snapshot().muted);
    assert!(s.graph_inspection().prepared.is_none());
    let pending_state = serde_json::to_value(s.snapshot()).unwrap();
    let activation = s.graph_activation().unwrap();
    assert_eq!(
        s.submit_renderer_graph_with_effects(commit.clone(), binding.clone(), 22, |_| {
            panic!("accepted replay must not persist")
        }),
        accepted
    );
    assert_eq!(serde_json::to_value(s.snapshot()).unwrap(), pending_state);
    let replayed_activation = s.graph_activation().unwrap();
    assert_eq!(replayed_activation.request_id, activation.request_id);
    assert_eq!(replayed_activation.epochs, activation.epochs);
    assert_eq!(
        replayed_activation.prepared.token,
        activation.prepared.token
    );
    assert_eq!(
        replayed_activation.renderer_application,
        activation.renderer_application
    );
    let mut failed_activation = s.clone();
    failed_activation.fail_graph_activation(
        &commit.request_id,
        earph_core::EngineError::new(earph_core::ErrorCode::AssetInvalid, "activation failed"),
    );
    assert!(!failed_activation.renderer_graph_binding_valid(&binding, 23));
    assert_eq!(
        failed_activation
            .submit_renderer_graph_with_effects(commit.clone(), binding.clone(), 23, |_| {
                panic!("failed activation lost playback authority")
            })
            .status,
        ReceiptStatus::Rejected
    );
    let mut conflict = commit.clone();
    conflict.expected_revision = Some(s.snapshot().revision);
    let rejected = s.submit_renderer_graph_with_effects(conflict, binding.clone(), 23, |_| {
        panic!("conflicting replay must not persist")
    });
    assert_eq!(rejected.status, ReceiptStatus::Rejected);
    assert_eq!(
        rejected.error.unwrap().code,
        earph_core::ErrorCode::AssetInvalid
    );
    let mut stale = binding.as_ref().clone();
    stale.attempt_generation += 1;
    let rejected = s.submit_renderer_graph_with_effects(commit.clone(), stale.into(), 24, |_| {
        panic!("stale replay must not persist")
    });
    assert_eq!(rejected.status, ReceiptStatus::Rejected);
    assert_eq!(
        rejected.error.unwrap().code,
        earph_core::ErrorCode::StaleSession
    );
    assert_eq!(serde_json::to_value(s.snapshot()).unwrap(), pending_state);
    let revoke = command(
        &s,
        "replay-revoke",
        json!({"method":"renderer.grants.revoke","expected_generation":1,"controller_id":"controller-a"}),
    );
    assert_eq!(s.submit_graph(revoke, 25).status, ReceiptStatus::Accepted);
    assert!(!s.renderer_graph_binding_valid(&binding, 26));
    let rejected = s.submit_renderer_graph_with_effects(commit, binding, 26, |_| {
        panic!("revoked replay must not persist")
    });
    assert_eq!(rejected.status, ReceiptStatus::Rejected);
    assert_eq!(
        rejected.error.unwrap().code,
        earph_core::ErrorCode::StaleSession
    );
}
#[test]
fn u4_revocation_discards_a_late_graph_result_and_its_continuation() {
    let mut s = supported();
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "late-stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    play(&mut s, "controller-a", "late-play", 1);
    measured_renderer_ready(&mut s, 10);
    let binding = s.renderer_graph_continuation().unwrap();
    let prepare = renderer_prepare(&s, "late-prepare");
    assert_eq!(
        s.submit_renderer_graph_with_effects(prepare.clone(), binding.clone(), 10, |_| Ok(()))
            .status,
        ReceiptStatus::Accepted
    );
    let job = s.graph_job().unwrap();
    let preview = renderer_preview(&s);
    let revoke = command(
        &s,
        "late-revoke",
        json!({"method":"renderer.grants.revoke","expected_generation":1,"controller_id":"controller-a"}),
    );
    assert_eq!(s.submit_graph(revoke, 11).status, ReceiptStatus::Accepted);
    assert!(
        s.complete_graph_preparation(&prepare.request_id, &job.binding, 12, Ok(preview))
            .is_err()
    );
    assert!(!s.renderer_graph_binding_valid(&binding, 12));
    assert!(s.renderer_graph_continuation().is_none());
    assert!(
        s.graph_job().is_none()
            && s.graph_activation().is_none()
            && s.graph_inspection().prepared.is_none()
    );
    assert!(s.snapshot().muted);
}

fn event_owner() -> Session {
    let mut s = owner();
    s.enable_renderer_control();
    let mut grant = grants(0, 60000);
    grant["grants"]["controllers"][0]["rights"] = json!(["stage", "events"]);
    assert_eq!(
        submit_at(&mut s, "event-controller", grant, 0).status,
        ReceiptStatus::Applied
    );
    let callback = json!({"method":"renderer.callbacks.replace","controller_id":"controller-a","expected_controller_generation":1,
        "expected_generation":0,"lifetime_ms":60000,"grants":{"origins":[{"origin":"http://private.example:80","pins":["192.168.1.2"]}]}});
    let receipt = submit_at(&mut s, "event-callback", callback, 0);
    assert_eq!(receipt.status, ReceiptStatus::Applied, "{receipt:?}");
    s
}
struct OwnedEventSlots {
    id: u64,
    service: earph_core::renderer_events::RendererService,
    slots: [String; 2],
}
impl earph_core::renderer_events::RendererSubscriptionLease for OwnedEventSlots {
    fn lease_id(&self) -> u64 {
        self.id
    }
    fn service(&self) -> earph_core::renderer_events::RendererService {
        self.service
    }
    fn event_slot_bytes(&self) -> (usize, usize) {
        (self.slots[0].capacity(), self.slots[1].capacity())
    }
}
fn subscribe(
    s: &mut Session,
    id: &str,
    lease_id: u64,
    service: earph_core::renderer_events::RendererService,
) -> earph_core::renderer_events::RendererSubscriptionResult {
    let c = command(
        s,
        id,
        json!({"method":"renderer.subscription","controller_id":"controller-a","grant_generation":1,"callback_generation":1,
        "action":{"operation":"subscribe","service":service,"callbacks":["http://private.example:80/events"],"timeout_seconds":1800}}),
    );
    s.submit_renderer_subscription_with_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        0,
        Some(earph_core::renderer_events::RendererSubscriptionAdmission {
            lease: std::sync::Arc::new(OwnedEventSlots {
                id: lease_id,
                service,
                slots: [
                    String::with_capacity(service.event_slot_bytes()),
                    String::with_capacity(service.event_slot_bytes()),
                ],
            }),
            response_deadline_monotonic_ns: 15_000_000_000,
        }),
        |_| Ok(()),
    )
}
#[test]
fn u4_subscribe_without_a_source_retains_a_complete_initial_and_the_actual_slot_lease() {
    use earph_core::renderer_events::RendererService;
    let mut s = event_owner();
    let result = subscribe(
        &mut s,
        "initial-without-source",
        1,
        RendererService::AvTransport,
    );
    assert_eq!(result.receipt.status, ReceiptStatus::Applied);
    let info = result.subscription.unwrap();
    assert_eq!(info.timeout_seconds, 60);
    assert_eq!(info.expires_monotonic_ns, 60_000_000_000);
    assert_eq!(s.renderer_subscription_count(), 1);
    assert!(s.network_source().is_none());
    assert!(!format!("{:?}", info).contains("uuid:"));
    assert!(
        !serde_json::to_string(&s.checkpoint())
            .unwrap()
            .contains("uuid:")
    );
}

fn subscription_action(
    s: &mut Session,
    id: &str,
    info: &earph_core::renderer_events::RendererSubscriptionInfo,
    action: Value,
    now: u64,
) -> earph_core::renderer_events::RendererSubscriptionResult {
    let c = command(
        s,
        id,
        json!({"method":"renderer.subscription","controller_id":info.binding.controller_id,
        "grant_generation":info.binding.controller_generation,"callback_generation":info.binding.callback_generation,"action":action}),
    );
    s.submit_renderer_subscription_with_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        now,
        None,
        |_| Ok(()),
    )
}
#[test]
fn u4_initial_waits_for_response_shutdown_and_latest_coalesces_without_changing_it() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let info = subscribe(&mut s, "ordered-initial", 1, RendererService::AvTransport)
        .subscription
        .unwrap();
    assert!(s.begin_renderer_delivery(&info.binding, 0).is_err());
    renderer_request(
        &mut s,
        "event-stage-a",
        "controller-a",
        stage(0, "http://private.example/a", "first"),
        0,
    );
    renderer_request(
        &mut s,
        "event-stage-b",
        "controller-a",
        stage(1, "http://private.example/b", "second"),
        0,
    );
    s.acknowledge_renderer_subscription_response(&info.binding, true, 100)
        .unwrap();
    let initial = s.begin_renderer_delivery(&info.binding, 100).unwrap();
    assert_eq!(initial.sequence, 0);
    assert!(initial.initial);
    assert!(initial.snapshot.descriptor.is_none());
    assert_eq!(initial.snapshot.transport_state, "NO_MEDIA_PRESENT");
    assert_eq!(initial.deadline_monotonic_ns, 30_000_000_100);
    assert!(s.begin_renderer_delivery(&info.binding, 101).is_err());
    s.complete_renderer_delivery(&initial, RendererDeliveryOutcome::Delivered, true, 110)
        .unwrap();
    assert!(
        s.begin_renderer_delivery(&info.binding, 5_000_000_099)
            .is_err()
    );
    let latest = s
        .begin_renderer_delivery(&info.binding, 5_000_000_100)
        .unwrap();
    assert_eq!(latest.sequence, 1);
    assert!(!latest.initial);
    assert_eq!(
        latest.snapshot.descriptor.as_ref().unwrap().metadata(),
        "second"
    );
    assert!(initial.snapshot.descriptor.is_none());
    renderer_request(
        &mut s,
        "event-stage-c",
        "controller-a",
        stage(2, "http://private.example/c", "third"),
        5_000_000_101,
    );
    assert_eq!(
        latest.snapshot.descriptor.as_ref().unwrap().metadata(),
        "second"
    );
    s.complete_renderer_delivery(
        &latest,
        RendererDeliveryOutcome::ConnectFailed,
        true,
        5_000_000_102,
    )
    .unwrap();
    let next = s
        .begin_renderer_delivery(&info.binding, 10_000_000_100)
        .unwrap();
    assert_eq!(next.sequence, 2);
    assert_eq!(
        next.snapshot.descriptor.as_ref().unwrap().metadata(),
        "third"
    );
    assert_eq!(s.renderer_subscription_count(), 1);
}
#[test]
fn u4_unsubscribe_preserves_one_initial_attempt_and_removes_renewal() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let info = subscribe(&mut s, "orphan", 1, RendererService::RenderingControl)
        .subscription
        .unwrap();
    let result = subscription_action(
        &mut s,
        "orphan-unsubscribe",
        &info,
        json!({"operation":"unsubscribe","sid":info.binding.sid}),
        1,
    );
    assert_eq!(result.receipt.status, ReceiptStatus::Applied);
    assert_eq!(s.renderer_subscription_count(), 1);
    let renewal = subscription_action(
        &mut s,
        "orphan-renew",
        &info,
        json!({"operation":"renew","sid":info.binding.sid,"timeout_seconds":1800}),
        2,
    );
    assert_eq!(renewal.receipt.status, ReceiptStatus::Rejected);
    assert_eq!(
        renewal.receipt.error.unwrap().code,
        earph_core::ErrorCode::StaleSession
    );
    s.acknowledge_renderer_subscription_response(&info.binding, true, 3)
        .unwrap();
    let initial = s.begin_renderer_delivery(&info.binding, 4).unwrap();
    assert_eq!(initial.sequence, 0);
    assert!(initial.initial);
    assert!(s.renderer_delivery_authority(&initial, 5).is_some());
    s.complete_renderer_delivery(&initial, RendererDeliveryOutcome::Delivered, true, 6)
        .unwrap();
    assert_eq!(s.renderer_subscription_count(), 0);
    assert!(s.begin_renderer_delivery(&info.binding, 7).is_err());
}
#[test]
fn u4_delivery_timeout_abandons_the_event_but_keeps_the_live_sid() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let info = subscribe(&mut s, "timeout-sid", 1, RendererService::RenderingControl)
        .subscription
        .unwrap();
    s.acknowledge_renderer_subscription_response(&info.binding, true, 1)
        .unwrap();
    let initial = s.begin_renderer_delivery(&info.binding, 1).unwrap();
    assert_eq!(
        submit_at(
            &mut s,
            "event-unmute",
            json!({"method":"mute.set","muted":false}),
            2
        )
        .status,
        ReceiptStatus::Applied
    );
    s.expire_network_authority(30_000_000_002).unwrap();
    assert!(
        s.renderer_delivery_authority(&initial, 30_000_000_002)
            .is_none()
    );
    assert_eq!(
        s.renderer_deliveries_needing_cancellation(30_000_000_002)
            .len(),
        1
    );
    s.complete_renderer_delivery(
        &initial,
        RendererDeliveryOutcome::Timeout,
        true,
        30_000_000_003,
    )
    .unwrap();
    assert_eq!(s.renderer_subscription_count(), 1);
    let next = s
        .begin_renderer_delivery(&info.binding, 30_000_000_004)
        .unwrap();
    assert_eq!(next.sequence, 1);
    assert!(!next.snapshot.muted);
}
#[test]
fn u4_callback_revoke_invalidates_live_views_and_retains_a_failed_join() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let info = subscribe(
        &mut s,
        "revoke-flight",
        1,
        RendererService::RenderingControl,
    )
    .subscription
    .unwrap();
    s.acknowledge_renderer_subscription_response(&info.binding, true, 1)
        .unwrap();
    let initial = s.begin_renderer_delivery(&info.binding, 1).unwrap();
    let revoke = json!({"method":"renderer.callbacks.revoke","controller_id":"controller-a","expected_controller_generation":1,"expected_generation":1});
    assert_eq!(
        submit_at(&mut s, "callback-revoke", revoke, 2).status,
        ReceiptStatus::Applied
    );
    assert!(s.renderer_delivery_authority(&initial, 2).is_none());
    assert_eq!(s.renderer_deliveries_needing_cancellation(2).len(), 1);
    assert!(
        s.complete_renderer_delivery(&initial, RendererDeliveryOutcome::JoinFailed, false, 3)
            .is_err()
    );
    assert_eq!(s.renderer_subscription_count(), 1);
    let mut forged = (*initial).clone();
    forged.delivery_id += 1;
    assert!(
        s.complete_renderer_delivery(&forged, RendererDeliveryOutcome::Cancelled, true, 4)
            .is_err()
    );
    assert_eq!(s.renderer_subscription_count(), 1);
    s.complete_renderer_delivery(&initial, RendererDeliveryOutcome::Cancelled, true, 5)
        .unwrap();
    assert_eq!(s.renderer_subscription_count(), 0);
    assert_eq!(s.renderer_callback_generation(), 2);
}
#[test]
fn u4_subscription_admission_reserves_four_initial_positions_atomically() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let mut infos = Vec::new();
    for id in 1..=4 {
        let result = subscribe(
            &mut s,
            &format!("position-{id}"),
            id,
            RendererService::RenderingControl,
        );
        assert_eq!(result.receipt.status, ReceiptStatus::Applied);
        infos.push(result.subscription.unwrap());
    }
    let before = s.snapshot();
    let rejected = subscribe(
        &mut s,
        "no-initial-position",
        5,
        RendererService::RenderingControl,
    );
    assert_eq!(rejected.receipt.status, ReceiptStatus::Rejected);
    assert_eq!(
        rejected.receipt.error.unwrap().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(s.snapshot(), before);
    assert_eq!(s.renderer_subscription_count(), 4);
    s.acknowledge_renderer_subscription_response(&infos[0].binding, true, 0)
        .unwrap();
    let initial = s.begin_renderer_delivery(&infos[0].binding, 0).unwrap();
    s.complete_renderer_delivery(&initial, RendererDeliveryOutcome::Delivered, true, 0)
        .unwrap();
    assert_eq!(
        subscribe(
            &mut s,
            "position-released",
            5,
            RendererService::RenderingControl
        )
        .receipt
        .status,
        ReceiptStatus::Applied
    );
    assert_eq!(s.renderer_subscription_count(), 5);
}
#[test]
fn u4_failed_response_and_response_deadline_release_the_exact_sid() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let first = subscribe(
        &mut s,
        "response-failed",
        1,
        RendererService::ConnectionManager,
    )
    .subscription
    .unwrap();
    s.acknowledge_renderer_subscription_response(&first.binding, false, 1)
        .unwrap();
    assert_eq!(s.renderer_subscription_count(), 0);
    let second = subscribe(
        &mut s,
        "response-timeout",
        2,
        RendererService::ConnectionManager,
    )
    .subscription
    .unwrap();
    s.expire_network_authority(15_000_000_000).unwrap();
    assert_eq!(s.renderer_subscription_count(), 0);
    assert!(
        s.acknowledge_renderer_subscription_response(&second.binding, true, 15_000_000_001)
            .is_err()
    );
    assert!(
        s.begin_renderer_delivery(&second.binding, 15_000_000_001)
            .is_err()
    );
}
#[test]
fn u4_invalid_xml_metadata_rejects_staging_before_mutation() {
    let mut s = supported();
    let before = s.snapshot();
    let receipt = renderer_request(
        &mut s,
        "invalid-event-xml",
        "controller-a",
        stage(0, "http://private.example/a", "\u{0001}"),
        0,
    );
    assert_eq!(receipt.status, ReceiptStatus::Rejected);
    assert_eq!(s.snapshot(), before);
}

#[test]
fn u4_unknown_position_remains_unknown_in_the_shared_getter_projection() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    renderer_request(
        &mut s,
        "unknown-position-stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    let p = s.renderer_projection(
        RendererService::AvTransport,
        &RendererControllerId::new("controller-a").unwrap(),
        1,
    );
    let mut fields = std::collections::BTreeMap::new();
    p.fields(|name, value| {
        fields.insert(name, value.to_string());
    });
    for name in [
        "CurrentMediaDuration",
        "CurrentTrackDuration",
        "RelativeTimePosition",
        "AbsoluteTimePosition",
    ] {
        assert_eq!(
            fields.get(name).map(String::as_str),
            Some("NOT_IMPLEMENTED"),
            "{name}"
        );
    }
    assert_eq!(
        fields.get("RelativeCounterPosition").map(String::as_str),
        Some("2147483647")
    );
}
#[test]
fn u4_explicit_play_can_capture_a_fresh_pin_after_a_competing_local_attempt() {
    let mut s = supported();
    media_grants(&mut s);
    renderer_request(
        &mut s,
        "retry-stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    play(&mut s, "controller-a", "retry-play", 1);
    measured_renderer_ready(&mut s, 10);
    let source = s.network_source().unwrap().binding.clone();
    let authorization = s
        .renderer_graph_continuation()
        .unwrap()
        .authorization
        .clone();
    let local = renderer_prepare(&s, "competing-local-graph");
    assert_eq!(
        s.submit_graph(local.clone(), 11).status,
        ReceiptStatus::Accepted
    );
    assert!(s.renderer_graph_continuation().is_none());
    let job = s.graph_job().unwrap();
    assert!(
        s.complete_graph_preparation(
            &local.request_id,
            &job.binding,
            12,
            Err(earph_core::EngineError::new(
                earph_core::ErrorCode::GraphInvalid,
                "controlled compiler refusal"
            )
            .into())
        )
        .is_err()
    );
    let replay = command(
        &s,
        "explicit-fresh-attempt",
        action(
            "controller-a",
            json!({"operation":"play","media_id":"staged-1","staging_generation":1,"grant_generation":1,"device_id":"dac"}),
        ),
    );
    let result = s.submit_renderer_with_network_effects(
        replay,
        "192.168.1.2".parse().unwrap(),
        13,
        |_, _| panic!("same binding must not fetch again"),
        |_| Ok(()),
    );
    assert_eq!(result.status, ReceiptStatus::Applied);
    assert_eq!(s.network_source().unwrap().binding, source);
    assert_eq!(
        s.renderer_graph_continuation().unwrap().authorization,
        authorization
    );
    assert!(s.snapshot().muted);
}

#[test]
fn u4_listener_retirement_keeps_queries_valid_while_exact_deliveries_close() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let info = subscribe(
        &mut s,
        "listener-retire",
        1,
        RendererService::RenderingControl,
    )
    .subscription
    .unwrap();
    s.acknowledge_renderer_subscription_response(&info.binding, true, 1)
        .unwrap();
    let initial = s.begin_renderer_delivery(&info.binding, 1).unwrap();
    let epoch = s.renderer_epoch();
    s.retire_renderer_epoch(epoch, 2).unwrap();
    assert_eq!(s.renderer_epoch(), epoch + 1);
    assert!(
        serde_json::from_value::<earph_core::renderer::RendererSnapshot>(
            serde_json::to_value(s.renderer_snapshot()).unwrap()
        )
        .is_ok()
    );
    assert!(s.renderer_delivery_authority(&initial, 2).is_none());
    assert_eq!(s.renderer_subscription_count(), 1);
    s.complete_renderer_delivery(&initial, RendererDeliveryOutcome::Cancelled, true, 3)
        .unwrap();
    assert_eq!(s.renderer_subscription_count(), 0);
}

#[test]
fn u4_http_callback_default_port_uses_the_explicit_local_origin_without_rewriting_the_url() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let c = command(
        &s,
        "default-callback-port",
        json!({"method":"renderer.subscription","controller_id":"controller-a","grant_generation":1,"callback_generation":1,
        "action":{"operation":"subscribe","service":"rendering_control","callbacks":["http://private.example/events/@session"],"timeout_seconds":10}}),
    );
    let result = s.submit_renderer_subscription_with_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        0,
        Some(RendererSubscriptionAdmission {
            lease: std::sync::Arc::new(OwnedEventSlots {
                id: 1,
                service: RendererService::RenderingControl,
                slots: [String::with_capacity(2048), String::with_capacity(2048)],
            }),
            response_deadline_monotonic_ns: 10_000_000_000,
        }),
        |_| Ok(()),
    );
    assert_eq!(
        result.receipt.status,
        ReceiptStatus::Applied,
        "{:?}",
        result.receipt
    );
    let info = result.subscription.unwrap();
    s.acknowledge_renderer_subscription_response(&info.binding, true, 0)
        .unwrap();
    let initial = s.begin_renderer_delivery(&info.binding, 0).unwrap();
    assert_eq!(
        initial.callbacks[0],
        "http://private.example/events/@session"
    );
}

#[test]
fn u4_invalid_callback_replacement_preserves_authority_and_existing_delivery() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let info = subscribe(
        &mut s,
        "callback-stability",
        1,
        RendererService::RenderingControl,
    )
    .subscription
    .unwrap();
    s.acknowledge_renderer_subscription_response(&info.binding, true, 0)
        .unwrap();
    let initial = s.begin_renderer_delivery(&info.binding, 0).unwrap();
    for (i, origin, pin) in [
        (0, "https://private.example:80", "192.168.1.2"),
        (1, "http://private.example", "192.168.1.2"),
        (2, "http://private.example:80", "127.0.0.1"),
        (3, "http://private.example:80", "192.168.1.3"),
        (4, "http://user@private.example:80", "192.168.1.2"),
    ] {
        let before = s.snapshot();
        let replacement = json!({"method":"renderer.callbacks.replace","controller_id":"controller-a","expected_controller_generation":1,"expected_generation":1,"lifetime_ms":60000,
            "grants":{"origins":[{"origin":origin,"pins":[pin]}]}});
        assert_eq!(
            submit_at(&mut s, &format!("invalid-callback-{i}"), replacement, 0).status,
            ReceiptStatus::Rejected
        );
        assert_eq!(s.snapshot(), before);
        assert_eq!(s.renderer_callback_generation(), 1);
        assert!(s.renderer_delivery_authority(&initial, 0).is_some());
    }
}
#[test]
fn u4_renewal_does_not_extend_authority_or_reset_the_initial_or_sequence() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let info = subscribe(&mut s, "renewal", 1, RendererService::RenderingControl)
        .subscription
        .unwrap();
    let result = subscription_action(
        &mut s,
        "bounded-renewal",
        &info,
        json!({"operation":"renew","sid":info.binding.sid,"timeout_seconds":1800}),
        500_000_000,
    );
    assert_eq!(result.receipt.status, ReceiptStatus::Applied);
    let renewed = result.subscription.unwrap();
    assert_eq!(renewed.binding, info.binding);
    assert_eq!(renewed.timeout_seconds, 59);
    assert_eq!(renewed.expires_monotonic_ns, 60_000_000_000);
    s.acknowledge_renderer_subscription_response(&info.binding, true, 500_000_001)
        .unwrap();
    let initial = s
        .begin_renderer_delivery(&info.binding, 500_000_001)
        .unwrap();
    assert_eq!(initial.sequence, 0);
    s.complete_renderer_delivery(
        &initial,
        RendererDeliveryOutcome::Delivered,
        true,
        500_000_002,
    )
    .unwrap();
    submit_at(
        &mut s,
        "renewal-mute",
        json!({"method":"mute.set","muted":false}),
        500_000_003,
    );
    let next = s
        .begin_renderer_delivery(&info.binding, 5_500_000_001)
        .unwrap();
    assert_eq!(next.sequence, 1);
    s.complete_renderer_delivery(
        &next,
        RendererDeliveryOutcome::InvalidSid,
        true,
        5_500_000_002,
    )
    .unwrap();
    assert_eq!(s.renderer_subscription_count(), 0);
}
#[test]
fn u4_maximum_record_count_includes_idle_subscriptions_and_412_targets_one_sid() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let mut bindings = Vec::new();
    for id in 1..=16 {
        let result = subscribe(
            &mut s,
            &format!("sid-{id}"),
            id,
            RendererService::RenderingControl,
        );
        assert_eq!(result.receipt.status, ReceiptStatus::Applied);
        let info = result.subscription.unwrap();
        s.acknowledge_renderer_subscription_response(&info.binding, true, 0)
            .unwrap();
        let initial = s.begin_renderer_delivery(&info.binding, 0).unwrap();
        s.complete_renderer_delivery(&initial, RendererDeliveryOutcome::Delivered, true, 0)
            .unwrap();
        bindings.push(info.binding);
    }
    let before = s.snapshot();
    assert_eq!(
        subscribe(&mut s, "seventeenth", 17, RendererService::RenderingControl)
            .receipt
            .status,
        ReceiptStatus::Rejected
    );
    assert_eq!(s.snapshot(), before);
    assert_eq!(s.renderer_subscription_count(), 16);
    submit_at(
        &mut s,
        "sixteen-mute",
        json!({"method":"mute.set","muted":false}),
        1,
    );
    let one = s
        .begin_renderer_delivery(&bindings[0], 5_000_000_000)
        .unwrap();
    let two = s
        .begin_renderer_delivery(&bindings[1], 5_000_000_000)
        .unwrap();
    s.complete_renderer_delivery(
        &one,
        RendererDeliveryOutcome::InvalidSid,
        true,
        5_000_000_001,
    )
    .unwrap();
    assert_eq!(s.renderer_subscription_count(), 15);
    assert!(s.renderer_delivery_authority(&two, 5_000_000_001).is_some());
}
#[test]
fn u4_callback_and_subscription_authority_never_survive_restart() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    subscribe(&mut s, "restart-sid", 1, RendererService::AvTransport);
    let checkpoint = serde_json::to_vec(&s.checkpoint()).unwrap();
    let serialized = String::from_utf8(checkpoint.clone()).unwrap();
    assert!(!serialized.contains("private.example"));
    assert!(!serialized.contains("uuid:"));
    let restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("replacement-owner").unwrap(),
        ControlLimits::default(),
        serde_json::from_slice(&checkpoint).unwrap(),
    )
    .unwrap();
    assert_eq!(restored.renderer_subscription_count(), 0);
    assert_eq!(restored.renderer_callback_generation(), 0);
    assert!(restored.renderer_graph_continuation().is_none());
    assert!(!restored.renderer_snapshot().enabled);
}

#[test]
fn u4_event_ownership_and_callback_revocation_survive_the_shared_ledger_pressure() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    let info = subscribe(&mut s, "pressure-initial", 1, RendererService::AvTransport)
        .subscription
        .unwrap();
    s.acknowledge_renderer_subscription_response(&info.binding, true, 0)
        .unwrap();
    let initial = s.begin_renderer_delivery(&info.binding, 0).unwrap();
    let mut refused = false;
    for generation in 0..512 {
        let result = renderer_request(
            &mut s,
            &format!("pressure-stage-{generation}"),
            "controller-a",
            stage(
                generation,
                &format!("http://private.example/{generation}"),
                &"x".repeat(65536),
            ),
            0,
        );
        if result.status == ReceiptStatus::Rejected {
            assert_eq!(
                result.error.unwrap().code,
                earph_core::ErrorCode::ResourceBudgetExceeded
            );
            refused = true;
            break;
        }
    }
    assert!(refused);
    assert!(s.network_retained_bytes().unwrap() <= 16 * 1024 * 1024);
    assert_eq!(s.renderer_subscription_count(), 1);
    assert!(initial.snapshot.descriptor.is_none());
    let revoke = json!({"method":"renderer.callbacks.revoke","controller_id":"controller-a","expected_controller_generation":1,"expected_generation":1});
    assert_eq!(
        submit_at(&mut s, "pressure-revoke", revoke, 0).status,
        ReceiptStatus::Applied
    );
    assert!(s.renderer_delivery_authority(&initial, 0).is_none());
    assert_eq!(s.renderer_subscription_count(), 1);
    s.complete_renderer_delivery(&initial, RendererDeliveryOutcome::Cancelled, true, 0)
        .unwrap();
    assert_eq!(s.renderer_subscription_count(), 0);
    assert!(s.network_retained_bytes().unwrap() <= 16 * 1024 * 1024);
}
#[test]
fn u4_shared_playback_arcs_are_charged_once_across_controller_authorities_and_receipts() {
    use earph_core::renderer::*;
    use std::sync::Arc;
    let baseline = supported();
    let authorization: RendererPlaybackAuthorization =
        serde_json::from_value(playback_authorization(&baseline)).unwrap();
    let shared = Arc::new(authorization);
    let separate = Arc::new(RendererPlaybackAuthorization {
        intent: Arc::new(earph_core::graph_control::GraphIntent {
            graph: Arc::new((*shared.intent.graph).clone()),
            processing: shared.intent.processing,
        }),
        output: shared.output.clone(),
        expected_device_epoch: shared.expected_device_epoch,
    });
    let mut states = [baseline.clone(), baseline];
    for (index, state) in states.iter_mut().enumerate() {
        let grants = Arc::new(RendererGrantDocument {
            controllers: vec![
                RendererControllerGrant {
                    id: RendererControllerId::new("a").unwrap(),
                    peers: vec!["192.168.1.2".parse().unwrap()],
                    rights: vec![RendererRight::Transport],
                    playback: Some(shared.clone()),
                },
                RendererControllerGrant {
                    id: RendererControllerId::new("b").unwrap(),
                    peers: vec!["192.168.1.3".parse().unwrap()],
                    rights: vec![RendererRight::Transport],
                    playback: Some(if index == 0 {
                        shared.clone()
                    } else {
                        separate.clone()
                    }),
                },
            ],
        });
        let mut request = command(state, "retained-pins", json!({"method":"state.get"}));
        request.payload = Command::RendererGrantsReplace {
            expected_generation: 1,
            grants,
            lifetime_ms: 60000,
        };
        assert_eq!(state.submit(request).status, ReceiptStatus::Applied);
    }
    assert!(
        states[1].network_retained_bytes().unwrap()
            >= states[0].network_retained_bytes().unwrap()
                + separate.intent.graph.retained_capacity_bytes().unwrap()
    );
}

#[test]
fn u4_avt_unsupported_counter_and_next_fields_follow_the_service_contract() {
    use earph_core::renderer_events::*;
    let s = event_owner();
    let projection = s.renderer_projection(
        RendererService::AvTransport,
        &RendererControllerId::new("controller-a").unwrap(),
        1,
    );
    let mut fields = std::collections::BTreeMap::new();
    projection.fields(|name, value| {
        fields.insert(name, value.to_string());
    });
    assert_eq!(fields["RelativeCounterPosition"], "2147483647");
    assert_eq!(fields["AbsoluteCounterPosition"], "2147483647");
    assert_eq!(fields["NextAVTransportURI"], "NOT_IMPLEMENTED");
    assert_eq!(fields["NextAVTransportURIMetaData"], "NOT_IMPLEMENTED");
    assert_eq!(fields["CurrentMediaDuration"], "00:00:00");
    assert_eq!(fields["PlaybackStorageMedium"], "NONE");
    assert_eq!(fields["CurrentTransportActions"], "");
}

#[test]
fn u4_getters_reflect_controller_transport_rights_and_current_preset_availability() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    renderer_request(
        &mut s,
        "getter-stage",
        "controller-a",
        stage(0, "http://private.example/a", ""),
        0,
    );
    let controller = RendererControllerId::new("controller-a").unwrap();
    let mut fields = std::collections::BTreeMap::new();
    s.renderer_projection(RendererService::AvTransport, &controller, 1)
        .fields(|name, value| {
            fields.insert(name, value.to_string());
        });
    assert_eq!(fields["CurrentTransportActions"], "");
    let mut presets = None;
    s.renderer_projection(RendererService::RenderingControl, &controller, 1)
        .fields(|name, value| {
            if name == "PresetNameList" {
                presets = Some(value.to_string());
            }
        });
    assert_eq!(presets.as_deref(), Some(""));
}

#[test]
fn u4_callback_generation_lookup_preserves_surviving_controller_scope() {
    use earph_core::renderer_events::*;
    let mut s = owner();
    s.enable_renderer_control();
    let mut grants = grants(0, 60000);
    grants["grants"]["controllers"][0]["rights"] = json!(["events"]);
    grants["grants"]["controllers"].as_array_mut().unwrap().push(json!({"id":"controller-b","peers":["192.168.1.3"],"rights":["events"],"playback":null}));
    assert_eq!(
        submit_at(&mut s, "two-controllers", grants, 0).status,
        ReceiptStatus::Applied
    );
    for (index, controller, origin, peer) in [
        (
            0,
            "controller-a",
            "http://a.private.example:80",
            "192.168.1.2",
        ),
        (
            1,
            "controller-b",
            "http://b.private.example:80",
            "192.168.1.3",
        ),
    ] {
        let callback = json!({"method":"renderer.callbacks.replace","controller_id":controller,"expected_controller_generation":1,"expected_generation":index,"lifetime_ms":60000,"grants":{"origins":[{"origin":origin,"pins":[peer]}]}});
        assert_eq!(
            submit_at(&mut s, &format!("scoped-callback-{index}"), callback, 0).status,
            ReceiptStatus::Applied
        );
    }
    let a = RendererControllerId::new("controller-a").unwrap();
    let b = RendererControllerId::new("controller-b").unwrap();
    assert_eq!(s.renderer_callback_generation(), 2);
    let generation = s
        .renderer_callback_generation_for(&a, 1, 0)
        .expect("the owner must inspect A's exact callback generation after B changes");
    assert_eq!(generation, 1);
    assert_eq!(s.renderer_callback_generation_for(&b, 1, 0), Some(2));
    assert_eq!(s.renderer_callback_generation_for(&a, 2, 0), None);
    let c = command(
        &s,
        "scoped-a-subscribe",
        json!({"method":"renderer.subscription","controller_id":"controller-a","grant_generation":1,"callback_generation":generation,
        "action":{"operation":"subscribe","service":"rendering_control","callbacks":["http://a.private.example:80/events"],"timeout_seconds":60}}),
    );
    let result = s.submit_renderer_subscription_with_effects(
        c,
        "192.168.1.2".parse().unwrap(),
        0,
        Some(RendererSubscriptionAdmission {
            lease: std::sync::Arc::new(OwnedEventSlots {
                id: 1,
                service: RendererService::RenderingControl,
                slots: [String::with_capacity(2048), String::with_capacity(2048)],
            }),
            response_deadline_monotonic_ns: 15_000_000_000,
        }),
        |_| Ok(()),
    );
    assert_eq!(result.receipt.status, ReceiptStatus::Applied);
    assert_eq!(result.subscription.unwrap().binding.callback_generation, 1);
}

#[test]
fn u4_shared_field_debug_redacts_uri_and_metadata() {
    use earph_core::renderer_events::*;
    let mut s = event_owner();
    renderer_request(
        &mut s,
        "debug-fields",
        "controller-a",
        stage(
            0,
            "http://private.example/secret-field",
            "never-log-this-metadata",
        ),
        0,
    );
    let projection = s.renderer_projection(
        RendererService::AvTransport,
        &RendererControllerId::new("controller-a").unwrap(),
        1,
    );
    let mut diagnostics = String::new();
    projection.fields(|name, value| {
        diagnostics.push_str(&format!("{name}={value:?}\n"));
    });
    assert!(!diagnostics.contains("private.example"));
    assert!(!diagnostics.contains("never-log-this-metadata"));
}
