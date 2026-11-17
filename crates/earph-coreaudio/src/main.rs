// ABOUTME: Runs read-only Core Audio inspection and silent registration checks.
// ABOUTME: Reports precision conflicts without changing hardware settings or starting playback.
#![forbid(unsafe_code)]
use earph_core::ErrorCode;
use earph_coreaudio::CoreAudioError;
use serde_json::{Value, json};
use std::process::ExitCode;

#[cfg(target_os = "macos")]
fn envelope(command: &str) -> Value {
    json!({"schema_version":1,"status":"pass","command":command,
        "build":earph_harness::build_provenance(),
        "observed_at_unix_ns":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok().map(|time|time.as_nanos().to_string()),
        "environment":{"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,
            "os_version":std::process::Command::new("sw_vers").arg("-productVersion").output().ok()
                .filter(|output|output.status.success()).map(|output|String::from_utf8_lossy(&output.stdout).trim().to_owned())},
        "signals_submitted":0,"physical_playback":"not_run","requested_device_changes":false})
}
#[cfg(target_os = "macos")]
fn native(args: &[&str]) -> Result<(Value, bool), CoreAudioError> {
    use earph_coreaudio::{PrecisionRequest, SilentRegistration, assess, devices, inspect};
    use std::path::Path;
    let mut result = envelope(args[0]);
    match args {
        ["devices"] => {
            let inventory = devices()?;
            let complete = inventory.failures.is_empty();
            result["inventory"] =
                serde_json::to_value(inventory).expect("serializable native inventory");
            result["evidence_level"] = "observed_current_settings".into();
            if !complete {
                result["status"] = "partial".into();
            }
            Ok((result, complete))
        }
        ["inspect", "--device-uid", uid] => {
            result["device"] =
                serde_json::to_value(inspect(uid)?).expect("serializable native observation");
            result["evidence_level"] = "observed_current_settings".into();
            Ok((result, true))
        }
        ["precision-check", "--request", path] => {
            let bytes = earph_harness::read_bounded(Path::new(path), 64 * 1024).map_err(|_| {
                CoreAudioError::new(
                    ErrorCode::AssetInvalid,
                    "cannot read a bounded precision request",
                )
            })?;
            let request: PrecisionRequest = serde_json::from_slice(&bytes).map_err(|_| {
                CoreAudioError::new(
                    ErrorCode::AssetInvalid,
                    "invalid precision request JSON or required fields",
                )
            })?;
            request.validate()?;
            let assessment = assess(&request, &inspect(&request.device_uid)?)?;
            let matches = assessment.matches_current_format;
            result["assessment"] =
                serde_json::to_value(assessment).expect("serializable format assessment");
            if !matches {
                result["status"] = "conflict".into();
                result["error"] = serde_json::to_value(CoreAudioError::new(ErrorCode::FormatUnsupported,
                    "current device settings conflict with exact integer PCM; no settings were changed")).expect("serializable error");
            }
            Ok((result, matches))
        }
        ["registration-check", "--device-uid", uid] => {
            let before = inspect(uid)?;
            let mut registration = SilentRegistration::new(uid)?;
            let report = registration.report();
            registration.close()?;
            let callbacks = registration.callbacks_observed();
            let after = inspect(uid)?;
            let unchanged = serde_json::to_value(&before).expect("serializable device")
                == serde_json::to_value(&after).expect("serializable device");
            result["registration"] =
                serde_json::to_value(report).expect("serializable registration");
            result["callbacks_observed"] = callbacks.into();
            result["closed"] = true.into();
            result["settings_unchanged"] = unchanged.into();
            if callbacks != 0 || !unchanged {
                result["status"] = "fail".into();
                result["error"] = serde_json::to_value(CoreAudioError::new(ErrorCode::OutputFault,
                    "unexpected callback or changed settings during silent verification; inspect the device again")).expect("serializable error");
            }
            Ok((result, callbacks == 0 && unchanged))
        }
        _ => unreachable!("validated native command"),
    }
}
#[cfg(not(target_os = "macos"))]
fn native(_: &[&str]) -> Result<(Value, bool), CoreAudioError> {
    Err(CoreAudioError::new(
        ErrorCode::CapabilityUnsupported,
        "Core Audio diagnostics require macOS",
    ))
}
fn run(args: &[String]) -> Result<(Value, bool), CoreAudioError> {
    let args: Vec<_> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["--help"] => Ok((
            json!({"schema_version":1,"status":"pass",
            "usage":"earph-coreaudio devices | inspect --device-uid UID | precision-check --request PATH | registration-check --device-uid UID | redact --record PATH",
            "safety":"read-only diagnostics; callback registration is never started; no format, volume, or ownership changes"}),
            true,
        )),
        ["devices"]
        | ["inspect" | "registration-check", "--device-uid", _]
        | ["precision-check", "--request", _] => native(&args),
        ["redact", "--record", path] => {
            let bytes = earph_harness::read_bounded(std::path::Path::new(path), 2 * 1024 * 1024)
                .map_err(|_| {
                    CoreAudioError::new(
                        ErrorCode::AssetInvalid,
                        "cannot read a bounded diagnostic record",
                    )
                })?;
            Ok((earph_coreaudio::redact_record(&bytes)?, true))
        }
        _ => Err(CoreAudioError::new(
            ErrorCode::AssetInvalid,
            "unknown command or options; use --help",
        )),
    }
}
fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let (result, success) = match run(&args) {
        Ok(result) => result,
        Err(error) => (
            json!({"schema_version":1,"status":"fail","error":error}),
            false,
        ),
    };
    let output = serde_json::to_vec(&result).expect("serializable diagnostic outcome");
    if output.len() > 2 * 1024 * 1024 {
        println!(
            "{}",
            json!({"schema_version":1,"status":"fail","error":CoreAudioError::new(ErrorCode::ResourceBudgetExceeded, "diagnostic response exceeds 2 MiB; inspect one explicit device")})
        );
        return ExitCode::from(2);
    }
    println!("{}", std::str::from_utf8(&output).expect("JSON is UTF-8"));
    if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}
