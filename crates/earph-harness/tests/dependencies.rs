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
        if package["name"].as_str().unwrap().starts_with("objc2-core-") {
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
