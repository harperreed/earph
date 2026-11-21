// ABOUTME: Exercises protected retention and the ordered Pairing PSK commit boundary.
// ABOUTME: Uses injected synthetic keys, monotonic clocks, and explicit local completions.
use earph_sendspin::{
    noise::{Category, SENTINEL, Selection},
    pairing::*,
};
use serde_json::json;

fn record(server: u8, order: u64) -> PairingRecord {
    PairingRecord {
        server_public: [server; 32],
        psk: [server + 30; 32],
        last_used_order: order,
    }
}
fn catalog() -> RecordCatalog {
    let mut catalog = RecordCatalog::new(5).unwrap();
    for i in 1..=5 {
        catalog = catalog
            .plan_replace(record(i, u64::from(i)), &[])
            .unwrap()
            .replacement;
    }
    catalog
}
fn client(category: Category) -> PairingClient {
    PairingClient::new(
        PairingContext {
            server_public: [20; 32],
            selection: Selection {
                category,
                sentinel_fallback: false,
            },
            connection_generation: 4,
            handshake_generation: 9,
            pairing_psk: [7; 32],
        },
        catalog(),
        vec![],
    )
    .unwrap()
}
fn activate(method: &str) -> PairingEvent {
    PairingEvent::Activate(
        json!({"activities":["pairing"],"active_roles":[],"pairing":{"method":method}}),
    )
}
fn begin(client: &mut PairingClient, now: u64) -> AttemptId {
    let effects = client.handle(activate("pairing_psk"), now);
    let [PairingEffect::RequestCandidate(id)] = effects.as_slice() else {
        panic!("candidate not requested: {effects:?}")
    };
    *id
}
fn flight(client: &mut PairingClient, id: AttemptId, now: u64) -> PairingFlight {
    let effects = client.handle(
        PairingEvent::CandidateReady(id, Candidate::new([90; 32])),
        now,
    );
    let [PairingEffect::SendFlight(actual, flight)] = effects.as_slice() else {
        panic!("flight not produced")
    };
    assert_eq!(*actual, id);
    flight.clone()
}
fn waiting(client: &mut PairingClient) -> AttemptId {
    let id = begin(client, 10);
    flight(client, id, 11);
    mark_emission(client, id, 12);
    assert!(
        client
            .handle(PairingEvent::PairingFlightSent(id, 12), 13)
            .is_empty()
    );
    assert_eq!(client.phase(), PairingPhase::AwaitServerFinalize);
    id
}
fn mark_emission(client: &mut PairingClient, id: AttemptId, first: u64) {
    assert!(
        client
            .handle(PairingEvent::PairingInitSent(id, first), first)
            .is_empty()
    );
    assert!(
        client
            .handle(PairingEvent::PairingFinalizeStarted(id), first)
            .is_empty()
    );
}
fn ack(client: &mut PairingClient, now: u64) -> (AttemptId, RecordUpdate) {
    let effects = client.handle(PairingEvent::ServerFinalize(json!({"future":true})), now);
    let [PairingEffect::CommitRecord(id, update)] = effects.as_slice() else {
        panic!("ack must produce one commit: {effects:?}")
    };
    (*id, update.clone())
}

#[test]
fn catalog_plans_complete_replacement_without_mutating_committed_state() {
    let catalog = catalog();
    let update = catalog
        .plan_replace(record(6, 6), &[record(1, 1).psk_id()])
        .unwrap();
    assert_eq!(catalog.records().len(), 5);
    assert!(catalog.records().contains(&record(2, 2)));
    assert_eq!(update.replacement.records().len(), 5);
    assert!(update.replacement.records().contains(&record(1, 1)));
    assert!(!update.replacement.records().contains(&record(2, 2)));
    assert!(update.replacement.records().contains(&record(6, 6)));
}

