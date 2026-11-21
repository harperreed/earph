// ABOUTME: Pins Sendspin filter arithmetic and bounded reducer failure contracts.
// ABOUTME: Uses real caller timestamps without native clocks, transports, or output.
use super::*;

fn binding() -> Binding {
    Binding::new([73; 32], 4, 8)
}
fn sync() -> TimeSync {
    TimeSync::new(
        binding(),
        TimePolicy::new(250_000, 9_000_000, 20_000_000).unwrap(),
    )
}
fn close(a: f64, b: f64) {
    assert!((a - b).abs() <= b.abs().max(1e-12) * 1e-11, "{a} != {b}");
}
fn sample(m: i64, e: i64, t: i64) -> Measurement {
    Measurement {
        offset: m,
        error: e,
        at: t,
    }
}
fn offer(s: &mut TimeSync, t: i64) -> ProbeToken {
    match s.poll(t).unwrap().action {
        Action::PrepareProbe(token) => token,
        other => panic!("{other:?}"),
    }
}
fn exchange(s: &mut TimeSync, t: i64, m: i64, e: i64) {
    let token = offer(s, t);
    s.probe_started(token, t).unwrap();
    s.reply(
        token,
        ServerTime {
            client_transmitted: t,
            server_received: t + m + e,
            server_transmitted: t + m + e + 100,
        },
        t + 2 * e + 100,
    )
    .unwrap();
}
fn burst(s: &mut TimeSync, start: i64, m: i64, e: i64) {
    for i in 0..8 {
        exchange(s, start + i * 10_000, m, e);
    }
}

