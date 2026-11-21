// ABOUTME: Verifies explicit typed player profiles preserve core-only transport admission.
// ABOUTME: Uses real transport requests without selecting credentials or native output.
use earph_core::ids::OwnerId;
use earph_network::sendspin::*;
use earph_sendspin::{pairing::*, player::*, session::*};
use serde_json::json;
use std::{
    net::IpAddr,
    time::{Duration, Instant},
};
struct Services;
impl SendspinServices for Services {
    fn admission(&mut self, _: AdmissionProposal) -> Operation<Result<bool, DriverError>> {
        Box::pin(async { Ok(true) })
    }
    fn candidate(&mut self, _: AttemptId) -> Operation<Result<Candidate, DriverError>> {
        panic!("no connection was admitted")
    }
    fn commit(&mut self, _: AttemptId, _: RecordUpdate) -> Operation<Result<(), CommitOutcome>> {
        panic!("no connection was admitted")
    }
}
fn config() -> SessionConfig {
    SessionConfig {
        private: [5; 32],
        pairing_psk: [7; 32],
        connection_generation: 1,
        hello: json!({"name":"fixture","supported_roles":[]}),
        catalog: RecordCatalog::new(5).unwrap(),
        protected: vec![],
        limits: roleless_session_limits(),
    }
}
fn permit() -> SendspinConnectPermit {
    SendspinConnectPermit::new(
        OwnerId::new("player-fixture").unwrap(),
        1,
        1,
        Instant::now() + Duration::from_secs(1),
        "ws://fixture.invalid:19991/sendspin",
        vec!["192.168.1.1".parse::<IpAddr>().unwrap()],
    )
    .unwrap()
}
fn clock() -> ClockConfiguration {
    ClockConfiguration {
        domain: MonotonicDomain::new(1).unwrap(),
        policy: earph_sendspin::timing::TimePolicy::new(1000, 100000, 200000).unwrap(),
    }
}
#[test]
fn player_offer_requires_explicit_profile_and_native_policy() {
    let mut worker = SendspinWorker::default();
    let p = permit();
    let mut forged = config();
    forged.hello["supported_roles"] = json!(["player@v1"]);
    forged.hello["player@v1_support"] = PlayerSupport::new(1024).unwrap().to_value();
    assert_eq!(
        worker
            .start(
                p.clone(),
                &p,
                forged,
                clock(),
                Box::new(Services),
                DriverLimits::default()
            )
            .err(),
        Some(DriverError::Admission)
    );
    assert_eq!(
        worker
            .start_player(
                p.clone(),
                &p,
                config(),
                clock(),
                Box::new(Services),
                DriverLimits::default(),
                PlayerSupport::new(1024).unwrap()
            )
            .err(),
        Some(DriverError::Admission)
    );
}
#[test]
fn player_support_cannot_exceed_retained_event_capacity() {
    let mut worker = SendspinWorker::default();
    let p = permit();
    assert_eq!(
        worker
            .start_player(
                p.clone(),
                &p,
                config(),
                clock(),
                Box::new(Services),
                DriverLimits {
                    event_bytes: 100,
                    ..DriverLimits::default()
                },
                PlayerSupport::new(101).unwrap()
            )
            .err(),
        Some(DriverError::Budget)
    );
}
#[test]
fn player_metadata_fits_the_existing_total_payload_ceiling() {
    let defaults = DriverLimits::default();
    let mut low = 1;
    let mut high = defaults.total_payload_bytes;
    while low < high {
        let middle = low + (high - low) / 2;
        if (DriverLimits {
            total_payload_bytes: middle,
            ..defaults
        })
        .validate(roleless_session_limits())
        .is_ok()
        {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let limits = DriverLimits {
        total_payload_bytes: low,
        ..defaults
    };
    assert!(limits.validate(roleless_session_limits()).is_ok());
    let p = permit();
    assert_eq!(
        SendspinWorker::default()
            .start_player(
                p.clone(),
                &p,
                config(),
                clock(),
                Box::new(Services),
                limits,
                PlayerSupport::new(1024).unwrap()
            )
            .err(),
        Some(DriverError::Budget)
    );
}
