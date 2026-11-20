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
fn exact_zero_cannot_authorize_dsd_fixed_hardware_or_strict_attenuation() {
    let policy = VolumePolicy::default();
    for mode in [
        VolumeMode::Software,
        VolumeMode::Hardware,
        VolumeMode::Fixed,
    ] {
        for domain in [SampleDomain::Pcm, SampleDomain::Dsd] {
            for fidelity in [
                FidelityPolicy::StrictPreservation,
                FidelityPolicy::CompatibleConversion,
                FidelityPolicy::ExpertOverride,
            ] {
                assert_eq!(
                    policy
                        .validate_software_zero(mode, domain, fidelity)
                        .is_ok(),
                    mode == VolumeMode::Software
                        && domain == SampleDomain::Pcm
                        && fidelity != FidelityPolicy::StrictPreservation
                );
            }
        }
    }
    policy
        .validate_unmute(
            &context("dac", "profile"),
            VolumeMode::Software,
            SampleDomain::Pcm,
            FidelityPolicy::StrictPreservation,
            0.0,
            None,
        )
        .unwrap();
}

#[test]
fn prospective_fixed_policy_never_acquires_current_or_target_arming() {
    let selected = context("dac", "current");
    let target = context("dac", "target");
    let mut current = VolumePolicy::default();
    let prospective = VolumePolicy::default();
    assert_eq!(
        current
            .validate_profile_preparation(
                &selected,
                VolumeMode::Fixed,
                FidelityPolicy::CompatibleConversion,
                0.0,
                &prospective,
                0.0
            )
            .unwrap_err()
            .code,
        ErrorCode::SafetyPolicyBlocked
    );
    current.arm_fixed(selected.clone());
    let before = current.clone();
    current
        .validate_profile_preparation(
            &selected,
            VolumeMode::Fixed,
            FidelityPolicy::CompatibleConversion,
            0.0,
            &prospective,
            0.0,
        )
        .unwrap();
    assert_eq!(current, before);
    for ctx in [&selected, &target] {
        assert_eq!(
            prospective
                .validate_unmute(
                    ctx,
                    VolumeMode::Fixed,
                    SampleDomain::Pcm,
                    FidelityPolicy::CompatibleConversion,
                    0.0,
                    None
                )
                .unwrap_err()
                .code,
            ErrorCode::SafetyPolicyBlocked
        );
    }
    for (gain, policy) in [
        (0.0, VolumePolicy::new(-20.0, None).unwrap()),
        (0.0, VolumePolicy::new(0.0, Some(-30.0)).unwrap()),
        (-40.0, VolumePolicy::default()),
        (f64::NAN, VolumePolicy::default()),
    ] {
        assert!(
            current
                .validate_profile_preparation(
                    &selected,
                    VolumeMode::Fixed,
                    FidelityPolicy::CompatibleConversion,
                    0.0,
                    &policy,
                    gain
                )
                .is_err()
        );
    }
}
#[test]
fn ordinary_unmute_preserves_constraint_error_priority_before_fixed_arming() {
    for (mode, domain, gain, software_cap, hardware_cap, readback, expected) in [
        (
            VolumeMode::Fixed,
            SampleDomain::Pcm,
            -40.0,
            0.0,
            None,
            None,
            ErrorCode::FidelityPolicyConflict,
        ),
        (
            VolumeMode::Fixed,
            SampleDomain::Pcm,
            -40.0,
            -60.0,
            None,
            None,
            ErrorCode::SafetyPolicyBlocked,
        ),
        (
            VolumeMode::Fixed,
            SampleDomain::Pcm,
            0.0,
            0.0,
            None,
            Some(f64::NAN),
            ErrorCode::SafetyPolicyBlocked,
        ),
        (
            VolumeMode::Software,
            SampleDomain::Dsd,
            -40.0,
            0.0,
            None,
            None,
            ErrorCode::FidelityPolicyConflict,
        ),
        (
            VolumeMode::Hardware,
            SampleDomain::Pcm,
            -40.0,
            0.0,
            Some(-10.0),
            None,
            ErrorCode::SafetyPolicyBlocked,
        ),
        (
            VolumeMode::Hardware,
            SampleDomain::Pcm,
            -40.0,
            0.0,
            Some(-10.0),
            Some(-20.0),
            ErrorCode::FidelityPolicyConflict,
        ),
    ] {
        let policy = VolumePolicy::new(software_cap, hardware_cap).unwrap();
        assert_eq!(
            policy
                .validate_unmute(
                    &context("dac", "profile"),
                    mode,
                    domain,
                    FidelityPolicy::CompatibleConversion,
                    gain,
                    readback
                )
                .unwrap_err()
                .code,
            expected
        );
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