#[test]
fn protected_same_server_versions_share_catalog_and_release_removes_only_superseded() {
    let mut replacement = record(1, 6);
    replacement.psk = [98; 32];
    let old = record(1, 1);
    let update = catalog()
        .plan_replace(replacement.clone(), &[old.psk_id()])
        .unwrap();
    assert!(update.replacement.records().contains(&old));
    assert!(update.replacement.records().contains(&replacement));
    let released = update.replacement.plan_release(&[]).unwrap().replacement;
    assert!(!released.records().contains(&old));
    assert!(released.records().contains(&replacement));
    assert!(released.superseded_ids().is_empty());
}

#[test]
fn admission_rejects_all_protected_before_attempt_and_never_overflows() {
    let catalog = catalog();
    let protected: Vec<_> = catalog
        .records()
        .iter()
        .map(PairingRecord::psk_id)
        .collect();
    assert_eq!(
        catalog.ensure_admission(&protected),
        Err(PairingError::AdmissionInvariant)
    );
    assert_eq!(
        catalog.plan_replace(record(6, 6), &protected).unwrap_err(),
        PairingError::AdmissionInvariant
    );
    assert!(RecordCatalog::new(4).is_err());
    let context = PairingContext {
        server_public: [20; 32],
        selection: Selection {
            category: Category::Pairing,
            sentinel_fallback: false,
        },
        connection_generation: 1,
        handshake_generation: 1,
        pairing_psk: [7; 32],
    };
    assert!(PairingClient::new(context, catalog, protected).is_err());
}

#[test]
fn lru_ties_use_stable_psk_identifier_and_updates_derive_identifiers() {
    let records: Vec<_> = (1..=5).map(|i| record(i, 1)).collect();
    let victim = records.iter().min_by_key(|r| r.psk_id()).unwrap().psk_id();
    let catalog = RecordCatalog::from_committed(5, records, vec![]).unwrap();
    let result = catalog.plan_replace(record(6, 2), &[]).unwrap();
    assert!(
        !result
            .replacement
            .records()
            .iter()
            .any(|r| r.psk_id() == victim)
    );
    let touched = catalog.plan_mark_used(victim, 8).unwrap().replacement;
    assert!(
        touched
            .records()
            .iter()
            .any(|r| r.psk_id() == victim && r.last_used_order == 8)
    );
}

#[test]
fn no_commit_before_ack_and_durable_success_leaves_current_session_unpaired() {
    let mut client = client(Category::Pairing);
    let id = waiting(&mut client);
    assert_eq!(client.catalog(), &catalog());
    let (committed, update) = ack(&mut client, 14);
    assert_eq!(committed, id);
    assert_eq!(client.catalog(), &catalog());
    assert_eq!(client.phase(), PairingPhase::Committing);
    assert!(
        client
            .handle(PairingEvent::CommitSucceeded(id), 15)
            .is_empty()
    );
    assert_eq!(client.catalog(), &update.replacement);
    assert_eq!(client.phase(), PairingPhase::RecordStored);
    assert_eq!(client.selection().category, Category::Pairing);
    assert!(!client.is_paired());
    assert!(client.handle(PairingEvent::Cancel, 16).is_empty());
    assert_eq!(client.catalog(), &update.replacement);
}

#[test]
fn flight_contains_only_immediate_indexed_init_then_unpadded_finalize() {
    let mut client = client(Category::Pairing);
    let id = begin(&mut client, 1);
    let messages = flight(&mut client, id, 2).messages();
    assert_eq!(
        messages[0],
        json!({"type":"client/pair-init","payload":{"pairing_index":1}})
    );
    assert_eq!(messages[1]["type"], "client/pair-finalize");
    assert_eq!(messages[1]["payload"].as_object().unwrap().len(), 1);
    assert_eq!(
        messages[1]["payload"]["long_term_psk"]
            .as_str()
            .unwrap()
            .len(),
        43
    );
    assert!(
        client
            .handle(
                PairingEvent::CandidateReady(id, Candidate::new([91; 32])),
                3
            )
            .is_empty()
    );
}

#[test]
fn sentinel_or_pairing_psk_candidates_request_fresh_material() {
    let mut client = client(Category::Pairing);
    let id = begin(&mut client, 0);
    for psk in [SENTINEL, [7; 32], record(3, 3).psk] {
        assert!(
            matches!(client.handle(PairingEvent::CandidateReady(id,Candidate::new(psk)),1).as_slice(),[PairingEffect::RequestCandidate(actual)] if *actual==id)
        );
    }
    flight(&mut client, id, 2);
}

