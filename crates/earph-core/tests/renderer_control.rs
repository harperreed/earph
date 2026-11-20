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
        "id":"controller-a","peers":["192.168.1.2"],"rights":["stage","transport"]
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
    let mut s = owner();
    s.enable_local_playback();
    s.enable_network_playback();
    s.enable_renderer_control();
    let mut document = grants(0, 1000);
    document["grants"]["controllers"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id":"controller-b","peers":["192.168.1.3"],"rights":["stage","transport","takeover"]
        }));
    for (id, payload) in [
        ("controllers", document),
        (
            "output",
            json!({"method":"output.select","output":{
                "backend":"alsa","route":"system","endpoint":"unopened-fixture","device_id":"dac","profile_id":"profile",
                "period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"
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
    s
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
fn renderer_metadata_pressure_shares_the_media_budget_and_keeps_exact_stop_available() {
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
fn controller_and_media_grant_generations_remain_separate_authorities() {
    let mut s = supported();
    assert_eq!(
        submit_at(&mut s, "replace-controller", grants(1, 1000), 0).status,
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
