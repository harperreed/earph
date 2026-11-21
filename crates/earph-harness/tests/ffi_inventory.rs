// ABOUTME: Verifies that the real dependency producer includes the binding generator feature closure.
// ABOUTME: Keeps UniFFI's selected status and reviewed license provenance tied to resolved packages.
use earph_harness::inventory;

#[test]
fn binding_generator_and_runtime_share_the_existing_reviewed_inventory() {
    let (record, notices) = inventory::snapshot().expect("resolve the actual binding inventory");
    let packages = record["packages"].as_array().unwrap();
    let runtime = packages.iter().find(|p| p["name"] == "uniffi").unwrap();
    assert_eq!(runtime["version"], "0.32.2");
    assert!(
        runtime["features"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f == "cli")
    );
    for name in ["uniffi", "uniffi_bindgen", "uniffi_core", "uniffi_macros"] {
        let package = packages.iter().find(|p| p["name"] == name).unwrap();
        assert_eq!(package["license_expression"], "MPL-2.0");
        assert_eq!(
            package["license_provenance"]["revision"],
            "7bc621ee9d0d15371d792612707ba5ed4111f950"
        );
        assert!(!package["license_files"].as_array().unwrap().is_empty());
    }
    let selected = record["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "uniffi")
        .unwrap();
    assert_eq!(selected["status"], "selected");
    assert_eq!(selected["qualification"], "partial");
    assert!(notices.contains("Mozilla Public License Version 2.0"));
    assert!(notices.contains("AUTHORS-MIT:"));
}