#[test]
fn unsupported_method_advances_index_and_sentinel_never_exports_a_psk() {
    let mut client = client(Category::Pairing);
    assert!(matches!(
        client.handle(activate("static_pairing_code"), 0).as_slice(),
        [PairingEffect::Abort(_, AbortReason::MethodNotSupported)]
    ));
    assert_eq!(begin(&mut client, 1).pairing_index, 2);
    let mut sentinel = client_with_sentinel();
    assert!(matches!(
        sentinel.handle(activate("pairing_psk"), 0).as_slice(),
        [PairingEffect::Abort(_, AbortReason::MethodNotSupported)]
    ));
    assert_eq!(sentinel.catalog(), &catalog());
}
fn client_with_sentinel() -> PairingClient {
    client(Category::Sentinel)
}

#[test]
fn activation_rejection_precedence_checks_whole_hypothetical_admissibility() {
    for category in [Category::Pairing, Category::Sentinel] {
        for (payload, reason) in [
            (
                json!({"activities":["playback"],"active_roles":["player@v1"]}),
                GoodbyeReason::PairingRequired,
            ),
            (
                json!({"activities":[],"active_roles":["player@v1"]}),
                GoodbyeReason::PairingRequired,
            ),
            (
                json!({"activities":["pairing","playback"],"pairing":{"method":"unsupported"}}),
                GoodbyeReason::Unauthorized,
            ),
            (
                json!({"activities":["unknown"]}),
                GoodbyeReason::Unauthorized,
            ),
        ] {
            let mut client = client(category);
            assert_eq!(
                client.handle(PairingEvent::Activate(payload), 0),
                vec![
                    PairingEffect::Goodbye(reason),
                    PairingEffect::Close(CloseReason::Unauthorized)
                ]
            );
        }
    }
    let mut paired = client(Category::LongTerm);
    assert_eq!(
        paired.handle(activate("pairing_psk"), 0),
        vec![
            PairingEffect::Goodbye(GoodbyeReason::Unauthorized),
            PairingEffect::Close(CloseReason::Unauthorized)
        ]
    );
}

#[test]
fn cancellation_before_and_after_emission_has_distinct_guarantees() {
    let mut client = client(Category::Pairing);
    let id = begin(&mut client, 0);
    assert_eq!(
        client.handle(PairingEvent::Cancel, 1),
        vec![PairingEffect::Abort(id, AbortReason::UserCancelled)]
    );
    assert!(
        client
            .handle(
                PairingEvent::CandidateReady(id, Candidate::new([90; 32])),
                2
            )
            .is_empty()
    );
    assert!(
        client
            .handle(PairingEvent::ServerFinalize(json!({})), 3)
            .is_empty()
    );
    let id = begin(&mut client, 4);
    flight(&mut client, id, 5);
    mark_emission(&mut client, id, 5);
    assert_eq!(
        client.handle(PairingEvent::Cancel, 6),
        vec![PairingEffect::Finishing(id)]
    );
    client.handle(PairingEvent::PairingFlightSent(id, 5), 7);
    let (_, update) = ack(&mut client, 8);
    client.handle(PairingEvent::Cancel, 9);
    client.handle(PairingEvent::CommitSucceeded(id), 10);
    assert_eq!(client.catalog(), &update.replacement);
}

#[test]
fn cancelling_activation_after_finalize_discards_and_supersession_fences_completions() {
    let mut client = client(Category::Pairing);
    let old = waiting(&mut client);
    client.handle(
        PairingEvent::Activate(json!({"activities":[],"future":true})),
        14,
    );
    assert_eq!(client.phase(), PairingPhase::Discarding);
    assert!(
        client
            .handle(PairingEvent::ServerFinalize(json!({})), 15)
            .is_empty()
    );
    let id = begin(&mut client, 16);
    assert_eq!(id.pairing_index, 2);
    assert!(
        client
            .handle(
                PairingEvent::CandidateReady(old, Candidate::new([91; 32])),
                17
            )
            .is_empty()
    );
    assert!(
        client
            .handle(PairingEvent::PairingFlightSent(old, 18), 18)
            .is_empty()
    );
    assert!(
        client
            .handle(PairingEvent::CommitSucceeded(old), 19)
            .is_empty()
    );
    flight(&mut client, id, 20);
    let newer = begin(&mut client, 21);
    assert_eq!(newer.pairing_index, 3);
    assert_eq!(client.catalog(), &catalog());
}

