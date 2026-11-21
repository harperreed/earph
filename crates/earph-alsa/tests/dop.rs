// ABOUTME: Tests the typed DoP boundary with isolated real software ALSA streams.
// ABOUTME: Retains generated captures and keeps physical endpoints unreachable.
#![cfg(target_os = "linux")]
use earph_alsa::*;
use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    dop::{DopBlock, DopStream},
    ids::Epochs,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

fn config(file: &Path) -> String {
    format!(
        "pcm.null {{ type null }}\npcm.capture {{ type file slave.pcm null file \"{}\" format raw }}\npcm.full {{ type file slave.pcm null file \"/dev/full\" format raw }}\n",
        file.display()
    )
}
fn isolated(test: &str, parallel: bool, diagnostic: Option<&str>) -> Option<PathBuf> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/dop-tests");
    if std::env::var("EARPH_DOP_CHILD").as_deref() == Ok(test) {
        let directory = PathBuf::from(std::env::var_os("EARPH_DOP_ARTIFACTS").unwrap());
        assert_eq!(
            directory.parent().unwrap().canonicalize().unwrap(),
            base.canonicalize().unwrap()
        );
        let conf = directory.join("alsa.conf");
        assert_eq!(
            std::env::var_os("ALSA_CONFIG_PATH").unwrap(),
            conf.as_os_str()
        );
        assert_eq!(
            fs::read_to_string(conf).unwrap(),
            config(&directory.join("capture.dop"))
        );
        return Some(directory);
    }
    fs::create_dir_all(&base).unwrap();
    let children: Vec<_> = (0..if parallel { 2 } else { 1 })
        .map(|index| {
            let directory = base.join(format!(
                "{}-{}-{index}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            fs::create_dir(&directory).unwrap();
            let conf = directory.join("alsa.conf");
            fs::write(&conf, config(&directory.join("capture.dop"))).unwrap();
            let child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env("EARPH_DOP_CHILD", test)
                .env("EARPH_DOP_ARTIFACTS", &directory)
                .env("EARPH_DOP_VARIANT", index.to_string())
                .env("ALSA_CONFIG_PATH", &conf)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            (directory, child)
        })
        .collect();
    let mut captures = Vec::new();
    for (directory, child) in children {
        let result = child.wait_with_output().unwrap();
        fs::write(directory.join("test.stdout"), &result.stdout).unwrap();
        fs::write(directory.join("test.stderr"), &result.stderr).unwrap();
        assert!(
            result.status.success(),
            "{} / {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        if let Some(diagnostic) = diagnostic {
            assert!(String::from_utf8_lossy(&result.stderr).contains(diagnostic));
        }
        if parallel {
            captures.push(fs::read(directory.join("capture.dop")).unwrap());
        }
    }
    if parallel {
        assert_ne!(captures[0], captures[1]);
    }
    None
}
fn request(rate: u32) -> OpenRequest {
    OpenRequest {
        schema_version: 1,
        device: "capture".into(),
        route: Route::System,
        format: PcmFormat::new(
            rate / 16,
            ChannelLayout::Stereo,
            PcmRepresentation::Signed24,
        )
        .unwrap(),
        period_frames: 64,
        buffer_frames: 256,
    }
}
fn stream(rate: u32) -> DopStream {
    DopStream {
        bit_rate_hz: rate,
        channels: ChannelLayout::Stereo,
        epochs: Epochs {
            session: 3,
            stream: 7,
        },
        source_sample_position: 0,
    }
}
fn block(s: DopStream, sequence: u64, position: u64, frames: usize, marker: u8) -> DopBlock {
    let mut payload = Vec::new();
    let mut phase = marker;
    for index in 0..frames {
        payload.extend([0xff, index as u8, phase, 0x80, 0x7f, phase]);
        phase = if phase == 0x05 { 0xfa } else { 0x05 };
    }
    DopBlock::new(
        payload,
        s.bit_rate_hz / 16,
        s.channels,
        s.epochs,
        sequence,
        position,
    )
    .unwrap()
}
fn accepted(output: &mut DopOutput, block: &DopBlock, offset: usize, frames: usize) {
    assert_eq!(
        output
            .write(block, offset, frames, SignalPermission::Granted)
            .unwrap(),
        WriteProgress::Accepted { frames }
    );
}
fn descriptors(path: &Path) -> usize {
    let path = path
        .parent()
        .unwrap()
        .canonicalize()
        .unwrap()
        .join(path.file_name().unwrap());
    fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|e| fs::read_link(e.path()).ok())
        .filter(|p| p == &path)
        .count()
}
#[test]
fn opening_checks_exact_carrier_without_submitting_or_qualifying() {
    let Some(directory) = isolated(
        "opening_checks_exact_carrier_without_submitting_or_qualifying",
        false,
        None,
    ) else {
        return;
    };
    for rate in [2_822_400, 5_644_800] {
        let mut output = DopOutput::open(request(rate), stream(rate)).unwrap();
        assert_eq!(output.report().observed.format, request(rate).format);
        assert_eq!(output.report().observed.alsa_format, "S24_3LE");
        assert_eq!(output.report().payload_preservation, "unverified");
        assert_eq!(output.report().evidence_level, "opened");
        assert_eq!(output.report().downstream_transformations, "unknown");
        assert_eq!(output.state(), OutputState::Prepared);
        assert_eq!(output.submitted_frames(), 0);
        assert_eq!(output.progress().unwrap().submitted_frames, 0);
        assert_eq!(output.audio_storage_bytes().unwrap(), 64 * 6);
        output.stop().unwrap();
        assert!(fs::read(directory.join("capture.dop")).unwrap().is_empty());
    }
    let s = stream(2_822_400);
    let mut mismatches = Vec::new();
    for (rate, layout, representation) in [
        (352_800, ChannelLayout::Stereo, PcmRepresentation::Signed24),
        (176_400, ChannelLayout::Mono, PcmRepresentation::Signed24),
        (176_400, ChannelLayout::Stereo, PcmRepresentation::Signed32),
    ] {
        let mut r = request(s.bit_rate_hz);
        r.format = PcmFormat::new(rate, layout, representation).unwrap();
        mismatches.push((r, s));
    }
    for altered in [
        DopStream {
            source_sample_position: 1,
            ..s
        },
        DopStream {
            channels: ChannelLayout::Mono,
            ..s
        },
        DopStream {
            bit_rate_hz: 11_289_600,
            ..s
        },
    ] {
        mismatches.push((request(s.bit_rate_hz), altered));
    }
    for (mut r, s) in mismatches {
        // This endpoint is absent from the private config. Format rejection must precede open.
        r.device = "unconfigured".into();
        let error = DopOutput::open(r, s)
            .err()
            .expect("invalid typed open succeeded");
        assert!(matches!(
            error.code,
            ErrorCode::FormatUnsupported | ErrorCode::AssetInvalid
        ));
        assert_eq!(error.native_errno, None);
    }
}
#[test]
fn exact_carrier_prefixes_and_rejections_preserve_one_pending_block() {
    let Some(directory) = isolated(
        "exact_carrier_prefixes_and_rejections_preserve_one_pending_block",
        false,
        None,
    ) else {
        return;
    };
    let s = stream(2_822_400);
    let mut output = DopOutput::open(request(s.bit_rate_hz), s).unwrap();
    let first = block(s, 0, 0, 5, 0x05);
    assert_eq!(
        output
            .write(&first, 0, 1, SignalPermission::Denied)
            .unwrap_err()
            .code,
        ErrorCode::SafetyPolicyBlocked
    );
    let stale = block(
        DopStream {
            epochs: Epochs::default(),
            ..s
        },
        0,
        0,
        5,
        0x05,
    );
    assert_eq!(
        output
            .write(&stale, 0, 1, SignalPermission::Granted)
            .unwrap_err()
            .code,
        ErrorCode::StaleSession
    );
    let other_rate = block(stream(5_644_800), 0, 0, 5, 0x05);
    assert_eq!(
        output
            .write(&other_rate, 0, 1, SignalPermission::Granted)
            .unwrap_err()
            .code,
        ErrorCode::FormatUnsupported
    );
    for (offset, count) in [(0, 0), (1, 1), (5, 1), (usize::MAX, 2), (0, 6)] {
        assert!(
            output
                .write(&first, offset, count, SignalPermission::Granted)
                .is_err()
        );
    }
    for invalid in [
        block(s, 1, 0, 5, 0x05),
        block(s, 0, 1, 5, 0x05),
        block(s, 0, 0, 5, 0xfa),
        block(s, 0, 0, 65, 0x05),
        block(s, u64::MAX, 0, 5, 0x05),
    ] {
        assert!(
            output
                .write(&invalid, 0, 1, SignalPermission::Granted)
                .is_err()
        );
    }
    assert_eq!(output.submitted_frames(), 0);
    accepted(&mut output, &first, 0, 1);
    let mut replaced = first.payload().to_vec();
    replaced[6] ^= 1;
    let replaced = DopBlock::new(replaced, 176_400, s.channels, s.epochs, 0, 0).unwrap();
    for invalid in [replaced, block(s, 0, 0, 4, 0x05), block(s, 1, 1, 4, 0xfa)] {
        assert!(
            output
                .write(&invalid, 1, 1, SignalPermission::Granted)
                .is_err()
        );
    }
    for offset in [0, 2] {
        assert!(
            output
                .write(&first, offset, 1, SignalPermission::Granted)
                .is_err()
        );
    }
    assert_eq!(output.submitted_frames(), 1);
    accepted(&mut output, &first, 1, 2);
    accepted(&mut output, &first, 3, 2);
    assert!(
        output
            .write(&first, 0, 1, SignalPermission::Granted)
            .is_err()
    );
    let next = block(s, 1, 5, 3, 0xfa);
    let wrong_phase = block(s, 1, 5, 3, 0x05);
    assert!(
        output
            .write(&wrong_phase, 0, 3, SignalPermission::Granted)
            .is_err()
    );
    accepted(&mut output, &next, 0, 3);
    assert_eq!(output.progress().unwrap().submitted_frames, 8);
    output.stop().unwrap();
    let mut expected = first.payload().to_vec();
    expected.extend(next.payload());
    assert_eq!(fs::read(directory.join("capture.dop")).unwrap(), expected);
}
#[test]
fn stop_drop_and_fresh_epochs_keep_only_accepted_prefixes() {
    let Some(directory) = isolated(
        "stop_drop_and_fresh_epochs_keep_only_accepted_prefixes",
        false,
        None,
    ) else {
        return;
    };
    let file = directory.join("capture.dop");
    let s = stream(2_822_400);
    assert_eq!(descriptors(&file), 0);
    let first = block(s, 0, 0, 5, 0x05);
    let mut output = DopOutput::open(request(s.bit_rate_hz), s).unwrap();
    assert_eq!(descriptors(&file), 1);
    accepted(&mut output, &first, 0, 1);
    output.stop().unwrap();
    output.stop().unwrap();
    assert_eq!(descriptors(&file), 0);
    assert_eq!(output.submitted_frames(), 1);
    assert!(output.progress().is_err());
    assert_eq!(
        output
            .write(&first, 1, 4, SignalPermission::Granted)
            .unwrap_err()
            .code,
        ErrorCode::OutputFault
    );
    assert_eq!(fs::read(&file).unwrap(), &first.payload()[..6]);
    fs::copy(&file, directory.join("stopped-prefix.dop")).unwrap();
    let fresh = DopStream {
        epochs: Epochs {
            session: 3,
            stream: 8,
        },
        source_sample_position: 16,
        ..s
    };
    let mut output = DopOutput::open(request(s.bit_rate_hz), fresh).unwrap();
    assert_eq!(
        output
            .write(&first, 0, 1, SignalPermission::Granted)
            .unwrap_err()
            .code,
        ErrorCode::StaleSession
    );
    assert!(
        output
            .write(
                &block(fresh, 0, 0, 3, 0x05),
                0,
                3,
                SignalPermission::Granted
            )
            .is_err()
    );
    let resumed = block(fresh, 0, 1, 3, 0x05);
    accepted(&mut output, &resumed, 0, 3);
    drop(output);
    assert_eq!(descriptors(&file), 0);
    assert_eq!(fs::read(file).unwrap(), resumed.payload());
}
#[test]
fn real_native_error_retires_handle_and_preserves_accepted_count() {
    let Some(_directory) = isolated(
        "real_native_error_retires_handle_and_preserves_accepted_count",
        false,
        Some("/dev/full write failed"),
    ) else {
        return;
    };
    let s = stream(2_822_400);
    let mut r = request(s.bit_rate_hz);
    r.device = "full".into();
    let baseline = descriptors(Path::new("/dev/full"));
    let mut output = DopOutput::open(r, s).unwrap();
    assert_eq!(descriptors(Path::new("/dev/full")), baseline + 1);
    let mut failure = None;
    for sequence in 0..256 {
        let b = block(s, sequence, sequence * 64, 64, 0x05);
        let before = output.submitted_frames();
        match output.write(&b, 0, 64, SignalPermission::Granted) {
            Err(error) => {
                assert_eq!(output.submitted_frames(), before);
                failure = Some(error);
                break;
            }
            Ok(WriteProgress::Accepted { frames }) => {
                assert_eq!(output.submitted_frames(), before + frames as u64)
            }
            Ok(WriteProgress::WouldBlock) => {
                panic!("file-to-null unexpectedly blocked; no native EAGAIN claim is made")
            }
        }
    }
    let error = failure.expect("real file plugin must eventually fail");
    assert_eq!(error.code, ErrorCode::OutputFault);
    assert!(error.native_errno.is_some_and(|errno| errno > 0));
    assert_eq!(output.state(), OutputState::Failed);
    assert_eq!(descriptors(Path::new("/dev/full")), baseline);
    let before = output.submitted_frames();
    assert!(
        output
            .write(&block(s, 0, 0, 1, 0x05), 0, 1, SignalPermission::Granted)
            .is_err()
    );
    output.stop().unwrap();
    assert_eq!(output.state(), OutputState::Failed);
    drop(output);
    assert_eq!(descriptors(Path::new("/dev/full")), baseline);
    assert!(before > 0);
}
#[test]
fn software_streams_remain_isolated() {
    let Some(directory) = isolated("software_streams_remain_isolated", true, None) else {
        return;
    };
    let variant = std::env::var("EARPH_DOP_VARIANT")
        .unwrap()
        .parse::<u64>()
        .unwrap();
    let s = DopStream {
        epochs: Epochs {
            session: 100 + variant,
            stream: 200 + variant,
        },
        source_sample_position: variant * 16,
        ..stream(2_822_400)
    };
    let mut output = DopOutput::open(request(s.bit_rate_hz), s).unwrap();
    let first = block(s, 0, variant, 1 + variant as usize, 0x05);
    accepted(&mut output, &first, 0, first.frame_count() as usize);
    let next = block(
        s,
        1,
        variant + first.frame_count(),
        2,
        if variant == 0 { 0xfa } else { 0x05 },
    );
    accepted(&mut output, &next, 0, 2);
    assert_eq!(output.submitted_frames(), 3 + variant);
    output.stop().unwrap();
    let mut expected = first.payload().to_vec();
    expected.extend(next.payload());
    assert_eq!(fs::read(directory.join("capture.dop")).unwrap(), expected);
}
