// ABOUTME: Validates explicit ALSA requests before opening or emitting samples.
// ABOUTME: Preserves partial progress when an approved exercise fails.
use earph_alsa::{
    AlsaError, AlsaOutput, OpenRequest, SignalPermission, WriteProgress, devices, library_version,
};
use earph_core::{
    ErrorCode,
    audio::{MemoryBudget, PcmBlock},
    ids::Epochs,
};
use earph_harness::{build_provenance, fixture, read_bounded};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::ExitCode,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Failure {
    error: AlsaError,
    partial: Option<Value>,
}
impl From<AlsaError> for Failure {
    fn from(error: AlsaError) -> Self {
        Self {
            error,
            partial: None,
        }
    }
}
fn invalid(message: &'static str) -> AlsaError {
    AlsaError::new(ErrorCode::AssetInvalid, message)
}

fn request(path: &str) -> Result<OpenRequest, Failure> {
    let bytes = read_bounded(Path::new(path), 64 * 1024)
        .map_err(|_| invalid("cannot read a bounded ALSA request file"))?;
    let request: OpenRequest = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("invalid ALSA request JSON or required fields"))?;
    request.validate()?;
    Ok(request)
}
fn envelope(command: &str) -> Value {
    json!({
        "schema_version":1,"status":"pass","command":command,
        "build":build_provenance(),
        "observed_at_unix_ns":SystemTime::now().duration_since(UNIX_EPOCH).ok().map(|value|value.as_nanos().to_string()),
        "environment":{"os":std::env::consts::OS,"architecture":std::env::consts::ARCH,
            "kernel":std::fs::read_to_string("/proc/sys/kernel/osrelease").ok().map(|value|value.trim().to_owned()),
            "alsa_version":library_version()},
    })
}
fn failed_output(error: AlsaError, output: &mut AlsaOutput) -> Failure {
    let state_at_failure = output.state();
    let stop_error = output.stop().err();
    Failure {
        error,
        partial: Some(
            json!({"report":output.report(),"state_at_failure":state_at_failure,
        "final_state":output.state(),"submitted_frames":output.submitted_frames(),"stop_error":stop_error}),
        ),
    }
}