#[test]
fn timeout_uses_first_emission_and_handles_monotonic_overflow_without_wrapping() {
    let mut client = client(Category::Pairing);
    let id = begin(&mut client, 1);
    flight(&mut client, id, 2);
    mark_emission(&mut client, id, 3);
    client.handle(PairingEvent::PairingFlightSent(id, 3), 120_002);
    assert!(client.handle(PairingEvent::Tick, 120_002).is_empty());
    assert_eq!(
        client.handle(PairingEvent::Tick, 120_003),
        vec![PairingEffect::Abort(id, AbortReason::AttemptTimeout)]
    );
    assert!(
        client
            .handle(
                PairingEvent::RemoteAbort(AbortReason::ConcurrentAttempt),
                120_004
            )
            .is_empty()
    );
    let mut near_max = client_with_sentinel();
    assert!(near_max.handle(PairingEvent::Tick, u64::MAX).is_empty());
    assert_eq!(
        near_max.handle(PairingEvent::Tick, 0),
        vec![PairingEffect::Close(CloseReason::Clock)]
    );
}

#[test]
fn ack_requires_completed_flight_and_object_payload_and_duplicates_close() {
    for event in [
        PairingEvent::ServerFinalize(json!({})),
        PairingEvent::UnexpectedPairingMessage,
    ] {
        let mut client = client(Category::Pairing);
        begin(&mut client, 0);
        assert_eq!(
            client.handle(event, 1),
            vec![PairingEffect::Close(CloseReason::Protocol)]
        );
        assert_eq!(client.catalog(), &catalog());
    }
    for payload in [json!(null), json!([]), json!("{}")] {
        let mut client = client(Category::Pairing);
        waiting(&mut client);
        assert_eq!(
            client.handle(PairingEvent::ServerFinalize(payload), 14),
            vec![PairingEffect::Close(CloseReason::Protocol)]
        );
    }
    for committed in [false, true] {
        let mut client = client(Category::Pairing);
        let id = waiting(&mut client);
        ack(&mut client, 14);
        if committed {
            client.handle(PairingEvent::CommitSucceeded(id), 15);
        }
        assert_eq!(
            client.handle(PairingEvent::ServerFinalize(json!({})), 16),
            vec![PairingEffect::Close(CloseReason::Protocol)]
        );
    }
}

#[test]
fn commit_completion_precedes_queued_activation_and_disconnect() {
    let mut client = client(Category::Pairing);
    let id = waiting(&mut client);
    let (_, update) = ack(&mut client, 14);
    assert_eq!(
        client.handle(activate("pairing_psk"), 15),
        vec![PairingEffect::DeferUntilCommitted(id)]
    );
    assert_eq!(
        client.handle(PairingEvent::Disconnected, 16),
        vec![PairingEffect::DeferUntilCommitted(id)]
    );
    client.handle(PairingEvent::CommitSucceeded(id), 17);
    assert_eq!(client.catalog(), &update.replacement);
    assert_eq!(begin(&mut client, 18).pairing_index, 2);
}

