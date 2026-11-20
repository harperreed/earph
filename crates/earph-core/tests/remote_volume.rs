// ABOUTME: Checks the complete shared renderer volume grid and its nearest-setting contract.
// ABOUTME: Keeps finite local millidecibels independent from quantized remote observations.
use earph_core::remote_volume::RemoteVolume;

#[test]
fn every_supported_remote_setting_round_trips_without_a_shadow_volume() {
    let silence = RemoteVolume::from_volume(0).unwrap();
    assert_eq!(silence.volume_db(), -32767);
    assert_eq!(silence.millidecibels(), None);
    let mut previous = -30721;
    for volume in 1..=30700 {
        let wire = RemoteVolume::from_volume(volume).unwrap();
        let db = wire.volume_db();
        assert!(db > previous && db - previous <= 2);
        assert_eq!(RemoteVolume::from_volume_db(db).unwrap(), wire);
        let local = wire.millidecibels().unwrap();
        assert!((f64::from(local) / 1000.0 - f64::from(db) / 256.0).abs() <= 0.0005 + 1e-12);
        assert_eq!(
            RemoteVolume::from_local(f64::from(local) / 1000.0, false).unwrap(),
            wire
        );
        previous = db;
    }
    assert_eq!(RemoteVolume::from_volume(1).unwrap().volume_db(), -30720);
    assert_eq!(RemoteVolume::from_volume(30700).unwrap().volume_db(), 0);
}

#[test]
fn omitted_db_codes_and_silence_transition_choose_the_quieter_nearest_setting() {
    for (volume, missing) in [
        (731, -29989),
        (2193, -28526),
        (3655, -27063),
        (5117, -25600),
        (6579, -24137),
        (8041, -22674),
        (9503, -21211),
        (10964, -19749),
        (12426, -18286),
        (13888, -16823),
        (15350, -15360),
        (16812, -13897),
        (18274, -12434),
        (19736, -10971),
        (21197, -9509),
        (22659, -8046),
        (24121, -6583),
        (25583, -5120),
        (27045, -3657),
        (28507, -2194),
        (29969, -731),
    ] {
        let selected = RemoteVolume::from_volume_db(missing).unwrap();
        assert_eq!(selected.volume(), volume);
        assert_eq!(selected.volume_db(), missing - 1);
    }
    assert_eq!(RemoteVolume::from_volume_db(-31744).unwrap().volume(), 0);
    assert_eq!(RemoteVolume::from_volume_db(-31743).unwrap().volume(), 1);
    assert_eq!(
        RemoteVolume::from_volume_db(-16).unwrap().millidecibels(),
        Some(-63)
    );
    assert_eq!(
        RemoteVolume::from_volume_db(-48).unwrap().millidecibels(),
        Some(-188)
    );
    for invalid in [-32768, 1, 32767] {
        assert!(RemoteVolume::from_volume_db(invalid).is_err());
    }
    assert!(RemoteVolume::from_volume(30701).is_err());
}

#[test]
fn every_local_millidecibel_stays_finite_and_has_a_bounded_remote_projection() {
    for millidecibels in -120000..=0 {
        let local = f64::from(millidecibels) / 1000.0;
        let observed = RemoteVolume::from_local(local, false).unwrap();
        assert_ne!(observed.volume(), 0);
        assert!((f64::from(observed.volume_db()) / 256.0 - local).abs() <= 1.0 / 256.0 + 1e-12);
        assert_eq!(RemoteVolume::from_local(local, true).unwrap().volume(), 0);
    }
    for invalid in [f64::NAN, f64::INFINITY, -120.001, 0.001] {
        assert!(RemoteVolume::from_local(invalid, false).is_err());
    }
}

#[test]
fn fractional_profile_caps_project_the_actual_finite_setting() {
    assert_eq!(
        RemoteVolume::from_local(-117.1445, false).unwrap().volume(),
        732
    );
    assert_eq!(
        RemoteVolume::from_local(-117.1446, false).unwrap().volume(),
        731
    );
    assert_eq!(
        RemoteVolume::from_local(-117.14453125, false)
            .unwrap()
            .volume(),
        731
    );
}