#[test]
fn four_timestamp_anchors_keep_sign_units_and_truncation() {
    assert_eq!(
        Measurement::exchange(
            ServerTime {
                client_transmitted: 1_000_000,
                server_received: 1_011_100,
                server_transmitted: 1_011_300
            },
            1_002_400
        )
        .unwrap(),
        sample(10_000, 1100, 1_002_400)
    );
    assert_eq!(
        Measurement::exchange(
            ServerTime {
                client_transmitted: 10_000,
                server_received: 8500,
                server_transmitted: 8600
            },
            10_201
        )
        .unwrap(),
        sample(-1550, 50, 10_201)
    );
    assert_eq!(
        Measurement::exchange(
            ServerTime {
                client_transmitted: 10,
                server_received: -100,
                server_transmitted: -101
            },
            12
        ),
        Err(TimingError::ServerOrder)
    );
    assert_eq!(
        Measurement::exchange(
            ServerTime {
                client_transmitted: 10,
                server_received: 100,
                server_transmitted: 101
            },
            9
        ),
        Err(TimingError::LocalOrder)
    );
    assert_eq!(
        Measurement::exchange(
            ServerTime {
                client_transmitted: 0,
                server_received: i64::MAX,
                server_transmitted: i64::MAX
            },
            10
        ),
        Err(TimingError::NumericRange)
    );
}
#[test]
fn first_three_samples_match_pinned_scalar_steps() {
    let mut f = Filter::new();
    assert!(f.poo.is_infinite());
    f.update(sample(1000, 2, 1_000_000)).unwrap();
    assert_eq!(
        (f.count, f.offset, f.poo, f.drift, f.use_drift),
        (1, 1000., 1., 0., false)
    );
    f.update(sample(2000, 2, 2_000_000)).unwrap();
    assert_eq!((f.count, f.offset, f.use_drift), (2, 2000., false));
    close(f.drift, 0.001);
    close(f.pdd, 2e-12);
    f.update(sample(3000, 2, 3_000_000)).unwrap();
    assert_eq!((f.count, f.offset, f.use_drift), (3, 3000., true));
    close(f.poo, 0.75);
    close(f.pod, 5e-7);
    close(f.pdd, 1.0001e-12);
}
#[test]
fn filter_discard_guards_preserve_every_scalar() {
    let mut f = Filter::new();
    f.update(sample(100, 2, 100)).unwrap();
    for v in [
        sample(999, 0, 101),
        sample(999, -1, 101),
        sample(999, 2, 100),
        sample(999, 2, 99),
    ] {
        let old = format!("{f:?}");
        assert!(!f.update(v).unwrap());
        assert_eq!(format!("{f:?}"), old);
    }
}
#[test]
fn half_away_rounding_and_exact_envelope_are_checked() {
    for (value, want) in [
        (0.5, 1),
        (-0.5, -1),
        (1.5, 2),
        (-1.5, -2),
        (MAX_TIME_US as f64, MAX_TIME_US),
        (-(MAX_TIME_US as f64), -MAX_TIME_US),
    ] {
        assert_eq!(round_us(value), Ok(want));
    }
    for value in [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        (MAX_TIME_US + 1) as f64,
    ] {
        assert_eq!(round_us(value), Err(TimingError::NumericRange));
    }
}
#[test]
fn both_conversion_directions_include_drift_and_refuse_stale_state() {
    let mut s = sync();
    s.filter.update(sample(1000, 2, 1_000_000)).unwrap();
    s.filter.update(sample(2000, 2, 2_000_000)).unwrap();
    s.filter.update(sample(3000, 2, 3_000_000)).unwrap();
    assert_eq!(
        s.map_client_time(4_000_000, 3_000_000).unwrap().time_us,
        4_004_000
    );
    assert_eq!(
        s.map_server_time(4_004_000, 3_000_000).unwrap().time_us,
        4_000_000
    );
    s.filter.drift = -0.001;
    s.filter.offset = -3000.;
    assert_eq!(
        s.map_client_time(4_000_000, 3_000_000).unwrap().time_us,
        3_996_000
    );
    assert_eq!(
        s.map_server_time(3_996_000, 3_000_000).unwrap().time_us,
        4_000_000
    );
    assert_eq!(
        s.map_server_time(4_000_000, 23_000_001),
        Err(TimingError::Stale)
    );
}
#[test]
fn eight_emissions_select_one_positive_minimum_and_first_tie() {
    let mut s = sync();
    assert_eq!(s.snapshot(0).unwrap().estimate, None);
    for (i, (m, e)) in [
        (100, 8),
        (200, 0),
        (300, 4),
        (400, -1),
        (500, 2),
        (600, 2),
        (700, 10),
        (800, 3),
    ]
    .into_iter()
    .enumerate()
    {
        let t = 1_000_000 + i as i64 * 10_000;
        let token = offer(&mut s, t);
        s.probe_started(token, t).unwrap();
        // Negative error still has sane same-domain ordering: server processing exceeds local RTT.
        let (t2, t3, t4) = if e < 0 {
            (t + m - 1, t + m + 101, t + 100)
        } else {
            (t + m + e, t + m + e + 100, t + 2 * e + 100)
        };
        s.reply(
            token,
            ServerTime {
                client_transmitted: t,
                server_received: t2,
                server_transmitted: t3,
            },
            t4,
        )
        .unwrap();
        assert_eq!(
            s.snapshot(t4).unwrap().diagnostics.accepted_measurements,
            u64::from(i == 7)
        );
    }
    let snap = s.snapshot(1_080_000).unwrap();
    assert_eq!(snap.estimate.unwrap().offset_us, 500.);
    assert_eq!(snap.estimate.unwrap().covariance.offset_variance, 1.);
    assert_eq!(snap.diagnostics.emitted_requests, 8);
    assert_eq!(snap.diagnostics.completed_bursts, 1);
    assert_eq!(snap.mapping_revision, 1);
    assert_eq!(
        s.poll(1_080_000).unwrap().next_deadline_us,
        Some(10_000_000)
    );
}
#[test]
fn offer_expiry_cancellation_and_duplicate_t1_do_not_fabricate_emissions() {
    let mut s = sync();
    let token = offer(&mut s, 100);
    assert_eq!(offer(&mut s, 100), token);
    let replacement = offer(&mut s, 250_100);
    assert_ne!(token, replacement);
    assert_eq!(
        s.probe_started(token, 250_100),
        Err(TimingError::UnknownProbe)
    );
    s.cancel_probe(replacement, 250_100).unwrap();
    let token = offer(&mut s, 250_100);
    s.probe_started(token, 250_100).unwrap();
    s.reply(
        token,
        ServerTime {
            client_transmitted: 250_100,
            server_received: 250_100,
            server_transmitted: 250_100,
        },
        250_100,
    )
    .unwrap();
    let next = offer(&mut s, 250_100);
    assert_eq!(
        s.probe_started(next, 250_100),
        Err(TimingError::ClockNotAdvanced)
    );
    assert_eq!(s.snapshot(250_100).unwrap().diagnostics.emitted_requests, 1);
    s.probe_started(next, 250_101).unwrap();
    assert_eq!(s.snapshot(250_101).unwrap().diagnostics.emitted_requests, 2);
}
#[test]
fn exact_deadline_timeout_and_unmatched_replies_never_rebase_pending() {
    let mut s = sync();
    let token = offer(&mut s, 1_000_000);
    s.probe_started(token, 1_000_000).unwrap();
    let reply = ServerTime {
        client_transmitted: 1_000_000,
        server_received: 1_010_001,
        server_transmitted: 1_010_101,
    };
    let foreign = ProbeToken {
        binding: Binding::new([74; 32], 4, 8),
        ..token
    };
    assert_eq!(
        s.reply(foreign, reply, 1_100_000),
        Err(TimingError::BindingMismatch)
    );
    assert_eq!(s.poll(1_100_000).unwrap().next_deadline_us, Some(1_250_000));
    assert_eq!(
        s.reply(token, reply, 1_250_000),
        Err(TimingError::ReplyExpired)
    );
    assert_eq!(
        s.reply(token, reply, 1_100_000),
        Err(TimingError::UnknownProbe)
    );
    assert_eq!(s.snapshot(1_250_000).unwrap().estimate, None);
    assert_eq!(s.snapshot(1_250_000).unwrap().diagnostics.timeouts, 1);
}
#[test]
fn no_replies_and_all_invalid_bursts_do_not_initialize() {
    let mut s = sync();
    let mut t = 0;
    for _ in 0..8 {
        let token = offer(&mut s, t);
        s.probe_started(token, t).unwrap();
        t += 250_000;
        s.poll(t).unwrap();
    }
    let snap = s.snapshot(t).unwrap();
    assert_eq!(snap.estimate, None);
    assert_eq!(snap.diagnostics.timeouts, 8);
    assert_eq!(snap.diagnostics.completed_bursts, 1);
    burst(&mut s, 9_000_000, 100, 0);
    let snap = s.snapshot(9_080_000).unwrap();
    assert_eq!(snap.estimate, None);
    assert_eq!(snap.diagnostics.completed_bursts, 2);
}
#[test]
fn suppression_keeps_mapping_discards_candidate_and_records_cadence_miss() {
    let mut s = sync();
    burst(&mut s, 1_000_000, 100, 2);
    let before = s.snapshot(1_080_000).unwrap();
    exchange(&mut s, 10_000_000, 900, 2);
    let token = offer(&mut s, 10_010_000);
    s.probe_started(token, 10_010_000).unwrap();
    s.set_suppressed(true, 10_010_001).unwrap();
    assert_eq!(s.poll(20_000_000).unwrap().action, Action::NoAction);
    let during = s.snapshot(20_000_000).unwrap();
    assert_eq!(during.estimate, before.estimate);
    assert_eq!(during.diagnostics.incomplete_bursts, 1);
    assert_eq!(during.diagnostics.cadence_misses, 1);
    s.set_suppressed(false, 20_000_000).unwrap();
    let fresh = offer(&mut s, 20_000_000);
    assert_ne!(token, fresh);
    assert_eq!(
        s.reply(
            token,
            ServerTime {
                client_transmitted: 10_010_000,
                server_received: 10_010_000,
                server_transmitted: 10_010_000
            },
            10_010_100
        ),
        Err(TimingError::UnknownProbe)
    );
    assert_eq!(
        s.snapshot(20_000_000).unwrap().mapping_revision,
        before.mapping_revision
    );
}
#[test]
fn incomplete_burst_at_cadence_boundary_cannot_feed_its_candidate() {
    let mut s = sync();
    exchange(&mut s, 1_000_000, 100, 2);
    s.poll(11_000_000).unwrap();
    let snap = s.snapshot(11_000_000).unwrap();
    assert_eq!(snap.estimate, None);
    assert_eq!(snap.diagnostics.incomplete_bursts, 1);
    assert_eq!(snap.diagnostics.cadence_misses, 1);
}
#[test]
fn clock_regression_requires_new_binding_and_reconnect_has_fresh_filter() {
    let mut s = sync();
    burst(&mut s, 1_000_000, 100, 2);
    assert_eq!(s.poll(1), Err(TimingError::ClockRegression));
    assert_eq!(s.snapshot(1_080_000).unwrap().status, Status::Retired);
    let mut fresh = TimeSync::new(
        Binding::new([73; 32], 5, 9),
        TimePolicy::new(250_000, 9_000_000, 1000).unwrap(),
    );
    assert_eq!(fresh.snapshot(0).unwrap().status, Status::Uninitialized);
}
#[test]
fn policies_ranges_deadlines_and_counters_fail_closed() {
    for args in [
        (0, 9_000_000, 1),
        (250_000, 2_000_000, 1),
        (1, 10_000_001, 1),
        (1, 10_000_000, 0),
        (-1, 9, 1),
    ] {
        assert_eq!(
            TimePolicy::new(args.0, args.1, args.2),
            Err(TimingError::InvalidPolicy)
        );
    }
    let mut s = sync();
    assert_eq!(s.poll(-1), Err(TimingError::NumericRange));
    assert_eq!(s.poll(MAX_TIME_US), Err(TimingError::DeadlineOverflow));
    assert_eq!(s.snapshot(MAX_TIME_US).unwrap().status, Status::Retired);
    let mut s = sync();
    s.serial = u64::MAX;
    assert_eq!(s.poll(0), Err(TimingError::CounterOverflow));
    let mut s = sync();
    s.diagnostics.emitted_requests = u64::MAX;
    let token = offer(&mut s, 0);
    assert_eq!(s.probe_started(token, 0), Err(TimingError::CounterOverflow));
    let mut s = sync();
    s.revision = u64::MAX;
    for i in 0..7 {
        exchange(&mut s, 1_000_000 + i * 10_000, 100, 2);
    }
    let token = offer(&mut s, 1_070_000);
    s.probe_started(token, 1_070_000).unwrap();
    assert_eq!(
        s.reply(
            token,
            ServerTime {
                client_transmitted: 1_070_000,
                server_received: 1_070_102,
                server_transmitted: 1_070_202
            },
            1_070_104
        ),
        Err(TimingError::CounterOverflow)
    );
    assert_eq!(s.filter.count, 0);
}
#[test]
fn malformed_reply_does_not_change_snapshot_or_pending_deadline() {
    let mut s = sync();
    let token = offer(&mut s, 1_000_000);
    s.probe_started(token, 1_000_000).unwrap();
    let before = s.snapshot(1_000_000).unwrap();
    assert_eq!(
        s.reply(
            token,
            ServerTime {
                client_transmitted: 1_000_000,
                server_received: MAX_TIME_US + 1,
                server_transmitted: MAX_TIME_US + 1
            },
            1_000_100
        ),
        Err(TimingError::NumericRange)
    );
    assert_eq!(s.snapshot(1_000_000).unwrap(), before);
    assert_eq!(s.poll(1_000_000).unwrap().next_deadline_us, Some(1_250_000));
}
#[test]
fn unusable_numeric_update_retires_mapping_without_covariance_repair() {
    let mut s = sync();
    burst(&mut s, 1_000_000, 100, 2);
    s.filter.pdd = -1.;
    for i in 0..7 {
        exchange(&mut s, 10_000_000 + i * 10_000, 100, 2);
    }
    let token = offer(&mut s, 10_070_000);
    s.probe_started(token, 10_070_000).unwrap();
    assert_eq!(
        s.reply(
            token,
            ServerTime {
                client_transmitted: 10_070_000,
                server_received: 10_070_102,
                server_transmitted: 10_070_202
            },
            10_070_104
        ),
        Err(TimingError::NumericFault)
    );
    assert_eq!(s.snapshot(10_080_000).unwrap().status, Status::NumericFault);
    assert_eq!(
        s.map_server_time(12_000_000, 10_080_000),
        Err(TimingError::NumericFault)
    );
}