#[test]
fn failures_keep_committed_catalog_and_distinguish_storage_recovery() {
    for outcome in [CommitOutcome::Unchanged, CommitOutcome::Uncertain] {
        let mut client = client(Category::Pairing);
        let id = waiting(&mut client);
        ack(&mut client, 14);
        let expected = if outcome == CommitOutcome::Uncertain {
            CloseReason::StorageRecoveryRequired
        } else {
            CloseReason::StorageUnchanged
        };
        assert_eq!(
            client.handle(PairingEvent::CommitFailed(id, outcome), 15),
            vec![PairingEffect::Close(expected)]
        );
        assert_eq!(client.catalog(), &catalog());
        assert_eq!(
            client.recovery_required(),
            outcome == CommitOutcome::Uncertain
        );
    }
    for event in [
        PairingEvent::SendFailed(AttemptId {
            connection_generation: 4,
            handshake_generation: 9,
            pairing_index: 1,
        }),
        PairingEvent::Disconnected,
    ] {
        let mut client = client(Category::Pairing);
        waiting(&mut client);
        client.handle(event, 14);
        assert_eq!(client.phase(), PairingPhase::Closed);
        assert_eq!(client.catalog(), &catalog());
    }
}

#[test]
fn aborts_discard_late_traffic_but_concurrent_active_abort_closes() {
    for reason in [AbortReason::UserCancelled, AbortReason::ConcurrentAttempt] {
        let mut client = client(Category::Pairing);
        waiting(&mut client);
        let effects = client.handle(PairingEvent::RemoteAbort(reason), 14);
        if reason == AbortReason::ConcurrentAttempt {
            assert_eq!(
                effects,
                vec![PairingEffect::Close(CloseReason::ConcurrentAttempt)]
            );
        } else {
            assert!(effects.is_empty());
            assert!(
                client
                    .handle(PairingEvent::UnexpectedPairingMessage, 15)
                    .is_empty()
            );
        }
        assert_eq!(client.catalog(), &catalog());
    }
}

#[test]
fn all_secret_bearing_diagnostics_are_redacted() {
    let mut client = client(Category::Pairing);
    let id = begin(&mut client, 0);
    let candidate = Candidate::new([90; 32]);
    assert!(format!("{candidate:?}").contains("REDACTED"));
    let flight = flight(&mut client, id, 1);
    let effect = PairingEffect::SendFlight(id, flight);
    mark_emission(&mut client, id, 1);
    client.handle(PairingEvent::PairingFlightSent(id, 1), 2);
    let (_, update) = ack(&mut client, 3);
    for text in [
        format!("{client:?}"),
        format!("{effect:?}"),
        format!("{update:?}"),
        format!("{:?}", record(1, 1)),
        format!("{:?}", PairingEvent::CandidateReady(id, candidate)),
    ] {
        assert!(text.contains("REDACTED"));
        assert!(!text.contains("Wlpa"));
        assert!(!text.contains("90, 90"));
    }
}

#[test]
fn stale_completions_do_not_change_the_clock_or_close_a_later_attempt() {
    let mut client = client(Category::Pairing);
    let old = begin(&mut client, 10);
    let current = begin(&mut client, 20);
    assert!(
        client
            .handle(
                PairingEvent::CandidateReady(old, Candidate::new([91; 32])),
                0
            )
            .is_empty()
    );
    assert!(
        client
            .handle(PairingEvent::PairingFlightSent(old, 0), 0)
            .is_empty()
    );
    assert!(client.handle(PairingEvent::SendFailed(old), 0).is_empty());
    flight(&mut client, current, 21);
}

#[test]
fn protected_superseded_record_survives_even_when_its_current_replacement_is_evicted() {
    let old = record(1, 1);
    let mut current = record(1, 6);
    current.psk = [98; 32];
    let catalog = catalog()
        .plan_replace(current, &[old.psk_id()])
        .unwrap()
        .replacement;
    let catalog = catalog
        .plan_mark_used(old.psk_id(), 100)
        .unwrap()
        .replacement;
    let protected = [
        old.psk_id(),
        record(3, 3).psk_id(),
        record(4, 4).psk_id(),
        record(5, 5).psk_id(),
    ];
    let catalog = catalog
        .plan_replace(record(6, 101), &protected)
        .unwrap()
        .replacement;
    assert!(catalog.records().iter().any(|r| r.psk_id() == old.psk_id()));
    let restored = RecordCatalog::from_committed(
        catalog.capacity(),
        catalog.records().to_vec(),
        catalog.superseded_ids().to_vec(),
    )
    .unwrap();
    assert_eq!(restored, catalog);
    assert!(
        !restored
            .plan_release(&[])
            .unwrap()
            .replacement
            .records()
            .iter()
            .any(|r| r.psk_id() == old.psk_id())
    );
}

