// ABOUTME: Verifies portable profile intent without backend handles or authorization.
// ABOUTME: Exercises strict documents, retained storage, cap intersection, and deterministic selection.
use earph_core::{
    ErrorCode,
    ids::{DeviceId, ProfileId},
    profiles::{
        MAX_PROFILE_BYTES, ProfileAssociation, ProfileDeviceContext, ProfileDocument,
        ProfileResolution, ProfileSelectionIssue, ProfileSelectionReason, read_profile,
        resolve_profile,
    },
    safety::{FidelityPolicy, OutputContext, SampleDomain, VolumeMode, VolumePolicy},
};
use serde_json::{Value, json};

fn document() -> Value {
    json!({
        "schema_version":1,"id":"desk-headphones","revision":1,"name":"Desk headphones",
        "graph_sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        "source_policy":"local_file","fidelity":"compatible_conversion",
        "output":{"backend":"alsa","route":"direct_hardware","device_id":"desk-dac","manual_setup":"headphones"},
        "volume":{"mode":"software","software_cap_db":-20.0,"hardware_cap_db":null},
        "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},
        "corrections":[{"kind":"text_report","object_sha256":"abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"}]
    })
}
fn parse(value: &Value) -> Result<ProfileDocument, earph_core::EngineError> {
    read_profile(&serde_json::to_vec(value).unwrap())
}

#[test]
fn profile_roundtrip_preserves_listening_intent_without_output_authority() {
    let value = document();
    let profile = parse(&value).unwrap();
    assert_eq!(serde_json::to_value(&profile).unwrap(), value);
    assert_eq!(
        read_profile(&serde_json::to_vec(&profile).unwrap()).unwrap(),
        profile
    );
    assert_eq!(profile.id.as_str(), "desk-headphones");
    assert_eq!(profile.graph_sha256.as_str().len(), 64);
    for (field, injected) in [
        ("consented", json!(true)),
        (
            "armed_for",
            json!({"device_id":"desk-dac","profile_id":"desk-headphones"}),
        ),
        ("endpoint", json!("private-native-path")),
    ] {
        let mut invalid = value.clone();
        invalid["output"][field] = injected;
        assert_eq!(parse(&invalid).unwrap_err().code, ErrorCode::AssetInvalid);
    }
    let mut invalid = value;
    invalid["volume"]["hardware_readback_db"] = json!(-40);
    assert_eq!(parse(&invalid).unwrap_err().code, ErrorCode::AssetInvalid);
}