fn exercise(request: OpenRequest, path: &str) -> Result<Value, Failure> {
    let (manifest, source) = fixture::load_fixture(Path::new(path))
        .map_err(|_| invalid("cannot load the fixture or verify its payload"))?;
    if manifest.spec.format != request.format {
        return Err(AlsaError::new(
            ErrorCode::FormatUnsupported,
            "fixture format differs from the exact request",
        )
        .into());
    }
    let duration_ns = u128::from(manifest.spec.frames) * 1_000_000_000
        / u128::from(request.format.sample_rate_hz());
    if duration_ns > 20_000_000_000 {
        return Err(AlsaError::new(
            ErrorCode::ResourceBudgetExceeded,
            "spike exercises are limited to 20 seconds of PCM",
        )
        .into());
    }
    let mut block = PcmBlock::prepare(
        request.format,
        request.period_frames as usize,
        Epochs::default(),
        MemoryBudget::default(),
    )
    .map_err(|_| {
        AlsaError::new(
            ErrorCode::ResourceBudgetExceeded,
            "cannot prepare fixture block",
        )
    })?;
    let mut output = AlsaOutput::open(request.clone(), Epochs::default())?;
    let deadline =
        Instant::now() + Duration::from_nanos(duration_ns as u64) + Duration::from_secs(2);
    let result = (|| -> Result<_, AlsaError> {
        let mut position = 0;
        while position < manifest.spec.frames {
            let frames = u64::from(request.period_frames).min(manifest.spec.frames - position);
            let samples = request
                .format
                .scalar_count(frames)
                .map_err(|_| invalid("invalid fixture sample count"))?;
            let start = request
                .format
                .scalar_count(position)
                .map_err(|_| invalid("invalid fixture position"))?;
            for index in 0..samples {
                block
                    .set_integer_sample(
                        index,
                        source
                            .integer_sample(start + index)
                            .ok_or(invalid("fixture ended before its declared frame count"))?,
                    )
                    .map_err(|_| invalid("invalid integer fixture sample"))?;
            }
            block
                .set_span(
                    Epochs::default(),
                    position / u64::from(request.period_frames),
                    position,
                    frames,
                )
                .map_err(|_| invalid("invalid fixture block span"))?;
            let mut offset = 0;
            while offset < frames as usize {
                if Instant::now() >= deadline {
                    return Err(AlsaError::new(
                        ErrorCode::OutputFault,
                        "ALSA exercise timed out; output stopped",
                    ));
                }
                match output.write(
                    &block,
                    offset,
                    frames as usize - offset,
                    SignalPermission::Granted,
                )? {
                    WriteProgress::Accepted { frames } => offset += frames,
                    WriteProgress::WouldBlock => std::thread::sleep(Duration::from_millis(1)),
                }
            }
            position += frames;
        }
        loop {
            let progress = output.progress()?;
            if progress.backend_delay_frames == 0 {
                return Ok(progress);
            }
            if Instant::now() >= deadline {
                return Err(AlsaError::new(
                    ErrorCode::OutputFault,
                    "ALSA consumption estimate timed out; output stopped",
                ));
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    })();
    let progress = result.map_err(|error| failed_output(error, &mut output))?;
    output
        .stop()
        .map_err(|error| failed_output(error, &mut output))?;
    let mut value = envelope("exercise");
    value["report"] = serde_json::to_value(output.report()).expect("serializable open report");
    value["exercise"] = json!({
        "evidence_level":"exercised", "submitted_frames":progress.submitted_frames,
        "backend_delay_frames":progress.backend_delay_frames,
        "estimated_consumed_frames":progress.estimated_consumed_frames,
        "measurement_boundary":progress.measurement_boundary,
        "fixture_payload_sha256":manifest.payload_sha256,
        "completion":"submitted_and_backend_delay_zero",
        "physical_output":"unverified",
    });
    value["closed"] = true.into();
    Ok(value)
}
fn run(args: &[String]) -> Result<Value, Failure> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["--help"] => Ok(
            json!({"status":"pass","usage":"earph-alsa devices | open --request PATH | exercise --request PATH --fixture PATH --allow-signal | matrix-check --record PATH","safety":"open submits no samples; exercise requires approval for the selected route"}),
        ),
        ["matrix-check", "--record", path] => {
            earph_alsa::validate_matrix(Path::new(path)).map_err(|detail| Failure {
                error: invalid("ALSA matrix validation failed"),
                partial: Some(json!({"validation_error":detail})),
            })
        }
        ["devices"] => {
            let mut result = envelope("devices");
            result["devices"] =
                serde_json::to_value(devices()?).expect("serializable device hints");
            result["evidence_level"] = "advertised".into();
            result["signals_submitted"] = 0.into();
            Ok(result)
        }
        ["open", "--request", path] => {
            let mut output = AlsaOutput::open(request(path)?, Epochs::default())?;
            let progress = output
                .progress()
                .map_err(|error| failed_output(error, &mut output))?;
            output
                .stop()
                .map_err(|error| failed_output(error, &mut output))?;
            let mut result = envelope("open");
            result["report"] =
                serde_json::to_value(output.report()).expect("serializable open report");
            result["progress"] =
                serde_json::to_value(progress).expect("serializable output progress");
            result["signals_submitted"] = 0.into();
            result["closed"] = true.into();
            Ok(result)
        }
        ["exercise", "--request", _, "--fixture", _] => Err(AlsaError::new(
            ErrorCode::SafetyPolicyBlocked,
            "obtain signal approval for the selected route, then pass --allow-signal",
        )
        .into()),
        [
            "exercise",
            "--request",
            path,
            "--fixture",
            fixture,
            "--allow-signal",
        ] => exercise(request(path)?, fixture),
        _ => Err(invalid("unknown command or options; use --help").into()),
    }
}
pub fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(result) => {
            println!("{result}");
            ExitCode::SUCCESS
        }
        Err(failure) => {
            println!(
                "{}",
                json!({"schema_version":1,"status":"fail","error":failure.error,"partial":failure.partial})
            );
            ExitCode::from(2)
        }
    }
}