#[test]
fn malformed_activation_fields_close_silently() {
    for payload in [
        json!({"activities":["pairing"]}),
        json!({"activities":["pairing"],"pairing":{}}),
        json!({"activities":["pairing"],"pairing":{"method":7}}),
    ] {
        let mut client = client(Category::Pairing);
        assert_eq!(
            client.handle(PairingEvent::Activate(payload), 0),
            vec![PairingEffect::Close(CloseReason::Protocol)]
        );
    }
}

#[test]
fn durable_completion_after_duplicate_ack_close_still_publishes_the_committed_record() {
    let mut client = client(Category::Pairing);
    let id = waiting(&mut client);
    let (_, update) = ack(&mut client, 14);
    client.handle(PairingEvent::ServerFinalize(json!({})), 15);
    assert_eq!(client.phase(), PairingPhase::Closed);
    client.handle(PairingEvent::CommitSucceeded(id), 16);
    assert_eq!(client.catalog(), &update.replacement);
    assert_eq!(client.phase(), PairingPhase::Closed);
}

#[test]
fn cancel_while_the_flight_is_queued_discards_before_any_finalize_can_be_sent() {
    let mut client = client(Category::Pairing);
    let id = begin(&mut client, 0);
    flight(&mut client, id, 1);
    assert_eq!(
        client.handle(PairingEvent::Cancel, 2),
        vec![PairingEffect::Abort(id, AbortReason::UserCancelled)]
    );
    assert_eq!(client.phase(), PairingPhase::Discarding);
    assert!(
        client
            .handle(PairingEvent::PairingFlightSent(id, 3), 4)
            .is_empty()
    );
    assert_eq!(client.catalog(), &catalog());
}

#[test]
fn cancel_after_init_but_before_finalize_aborts_without_credential_delivery() {
    let mut client = client(Category::Pairing);
    let id = begin(&mut client, 0);
    flight(&mut client, id, 1);
    assert!(
        client
            .handle(PairingEvent::PairingInitSent(id, 2), 2)
            .is_empty()
    );
    assert_eq!(
        client.handle(PairingEvent::Cancel, 3),
        vec![PairingEffect::Abort(id, AbortReason::UserCancelled)]
    );
    assert!(
        client
            .handle(PairingEvent::PairingFinalizeStarted(id), 4)
            .is_empty()
    );
    assert_eq!(client.catalog(), &catalog());
}

#[test]
fn disconnect_and_send_failure_at_the_deadline_close_instead_of_sending_an_abort() {
    let id = AttemptId {
        connection_generation: 4,
        handshake_generation: 9,
        pairing_index: 1,
    };
    for (event, reason) in [
        (PairingEvent::Disconnected, CloseReason::Disconnected),
        (PairingEvent::SendFailed(id), CloseReason::SendFailed),
    ] {
        let mut client = client(Category::Pairing);
        waiting(&mut client);
        assert_eq!(
            client.handle(event, 120_012),
            vec![PairingEffect::Close(reason)]
        );
        assert_eq!(client.phase(), PairingPhase::Closed);
    }
}

#[test]
fn elapsed_deadlines_near_u64_max_do_not_overflow_and_generations_fence_old_work() {
    let mut client = client(Category::Pairing);
    let id = begin(&mut client, u64::MAX - 10);
    let wrong_generation = AttemptId {
        handshake_generation: id.handshake_generation - 1,
        ..id
    };
    assert!(
        client
            .handle(
                PairingEvent::CandidateReady(wrong_generation, Candidate::new([91; 32])),
                0
            )
            .is_empty()
    );
    flight(&mut client, id, u64::MAX - 9);
    mark_emission(&mut client, id, u64::MAX - 8);
    client.handle(
        PairingEvent::PairingFlightSent(id, u64::MAX - 8),
        u64::MAX - 7,
    );
    assert!(client.handle(PairingEvent::Tick, u64::MAX).is_empty());
    assert_eq!(client.phase(), PairingPhase::AwaitServerFinalize);
}