#[test]
fn adaptive_forgetting_waits_for_count_100_and_uses_strict_residual() {
    let base = Filter {
        offset: 0.,
        drift: 0.,
        poo: 1.,
        pod: 0.,
        pdd: 1e-12,
        last: 1_000_000,
        count: 99,
        use_drift: false,
    };
    let mut before = base;
    before.update(sample(7, 2, 2_000_000)).unwrap();
    assert_eq!(before.count, 100);
    close(before.poo, 2. / 3.);
    close(before.pod, 1e-6 / 3.);
    let mut equal = Filter { count: 100, ..base };
    equal.update(sample(6, 2, 2_000_000)).unwrap();
    assert_eq!(equal.count, 100);
    close(equal.poo, 2. / 3.);
    let mut after = Filter { count: 100, ..base };
    after.update(sample(7, 2, 2_000_000)).unwrap();
    assert_eq!(after.count, 100);
    close(after.poo, 8. / 9.);
    close(after.pod, 4e-6 / 9.);
    close(after.pdd, 4.0004e-12 - 16e-12 / 9.);
}
#[test]
fn drift_significance_is_strict_and_initial_two_branches_skip_it() {
    for (drift, want) in [(2., false), (2. + f64::EPSILON * 2., true)] {
        let mut f = Filter {
            offset: 0.,
            drift,
            poo: 2.,
            pod: -1.,
            pdd: 1.,
            last: 100,
            count: 2,
            use_drift: false,
        };
        f.update(sample(2, 2, 101)).unwrap();
        assert_eq!(f.use_drift, want);
    }
}
#[test]
fn covariance_and_effective_denominator_fault_instead_of_clamping() {
    for mut f in [
        Filter {
            poo: 1.,
            pod: 2.,
            pdd: 1.,
            count: 2,
            ..Filter::new()
        },
        Filter {
            poo: 1.,
            pod: 0.,
            pdd: 0.,
            drift: -2.,
            use_drift: true,
            count: 2,
            ..Filter::new()
        },
        Filter {
            poo: f64::MAX,
            pod: 0.,
            pdd: f64::MAX,
            count: 2,
            ..Filter::new()
        },
    ] {
        assert_eq!(f.update(sample(0, 2, 1)), Err(TimingError::NumericFault));
    }
}
#[test]
fn uncertain_pending_cancellation_and_retirement_clear_all_probe_work() {
    let mut s = sync();
    exchange(&mut s, 1_000_000, 100, 2);
    let token = offer(&mut s, 1_010_000);
    s.probe_started(token, 1_010_000).unwrap();
    s.cancel_probe(token, 1_010_001).unwrap();
    assert_eq!(
        s.snapshot(1_010_001).unwrap().diagnostics.incomplete_bursts,
        1
    );
    assert_eq!(s.snapshot(1_010_001).unwrap().estimate, None);
    s.retire();
    assert_eq!(s.poll(1_010_001).unwrap().action, Action::NoAction);
    assert_eq!(s.probe_started(token, 1_010_001), Err(TimingError::Retired));
    assert_eq!(
        s.map_server_time(1_010_000, 1_010_001),
        Err(TimingError::Retired)
    );
}
#[test]
fn mapping_results_are_checked_at_both_envelope_boundaries() {
    let mut s = sync();
    s.filter.update(sample(0, 2, 1)).unwrap();
    assert_eq!(
        s.map_server_time(MAX_TIME_US, 1).unwrap().time_us,
        MAX_TIME_US
    );
    assert_eq!(s.map_client_time(0, 1).unwrap().time_us, 0);
    assert_eq!(s.map_server_time(-1, 1), Err(TimingError::NumericRange));
    assert_eq!(
        s.map_client_time(MAX_TIME_US + 1, 1),
        Err(TimingError::NumericRange)
    );
    s.filter.offset = 1.;
    assert_eq!(
        s.map_client_time(MAX_TIME_US, 1),
        Err(TimingError::NumericRange)
    );
    let snapshot = s.snapshot(1).unwrap();
    let text = format!("{s:?} {snapshot:?}");
    assert!(!text.contains("[73"));
    assert!(!text.contains("server_public"));
}

