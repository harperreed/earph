// ABOUTME: Exercises strict compiled-build records and explicit unavailable provenance through real codecs.
// ABOUTME: Rejects private free-form fields, incomplete nullable facts and substituted producer identities.
use earph_core::{ErrorCode, provenance::*};
use serde_json::{Value, json};

fn record() -> CompiledBuildRecord {
    CompiledBuildRecord {
        producer: BuildProducer::EnginePackage,
        input_set_version: 1,
        commit: BuildField::available("1".repeat(40).try_into().unwrap()),
        source_dirty: BuildField::available(false),
        dependency_lock_sha256: "2".repeat(64).try_into().unwrap(),
        source_inputs_sha256: "3".repeat(64).try_into().unwrap(),
        compiler: BuildField::available(RustCompiler {
            major: 1,
            minor: 98,
            patch: 1,
            channel: RustChannel::Stable,
            commit: BuildField::available("4".repeat(40).try_into().unwrap()),
        }),
        target: BuildField::available(BuildTarget::X86_64UnknownLinuxGnu),
        cargo_profile_class: BuildField::available(CargoProfileClass::Debug),
    }
}

#[test]
fn compiled_engine_record_round_trips_without_environment_or_private_strings() {
    let value = RunningBuildProvenance::available(record()).unwrap();
    value.validate_for_engine().unwrap();
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(bytes.len() <= MAX_BUILD_PROVENANCE_BYTES);
    assert_eq!(RunningBuildProvenance::decode(&bytes).unwrap(), value);
    assert!(value.retained_capacity_bytes() <= MAX_BUILD_PROVENANCE_BYTES);
}

#[test]
fn unavailable_build_facts_stay_null_with_closed_reasons() {
    let value = RunningBuildProvenance::unavailable(BuildUnavailableReason::NotProvided);
    let bytes = serde_json::to_vec(&value).unwrap();
    let document: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(document["record"], Value::Null);
    assert_eq!(document["unavailable_reason"], "not_provided");
    assert_eq!(RunningBuildProvenance::decode(&bytes).unwrap(), value);
    let mut partial = record();
    partial.commit = BuildField::unavailable(BuildFieldReason::NotProvided);
    partial.source_dirty = BuildField::unavailable(BuildFieldReason::NotProvided);
    partial.target = BuildField::unavailable(BuildFieldReason::UnsupportedValue);
    RunningBuildProvenance::available(partial)
        .unwrap()
        .validate_for_engine()
        .unwrap();
}

#[test]
fn strict_build_codec_rejects_missing_nulls_inconsistent_facts_and_private_text() {
    let original =
        serde_json::to_value(RunningBuildProvenance::available(record()).unwrap()).unwrap();
    for path in [
        "/unavailable_reason",
        "/record/commit/unavailable_reason",
        "/record/compiler/value/commit/unavailable_reason",
    ] {
        let mut invalid = original.clone();
        let (parent, field) = path.rsplit_once('/').unwrap();
        invalid
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            RunningBuildProvenance::decode(&serde_json::to_vec(&invalid).unwrap()).is_err(),
            "{path}"
        );
    }
    for (path, replacement) in [
        ("/record", Value::Null),
        ("/record/input_set_version", json!(2)),
        ("/record/commit/value", json!("A".repeat(40))),
        (
            "/record/source_dirty/unavailable_reason",
            json!("not_provided"),
        ),
        (
            "/record/target/value",
            json!("/private/company-target.json"),
        ),
        (
            "/record/compiler/value/channel",
            json!("private-company-compiler"),
        ),
        ("/record/compiler/value/major", json!(0)),
    ] {
        let mut invalid = original.clone();
        *invalid.pointer_mut(path).unwrap() = replacement;
        assert!(
            RunningBuildProvenance::decode(&serde_json::to_vec(&invalid).unwrap()).is_err(),
            "{path}"
        );
    }
    let mut extra = original;
    extra["private_checkout_path"] = json!("/private/checkout");
    assert!(RunningBuildProvenance::decode(&serde_json::to_vec(&extra).unwrap()).is_err());
}

#[test]
fn valid_harness_provenance_cannot_substitute_for_the_running_engine() {
    let mut harness = record();
    harness.producer = BuildProducer::Harness;
    let provenance = RunningBuildProvenance::available(harness).unwrap();
    let decoded =
        RunningBuildProvenance::decode(&serde_json::to_vec(&provenance).unwrap()).unwrap();
    assert!(decoded.validate_for_engine().is_err());
}

#[test]
fn build_codec_refuses_oversized_json_before_decoding() {
    let mut bytes =
        serde_json::to_vec(&RunningBuildProvenance::available(record()).unwrap()).unwrap();
    bytes.resize(MAX_BUILD_PROVENANCE_BYTES + 1, b' ');
    assert_eq!(
        RunningBuildProvenance::decode(&bytes).unwrap_err().code,
        ErrorCode::ResourceBudgetExceeded
    );
}
