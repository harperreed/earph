// ABOUTME: Exercises catalog validation against real and damaged specifications.
// ABOUTME: Prevents broken traceability from masquerading as release evidence.
use earph_harness::catalog::validate_catalog;
use serde_json::{Value, json};

fn catalog() -> Value {
    serde_json::from_slice(include_bytes!("../../../docs/requirements.json")).unwrap()
}
fn validate(value: &Value) -> Result<earph_harness::catalog::CatalogSummary, String> {
    validate_catalog(&serde_json::to_vec(value).unwrap())
}

#[test]
fn supplied_catalog_has_complete_traceability() {
    let summary = validate(&catalog()).unwrap();
    assert_eq!(
        (summary.requirements, summary.scenarios, summary.deferred),
        (69, 50, 10)
    );
}

#[test]
fn broken_links_cycles_ids_and_statuses_fail() {
    let changes = [
        ("/requirements/0/id", json!("ARCH-02")),
        ("/acceptance_scenarios/0/id", json!("AC-02")),
        ("/requirements/0/acceptance_ids", json!(["AC-99"])),
        (
            "/acceptance_scenarios/0/requirement_ids",
            json!(["MISSING"]),
        ),
        ("/requirements/0/depends_on", json!(["ARCH-01"])),
        ("/requirements/0/depends_on", json!(["ARCH-02"])),
        ("/requirements/0/depends_on", json!(["MISSING"])),
        ("/requirements/0/implementation_status", json!("wishful")),
        ("/requirements/0/verification_status", json!("wishful")),
        ("/acceptance_scenarios/0/status", json!("wishful")),
        ("/q25/option_text", json!("invented answer")),
        ("/requirements/0/decision_ids", json!(["D99"])),
    ];
    for (pointer, replacement) in changes {
        let mut input = catalog();
        *input.pointer_mut(pointer).unwrap() = replacement;
        assert!(validate(&input).is_err(), "accepted damaged {pointer}");
    }
}

#[test]
fn lost_decisions_and_asymmetric_links_fail() {
    let mut input = catalog();
    for requirement in input["requirements"].as_array_mut().unwrap() {
        requirement["decision_ids"]
            .as_array_mut()
            .unwrap()
            .retain(|id| id != "D24");
    }
    assert!(validate(&input).is_err());
    let mut input = catalog();
    input["acceptance_scenarios"][0]["requirement_ids"] = json!([]);
    assert!(validate(&input).is_err());
}

#[test]
fn malformed_and_oversized_catalogs_fail() {
    assert!(validate_catalog(b"{").is_err());
    assert!(validate_catalog(&vec![b' '; 2 * 1024 * 1024 + 1]).is_err());
}
