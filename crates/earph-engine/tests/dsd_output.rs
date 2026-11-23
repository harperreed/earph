// ABOUTME: Tests generated DSD files through the real typed ALSA software boundary.
// ABOUTME: Independently checks retained carrier bytes without starting Engine playback.
#![cfg(target_os = "linux")]
use earph_alsa::{DopOutput, OpenRequest, Route, SignalPermission, WriteProgress};
use earph_core::{
    audio::{ChannelLayout, DsdBlock, MemoryBudget, PcmFormat, PcmRepresentation},
    dop::{DopPacker, DopStream},
    ids::Epochs,
};
use earph_media::dsd::DsdFile;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
#[allow(dead_code)]
#[path = "../../earph-media/tests/support/dsd.rs"]
mod containers;

fn config(capture: &Path) -> String {
    format!(
        "pcm.null {{ type null }}\npcm.capture {{ type file slave.pcm null file \"{}\" format raw }}\n",
        capture.display()
    )
}
fn isolated(test: &str) -> Option<PathBuf> {
    let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/dsd-output");
    if std::env::var("EARPH_DSD_OUTPUT_CHILD").as_deref() == Ok(test) {
        let directory = PathBuf::from(std::env::var_os("EARPH_DSD_OUTPUT_ARTIFACTS").unwrap());
        assert_eq!(
            directory.parent().unwrap().canonicalize().unwrap(),
            base.canonicalize().unwrap()
        );
        let capture = directory.join("capture.dop");
        let conf = directory.join("alsa.conf");
        assert_eq!(
            std::env::var_os("ALSA_CONFIG_PATH").unwrap(),
            conf.as_os_str()
        );
        assert_eq!(fs::read_to_string(conf).unwrap(), config(&capture));
        return Some(directory);
    }
    fs::create_dir_all(&base).unwrap();
    let directory = base.join(format!(
        "{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&directory).unwrap();
    let conf = directory.join("alsa.conf");
    fs::write(&conf, config(&directory.join("capture.dop"))).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("EARPH_DSD_OUTPUT_CHILD", test)
        .env("EARPH_DSD_OUTPUT_ARTIFACTS", &directory)
        .env("ALSA_CONFIG_PATH", &conf)
        .output()
        .unwrap();
    fs::write(directory.join("test.stdout"), &output.stdout).unwrap();
    fs::write(directory.join("test.stderr"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{} / {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    None
}
fn consume(path: &Path) -> Vec<u8> {
    let mut input = std::io::BufReader::with_capacity(5, fs::File::open(path).unwrap());
    let mut recovered = Vec::new();
    let mut marker = 0x05;
    loop {
        let mut frame = [0; 6];
        if input.read(&mut frame[..1]).unwrap() == 0 {
            break;
        }
        input.read_exact(&mut frame[1..]).unwrap();
        assert_eq!((frame[2], frame[5]), (marker, marker));
        recovered.extend([frame[1], frame[4], frame[0], frame[3]]);
        marker = if marker == 0x05 { 0xfa } else { 0x05 };
    }
    recovered
}
#[test]
fn generated_dsf_dff_reader_packer_and_native_capture_agree() {
    let Some(directory) = isolated("generated_dsf_dff_reader_packer_and_native_capture_agree")
    else {
        return;
    };
    let known: Vec<u8> = (0..4102)
        .flat_map(|i| [(i % 251) as u8, (255 - i % 239) as u8])
        .collect();
    for rate in [2_822_400, 5_644_800] {
        for (kind, bytes) in [
            ("dsf-lsb", containers::dsf(&known, 1, rate, b"tag")),
            ("dsf-msb", containers::dsf(&known, 8, rate, b"")),
            ("dff", containers::dff(&known, rate)),
        ] {
            let source = directory.join(format!("{kind}-{rate}.dsd"));
            fs::write(&source, &bytes).unwrap();
            let mut reader =
                DsdFile::open(&source, Epochs::default(), MemoryBudget::default()).unwrap();
            for start_bits in [0, 16, 32752] {
                let epochs = Epochs {
                    session: rate.into(),
                    stream: start_bits + 1,
                };
                reader.seek(start_bits, epochs).unwrap();
                let stream = DopStream {
                    bit_rate_hz: rate,
                    channels: ChannelLayout::Stereo,
                    epochs,
                    source_sample_position: start_bits,
                };
                let mut packer = DopPacker::new(stream, MemoryBudget::default()).unwrap();
                let request = OpenRequest {
                    schema_version: 1,
                    device: "capture".into(),
                    route: Route::System,
                    format: PcmFormat::new(
                        rate / 16,
                        ChannelLayout::Stereo,
                        PcmRepresentation::Signed24,
                    )
                    .unwrap(),
                    period_frames: 31,
                    buffer_frames: 124,
                };
                let mut output = DopOutput::open(request, stream).unwrap();
                assert_eq!(output.submitted_frames(), 0);
                let read_limit = 4096.min(output.report().requested.period_frames as usize * 2);
                let mut emitted = 0;
                let mut expected_wire: Vec<u8> = Vec::new();
                while let Some(source) = reader.next_block(read_limit).unwrap() {
                    // Odd fragments force the packer's remainder across reader boundaries.
                    for bytes in source.block.payload.chunks(7) {
                        let fragment =
                            DsdBlock::new(bytes.to_vec(), rate, ChannelLayout::Stereo, epochs)
                                .unwrap();
                        if let Some(block) = packer.push(&fragment).unwrap() {
                            assert!(block.frame_count() <= 31);
                            expected_wire.extend(block.payload());
                            let mut offset = 0;
                            let mut attempts = 0;
                            while offset < block.frame_count() as usize {
                                attempts += 1;
                                assert!(
                                    attempts <= 100,
                                    "software capture made no bounded progress"
                                );
                                match output
                                    .write(
                                        &block,
                                        offset,
                                        block.frame_count() as usize - offset,
                                        SignalPermission::Granted,
                                    )
                                    .unwrap()
                                {
                                    WriteProgress::Accepted { frames } => {
                                        offset += frames;
                                        emitted += frames as u64;
                                    }
                                    WriteProgress::WouldBlock => {
                                        assert!(output.wait_ready(1).unwrap());
                                    }
                                }
                            }
                        }
                    }
                }
                packer.finish().unwrap();
                assert_eq!(output.submitted_frames(), emitted);
                assert_eq!(output.progress().unwrap().submitted_frames, emitted);
                output.stop().unwrap();
                let capture = directory.join("capture.dop");
                let wire = fs::read(&capture).unwrap();
                assert_eq!(wire, expected_wire);
                let recovered = consume(&capture);
                assert_eq!(recovered, &known[(start_bits / 8 * 2) as usize..]);
                assert_eq!(wire.len(), recovered.len() / 4 * 6);
                fs::copy(
                    capture,
                    directory.join(format!("{kind}-{rate}-{start_bits}.dop")),
                )
                .unwrap();
                assert_eq!(fs::read(&source).unwrap(), bytes);
            }
        }
    }
}