#[test]
fn profile_schema_and_bounds_reject_unsupported_or_unbounded_intent() {
    for (field, value, code) in [
        ("schema_version", json!(2), ErrorCode::CapabilityUnsupported),
        ("revision", json!(0), ErrorCode::AssetInvalid),
        ("name", json!(""), ErrorCode::AssetInvalid),
        ("name", json!("line\nbreak"), ErrorCode::AssetInvalid),
        (
            "name",
            json!("x".repeat(129)),
            ErrorCode::ResourceBudgetExceeded,
        ),
        (
            "graph_sha256",
            json!("A".repeat(64)),
            ErrorCode::AssetInvalid,
        ),
        ("source_policy", json!("virtual"), ErrorCode::AssetInvalid),
    ] {
        let mut invalid = document();
        invalid[field] = value;
        assert_eq!(parse(&invalid).unwrap_err().code, code, "{field}");
    }
    let mut maximum = document();
    maximum["name"] = json!("é".repeat(64));
    maximum["corrections"] = json!(
        (0..32)
            .map(|n| json!({
                "kind":"fir_manifest","object_sha256":format!("{n:064x}")
            }))
            .collect::<Vec<_>>()
    );
    assert!(parse(&maximum).is_ok());
    let extra = maximum["corrections"][0].clone();
    maximum["corrections"].as_array_mut().unwrap().push(extra);
    assert_eq!(
        parse(&maximum).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(
        read_profile(&vec![b' '; MAX_PROFILE_BYTES + 1])
            .unwrap_err()
            .code,
        ErrorCode::ResourceBudgetExceeded
    );

    let mut invalid = document();
    invalid["latency"]["max_resident_bytes"] = json!(0);
    assert_eq!(
        parse(&invalid).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    invalid = document();
    invalid["latency"]["max_preparation_bytes"] = json!(1024);
    assert_eq!(
        parse(&invalid).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
    invalid = document();
    invalid["volume"]["software_cap_db"] = json!(1);
    assert_eq!(parse(&invalid).unwrap_err().code, ErrorCode::AssetInvalid);
}

#[test]
fn caller_constructed_profiles_cannot_hide_excess_retained_capacity() {
    let mut profile = parse(&document()).unwrap();
    profile.name.reserve(MAX_PROFILE_BYTES * 2);
    assert_eq!(
        profile.validate().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );

    let mut profile = parse(&document()).unwrap();
    let mut id = String::with_capacity(MAX_PROFILE_BYTES * 2);
    id.push_str("desk-headphones");
    profile.id = ProfileId::new(id).unwrap();
    assert_eq!(
        profile.validate().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );

    let mut profile = parse(&document()).unwrap();
    let mut id = String::with_capacity(MAX_PROFILE_BYTES * 2);
    id.push_str("desk-dac");
    profile.output.device_id = DeviceId::new(id).unwrap();
    assert_eq!(
        profile.validate().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );

    let mut profile = parse(&document()).unwrap();
    profile.corrections.reserve(MAX_PROFILE_BYTES);
    assert_eq!(
        profile.validate().unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
}

#[test]
fn imported_caps_intersect_local_caps_and_never_copy_fixed_arming() {
    let profile = parse(&document()).unwrap();
    let context = OutputContext {
        device_id: profile.output.device_id.clone(),
        profile_id: profile.id.clone(),
    };
    let mut local = VolumePolicy::new(-30.0, Some(-24.0)).unwrap();
    local.arm_fixed(context.clone());
    let effective = profile.volume.effective_policy(&local).unwrap();
    assert_eq!(effective.software_cap_db(), -30.0);
    assert_eq!(effective.hardware_cap_db(), Some(-24.0));
    assert_eq!(local.software_cap_db(), -30.0);

    for (local_cap, requested_cap, expected) in [(-24.0, -10.0, -24.0), (-10.0, -24.0, -24.0)] {
        let mut requested = profile.volume.clone();
        requested.hardware_cap_db = Some(requested_cap);
        let local = VolumePolicy::new(0.0, Some(local_cap)).unwrap();
        let effective = requested.effective_policy(&local).unwrap();
        assert_eq!(effective.software_cap_db(), -20.0);
        assert_eq!(effective.hardware_cap_db(), Some(expected));
    }

    let mut fixed = profile.volume;
    fixed.mode = VolumeMode::Fixed;
    fixed.software_cap_db = 0.0;
    fixed.hardware_cap_db = Some(-10.0);
    let mut armed = VolumePolicy::new(0.0, None).unwrap();
    armed.arm_fixed(context.clone());
    assert!(
        armed
            .validate_unmute(
                &context,
                VolumeMode::Fixed,
                SampleDomain::Pcm,
                FidelityPolicy::StrictPreservation,
                0.0,
                None
            )
            .is_ok()
    );
    let effective = fixed.effective_policy(&armed).unwrap();
    assert_eq!(effective.hardware_cap_db(), Some(-10.0));
    assert_eq!(
        effective
            .validate_unmute(
                &context,
                VolumeMode::Fixed,
                SampleDomain::Pcm,
                FidelityPolicy::StrictPreservation,
                0.0,
                Some(-20.0)
            )
            .unwrap_err()
            .code,
        ErrorCode::SafetyPolicyBlocked
    );
}

fn setups() -> Vec<ProfileDocument> {
    let headphones = parse(&document()).unwrap();
    let mut speakers = document();
    speakers["id"] = json!("desk-speakers");
    speakers["revision"] = json!(3);
    speakers["name"] = json!("Desk speakers");
    speakers["output"]["manual_setup"] = json!("speakers");
    vec![headphones, parse(&speakers).unwrap()]
}
fn association(profile: &ProfileDocument) -> ProfileAssociation {
    ProfileAssociation {
        backend: profile.output.backend,
        route: profile.output.route,
        device_identity_sha256: profile.graph_sha256,
        profile_id: profile.id.clone(),
    }
}
fn context(profile: &ProfileDocument) -> ProfileDeviceContext {
    ProfileDeviceContext {
        backend: profile.output.backend,
        route: profile.output.route,
        device_id: profile.output.device_id.clone(),
        verified_identity_sha256: Some(profile.graph_sha256),
    }
}

#[test]
fn manual_setup_selection_wins_without_inferring_a_passive_device() {
    let profiles = setups();
    let device = context(&profiles[0]);
    let selected = resolve_profile(
        &profiles,
        &[association(&profiles[1])],
        Some(&profiles[0].id),
        Some(&device),
    )
    .unwrap();
    assert!(
        matches!(selected, ProfileResolution::Selected { profile, reason: ProfileSelectionReason::Manual }
        if profile.id == profiles[0].id && profile.revision == 1)
    );
    assert!(
        matches!(resolve_profile(&profiles, &[], Some(&profiles[1].id), None).unwrap(),
        ProfileResolution::Selected { profile, reason: ProfileSelectionReason::Manual } if profile.revision == 3)
    );

    let mut different = device;
    different.device_id = DeviceId::new("another-dac").unwrap();
    assert_eq!(
        resolve_profile(&profiles, &[], Some(&profiles[0].id), Some(&different))
            .unwrap_err()
            .code,
        ErrorCode::DeviceUnavailable
    );
}

#[test]
fn exact_device_resolution_requires_verified_identity_and_reports_ambiguity() {
    let mut profiles = setups();
    let mut device = context(&profiles[0]);
    let bindings = vec![association(&profiles[1]), association(&profiles[0])];
    let selected = resolve_profile(&profiles, &bindings[..1], None, Some(&device)).unwrap();
    assert!(
        matches!(selected, ProfileResolution::Selected { profile, reason: ProfileSelectionReason::ExactDevice }
        if profile.id == profiles[1].id && profile.revision == 3)
    );
    let expected = resolve_profile(&profiles, &bindings, None, Some(&device)).unwrap();
    let ProfileResolution::SelectionRequired {
        reason: ProfileSelectionIssue::Ambiguous,
        candidates,
    } = &expected
    else {
        panic!("two setups on one DAC require selection: {expected:?}");
    };
    assert_eq!(
        candidates.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ["desk-headphones", "desk-speakers"]
    );
    profiles.reverse();
    let mut reversed = bindings.clone();
    reversed.reverse();
    assert_eq!(
        resolve_profile(&profiles, &reversed, None, Some(&device)).unwrap(),
        expected
    );

    device.verified_identity_sha256 = None;
    assert!(
        matches!(resolve_profile(&profiles, &bindings, None, Some(&device)).unwrap(),
        ProfileResolution::SelectionRequired { reason: ProfileSelectionIssue::NoVerifiedMatch, candidates } if candidates.is_empty())
    );
    device.verified_identity_sha256 = Some("f".repeat(64).try_into().unwrap());
    assert!(matches!(
        resolve_profile(&profiles, &bindings, None, Some(&device)).unwrap(),
        ProfileResolution::SelectionRequired {
            reason: ProfileSelectionIssue::NoVerifiedMatch,
            ..
        }
    ));
}

#[test]
fn resolution_rejects_inconsistent_catalogs_and_limits_before_selection() {
    let profiles = setups();
    let device = context(&profiles[0]);
    let mut binding = association(&profiles[0]);
    binding.profile_id = ProfileId::new("missing-profile").unwrap();
    assert_eq!(
        resolve_profile(&profiles, &[binding], None, Some(&device))
            .unwrap_err()
            .code,
        ErrorCode::AssetInvalid
    );
    let duplicate = vec![profiles[0].clone(), profiles[0].clone()];
    assert_eq!(
        resolve_profile(&duplicate, &[], None, Some(&device))
            .unwrap_err()
            .code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(
        resolve_profile(&vec![profiles[0].clone(); 33], &[], None, Some(&device))
            .unwrap_err()
            .code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(
        resolve_profile(
            &profiles,
            &vec![association(&profiles[0]); 65],
            None,
            Some(&device)
        )
        .unwrap_err()
        .code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut binding = association(&profiles[0]);
    let mut id = String::with_capacity(MAX_PROFILE_BYTES * 2);
    id.push_str(profiles[0].id.as_str());
    binding.profile_id = ProfileId::new(id).unwrap();
    assert_eq!(
        resolve_profile(&profiles, &[binding], None, Some(&device))
            .unwrap_err()
            .code,
        ErrorCode::ResourceBudgetExceeded
    );
}
