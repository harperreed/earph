// ABOUTME: Checks the dependency inventory against the actual Cargo graph.
// ABOUTME: Detects unrecorded feature and license changes before qualification.
use earph_harness::inventory;

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