#[test]
fn every_binding_field_and_exact_echo_fence_reply_authority() {
    let mut s = sync();
    let token = offer(&mut s, 1000);
    s.probe_started(token, 1000).unwrap();
    let reply = ServerTime {
        client_transmitted: 1000,
        server_received: 1102,
        server_transmitted: 1202,
    };
    for binding in [
        Binding::new([74; 32], 4, 8),
        Binding::new([73; 32], 5, 8),
        Binding::new([73; 32], 4, 9),
    ] {
        assert_eq!(
            s.reply(ProbeToken { binding, ..token }, reply, 1104),
            Err(TimingError::BindingMismatch)
        );
    }
    assert_eq!(
        s.reply(
            token,
            ServerTime {
                client_transmitted: 1001,
                ..reply
            },
            1104
        ),
        Err(TimingError::UnknownProbe)
    );
    assert_eq!(s.poll(1000).unwrap().next_deadline_us, Some(251000));
    s.reply(token, reply, 1104).unwrap();
    assert_eq!(s.reply(token, reply, 1104), Err(TimingError::UnknownProbe));
}
#[test]
fn exact_age_limit_is_usable_but_one_microsecond_later_is_stale() {
    let mut s = sync();
    burst(&mut s, 1_000_000, 100, 2);
    let last = s
        .snapshot(1_080_000)
        .unwrap()
        .estimate
        .unwrap()
        .last_update_us;
    assert_eq!(
        s.snapshot(last + 20_000_000).unwrap().status,
        Status::Mapped
    );
    assert_eq!(s.snapshot(last + 20_000_001).unwrap().status, Status::Stale);
}

