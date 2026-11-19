// ABOUTME: Checks the dependency inventory against the actual Cargo graph.
// ABOUTME: Detects unrecorded feature and license changes before qualification.
use earph_harness::inventory;

#[test]
fn selected_decoder_keeps_extended_format_qualification_partial() {
    let (snapshot, _) = inventory::snapshot().unwrap();
    let candidate = snapshot["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "symphonia")
        .unwrap();
    assert_eq!(candidate["status"], "selected");
    assert_eq!(candidate["revision"], "symphonia 0.6.1");
    assert_eq!(candidate["qualification"], "partial");
}

#[test]
fn changed_dependency_features_or_license_hashes_are_rejected() {
    let (snapshot, _) = inventory::snapshot().unwrap();
    inventory::validate(&snapshot).unwrap();
    let mut changed = snapshot.clone();
    changed["packages"][0]["features"] = serde_json::json!(["invented"]);
    assert!(inventory::validate(&changed).is_err());
    let mut changed = snapshot;
    changed["packages"][0]["license_files"][0]["sha256"] = "changed".into();
    assert!(inventory::validate(&changed).is_err());
}

#[test]
fn selected_alsa_adapter_keeps_platform_qualification_partial() {
    let (snapshot, _) = inventory::snapshot().unwrap();
    let candidate = snapshot["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|candidate| candidate["name"] == "ALSA")
        .unwrap();
    assert_eq!(candidate["status"], "selected");
    assert_eq!(candidate["revision"], "alsa-sys 0.6.1");
    assert_eq!(candidate["qualification"], "partial");
}

#[test]
fn core_audio_licenses_are_bound_to_the_published_source_revision() {
    let (snapshot, _) = inventory::snapshot().unwrap();
    let mut count = 0;
    for package in snapshot["packages"].as_array().unwrap() {
        if matches!(
            package["name"].as_str(),
            Some("objc2-core-audio" | "objc2-core-audio-types" | "objc2-core-foundation")
        ) {
            count += 1;
            assert_eq!(package["version"], "0.3.2");
            assert_eq!(
                package["license_provenance"]["revision"],
                "7b1abfd750a2cacaea71d6a56ecfb83cb7de560b"
            );
            assert_eq!(
                package["license_provenance"]["distribution_status"],
                "requires_apple_sdk_distribution_review"
            );
            assert!(!package["license_files"].as_array().unwrap().is_empty());
        }
    }
    assert_eq!(count, 3);
}

#[test]
fn native_gui_stays_partially_qualified_with_pinned_reviewed_notices() {
    let (snapshot, notices) = inventory::snapshot().unwrap();
    let candidate = snapshot["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "Linux GUI")
        .unwrap();
    assert_eq!(candidate["status"], "selected");
    assert_eq!(candidate["qualification"], "partial");
    assert_eq!(
        candidate["revision"],
        "eframe 0.37.0; attach-only native GUI"
    );
    let packages = snapshot["packages"].as_array().unwrap();
    let find = |name: &str, version: &str| {
        packages
            .iter()
            .find(|p| p["name"] == name && p["version"] == version)
            .unwrap()
    };
    for (name, version) in [("windows-link", "0.2.1"), ("windows-sys", "0.61.2")] {
        let p = find(name, version);
        assert_eq!(p["license_provenance"]["version"], version);
        assert!(p["license_provenance"]["license_sha256"].as_str().is_some());
    }
    let lowercase = find("windows", "0.62.2");
    assert!(
        lowercase["license_files"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["name"] == "license-mit")
    );
    let graphics = find("objc2-core-graphics", "0.3.2");
    assert_eq!(
        graphics["license_provenance"]["distribution_status"],
        "requires_apple_sdk_distribution_review"
    );
    let fonts = find("epaint_default_fonts", "0.37.0");
    assert_eq!(
        fonts["license_provenance"]["revision"],
        "7c0b40402271e8d40cda69f87cf3c2617c2e4382"
    );
    assert_eq!(
        fonts["license_provenance"]["license_source_revision"],
        "841a328ba2dd8f6996beb7189ebb7fc00e668a2a"
    );
    assert_eq!(
        fonts["license_provenance"]["font_assets"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    let dispatch = find("dispatch", "0.2.0");
    assert_eq!(
        dispatch["license_provenance"]["distribution_status"],
        "requires_original_license_notice_review"
    );
    assert!(
        notices.contains("The original upstream copyright and permission notice was not supplied.")
    );
    assert!(notices.contains("BITSTREAM VERA LICENSE"));
    assert!(notices.contains("UBUNTU FONT LICENCE Version 1.0"));
}
