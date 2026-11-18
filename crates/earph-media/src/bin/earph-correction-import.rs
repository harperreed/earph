// ABOUTME: Imports or exports correction definitions through a bounded offline command.
// ABOUTME: Emits validated graph intent without contacting an owner or granting playback authority.
#![forbid(unsafe_code)]
use earph_core::audio::ChannelLayout;
use earph_media::correction_text::{
    CorrectionFormat, ImportContext, ImportError, ImportMetadata, MAX_CORRECTION_BYTES,
    MAX_REPORT_BYTES, export_correction_text, import_correction_text, read_correction_file,
    read_correction_report,
};
use std::{
    collections::BTreeSet,
    io::{self, Write},
    path::PathBuf,
    process::ExitCode,
};

const USAGE: &str = "usage: earph-correction-import --format rew|autoeq|camilladsp --rate HZ --channels mono|stereo --quantum FRAMES [metadata options] [--] FILE\n       earph-correction-import --validate-report REPORT.json\n       earph-correction-import --export REPORT.json\nmetadata options: --model --provider --target --parameter-set --revision --license VALUE";
fn error(code: &'static str, message: &'static str) -> ImportError {
    ImportError {
        code,
        message,
        line: None,
    }
}
fn usage() -> ImportError {
    error("USAGE", USAGE)
}
fn write(bytes: &[u8]) -> Result<(), ImportError> {
    let mut out = io::stdout().lock();
    out.write_all(bytes)
        .and_then(|_| out.write_all(b"\n"))
        .map_err(|_| error("OUTPUT_UNAVAILABLE", "cannot write the correction result"))
}
fn print_report(
    report: &earph_media::correction_text::CorrectionImport,
) -> Result<(), ImportError> {
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|_| error("INVALID_REPORT", "cannot encode correction result"))?;
    write(&bytes)
}

fn run() -> Result<(), ImportError> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--help" {
        return write(USAGE.as_bytes());
    }
    if args.len() == 2 && (args[0] == "--export" || args[0] == "--validate-report") {
        let bytes = read_correction_file(&PathBuf::from(&args[1]), MAX_REPORT_BYTES)?;
        let report = read_correction_report(&bytes)?;
        return if args[0] == "--export" {
            let source = export_correction_text(&report)?;
            write(source.as_bytes())
        } else {
            print_report(&report)
        };
    }
    let (mut format, mut rate, mut channels, mut quantum, mut path) =
        (None, None, None, None, None);
    let mut metadata = ImportMetadata::default();
    let mut seen = BTreeSet::new();
    let mut position = 0;
    let mut literal = false;
    while position < args.len() {
        let argument = &args[position];
        position += 1;
        if !literal && argument == "--" {
            literal = true;
            continue;
        }
        if !literal && argument.to_str().is_some_and(|s| s.starts_with('-')) {
            let flag = argument.to_str().ok_or_else(usage)?;
            if !seen.insert(flag) {
                return Err(usage());
            }
            let value = args
                .get(position)
                .and_then(|s| s.to_str())
                .ok_or_else(usage)?;
            position += 1;
            match flag {
                "--format" => {
                    format = Some(match value {
                        "rew" => CorrectionFormat::Rew,
                        "autoeq" => CorrectionFormat::AutoEq,
                        "camilladsp" => CorrectionFormat::CamillaDsp,
                        _ => return Err(usage()),
                    })
                }
                "--rate" => rate = Some(value.parse::<u32>().map_err(|_| usage())?),
                "--channels" => {
                    channels = Some(match value {
                        "mono" => ChannelLayout::Mono,
                        "stereo" => ChannelLayout::Stereo,
                        _ => return Err(usage()),
                    })
                }
                "--quantum" => quantum = Some(value.parse::<usize>().map_err(|_| usage())?),
                "--model" => metadata.headphone_model = Some(value.into()),
                "--provider" => metadata.measurement_provider = Some(value.into()),
                "--target" => metadata.target = Some(value.into()),
                "--parameter-set" => metadata.parameter_set = Some(value.into()),
                "--revision" => metadata.revision = Some(value.into()),
                "--license" => metadata.redistribution_license = Some(value.into()),
                _ => return Err(usage()),
            }
        } else if path.replace(PathBuf::from(argument)).is_some() {
            return Err(usage());
        }
    }
    let format = format.ok_or_else(usage)?;
    let context = ImportContext {
        sample_rate_hz: rate.ok_or_else(usage)?,
        channels: channels.ok_or_else(usage)?,
        quantum_frames: quantum.ok_or_else(usage)?,
    };
    context.validate()?;
    let source = read_correction_file(&path.ok_or_else(usage)?, MAX_CORRECTION_BYTES)?;
    let report = import_correction_text(&source, format, context, metadata)?;
    print_report(&report)
}
fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", serde_json::json!({"error":error}));
            ExitCode::from(2)
        }
    }
}