#[test]
fn timeout_wins_before_payload_validation_and_cannot_resurrect() {
    let mut s = sync();
    let token = offer(&mut s, 1000);
    s.probe_started(token, 1000).unwrap();
    let malformed = ServerTime {
        client_transmitted: 1000,
        server_received: MAX_TIME_US + 1,
        server_transmitted: MAX_TIME_US + 1,
    };
    assert_eq!(
        s.reply(token, malformed, 251_000),
        Err(TimingError::ReplyExpired)
    );
    let valid = ServerTime {
        client_transmitted: 1000,
        server_received: 1102,
        server_transmitted: 1202,
    };
    assert_eq!(s.reply(token, valid, 1104), Err(TimingError::UnknownProbe));
    assert_eq!(s.snapshot(251_000).unwrap().diagnostics.timeouts, 1);
}

#[test]
fn matching_reply_clock_regression_retires_the_domain() {
    let mut s = sync();
    let token = offer(&mut s, 1000);
    s.probe_started(token, 1000).unwrap();
    let reply = ServerTime {
        client_transmitted: 1000,
        server_received: 1102,
        server_transmitted: 1202,
    };
    assert_eq!(
        s.reply(token, reply, 999),
        Err(TimingError::ClockRegression)
    );
    assert_eq!(s.snapshot(1000).unwrap().status, Status::Retired);
}

#[test]
fn repeated_old_t1_is_a_regression_after_the_caller_clock_has_advanced() {
    let mut s = sync();
    let token = offer(&mut s, 1000);
    s.probe_started(token, 1000).unwrap();
    s.reply(
        token,
        ServerTime {
            client_transmitted: 1000,
            server_received: 1000,
            server_transmitted: 1000,
        },
        1000,
    )
    .unwrap();
    let next = offer(&mut s, 1001);
    assert_eq!(
        s.probe_started(next, 1000),
        Err(TimingError::ClockRegression)
    );
}

#[test]
fn a_complete_burst_can_start_its_successor_at_exactly_ten_seconds() {
    let mut s = sync();
    burst(&mut s, 1_000_000, 100, 2);
    let token = offer(&mut s, 11_000_000);
    s.probe_started(token, 11_000_000).unwrap();
    assert_eq!(
        s.snapshot(11_000_000).unwrap().diagnostics.cadence_misses,
        0
    );
}
