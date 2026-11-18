// ABOUTME: Tests output admission against actual safety policy values.
// ABOUTME: Covers lost arming, unknown hardware gain, caps, and fidelity conflicts.
use earph_core::{
    ErrorCode,
    ids::{DeviceId, ProfileId},
    safety::*,
};

fn context(device: &str, profile: &str) -> OutputContext {
    OutputContext {
        device_id: DeviceId::new(device).unwrap(),
        profile_id: ProfileId::new(profile).unwrap(),
    }
}

#[test]
fn fixed_output_requires_current_context_and_loses_arming_on_restore() {
    let mut policy = VolumePolicy::default();
    let selected = context("dac-a", "profile-a");
    let check = |policy: &VolumePolicy, ctx: &OutputContext| {
        policy.validate_unmute(
            ctx,
            VolumeMode::Fixed,
            SampleDomain::Pcm,
            FidelityPolicy::StrictPreservation,
            0.0,
            None,
        )
    };
    assert_eq!(
        check(&policy, &selected).unwrap_err().code,
        ErrorCode::SafetyPolicyBlocked
    );
    policy.arm_fixed(selected.clone());
    assert!(check(&policy, &selected).is_ok());
    for changed in [context("dac-b", "profile-a"), context("dac-a", "profile-b")] {
        assert!(check(&policy, &changed).is_err());
    }
    let restored: VolumePolicy =
        serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
    assert!(check(&restored, &selected).is_err());
    policy.disarm();
    assert!(check(&policy, &selected).is_err());
}

#[test]
fn software_caps_and_strict_preservation_are_independent_checks() {
    let policy = VolumePolicy::new(-20.0, None).unwrap();
    let selected = context("dac", "profile");
    let check = |gain, fidelity, domain| {
        policy.validate_unmute(
            &selected,
            VolumeMode::Software,
            domain,
            fidelity,
            gain,
            None,
        )
    };
    assert!(
        check(
            -40.0,
            FidelityPolicy::CompatibleConversion,
            SampleDomain::Pcm
        )
        .is_ok()
    );
    assert_eq!(
        check(
            -10.0,
            FidelityPolicy::CompatibleConversion,
            SampleDomain::Pcm
        )
        .unwrap_err()
        .code,
        ErrorCode::SafetyPolicyBlocked
    );
    assert_eq!(
        check(-40.0, FidelityPolicy::StrictPreservation, SampleDomain::Pcm)
            .unwrap_err()
            .code,
        ErrorCode::FidelityPolicyConflict
    );
    assert_eq!(
        check(
            -40.0,
            FidelityPolicy::CompatibleConversion,
            SampleDomain::Dsd
        )
        .unwrap_err()
        .code,
        ErrorCode::FidelityPolicyConflict
    );
    for gain in [f64::NAN, f64::INFINITY, -121.0, 1.0] {
        assert!(
            check(
                gain,
                FidelityPolicy::CompatibleConversion,
                SampleDomain::Pcm
            )
            .is_err()
        );
    }
}

#[test]
fn hardware_unmute_requires_known_readback_and_a_cap() {
    let selected = context("dac", "profile");
    let policy = VolumePolicy::new(0.0, Some(-20.0)).unwrap();
    let check = |value| {
        policy.validate_unmute(
            &selected,
            VolumeMode::Hardware,
            SampleDomain::Dsd,
            FidelityPolicy::StrictPreservation,
            0.0,
            value,
        )
    };
    assert!(check(Some(-40.0)).is_ok());
    for value in [None, Some(-10.0), Some(f64::NAN)] {
        assert_eq!(
            check(value).unwrap_err().code,
            ErrorCode::SafetyPolicyBlocked
        );
    }
    assert!(
        VolumePolicy::default()
            .validate_unmute(
                &selected,
                VolumeMode::Hardware,
                SampleDomain::Pcm,
                FidelityPolicy::StrictPreservation,
                0.0,
                Some(-40.0)
            )
            .is_err()
    );
}

#[test]
fn serialized_caps_validate_and_survive_without_profile_arming() {
    for doc in [
        r#"{"software_cap_db":1.0,"hardware_cap_db":null}"#,
        r#"{"software_cap_db":-121.0,"hardware_cap_db":null}"#,
        r#"{"software_cap_db":0.0,"hardware_cap_db":500.0}"#,
    ] {
        assert!(serde_json::from_str::<VolumePolicy>(doc).is_err());
    }
    let mut policy = VolumePolicy::new(-25.0, Some(-35.0)).unwrap();
    policy.arm_fixed(context("dac", "profile"));
    let restored: VolumePolicy =
        serde_json::from_slice(&serde_json::to_vec(&policy).unwrap()).unwrap();
    assert_eq!(restored.software_cap_db(), -25.0);
    assert_eq!(restored.hardware_cap_db(), Some(-35.0));
}

#[test]
fn fixed_arming_cannot_bypass_persistent_caps_or_invalid_readback() {
    let selected = context("dac", "profile");
    let mut capped = VolumePolicy::new(-20.0, None).unwrap();
    capped.arm_fixed(selected.clone());
    assert!(
        capped
            .validate_unmute(
                &selected,
                VolumeMode::Fixed,
                SampleDomain::Pcm,
                FidelityPolicy::StrictPreservation,
                0.0,
                None
            )
            .is_err()
    );
    let mut policy = VolumePolicy::default();
    policy.arm_fixed(selected.clone());
    assert!(
        policy
            .validate_unmute(
                &selected,
                VolumeMode::Fixed,
                SampleDomain::Pcm,
                FidelityPolicy::StrictPreservation,
                0.0,
                Some(f64::NAN)
            )
            .is_err()
    );
}
