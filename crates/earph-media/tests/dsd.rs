// ABOUTME: Exercises real generated DSD containers and typed software transport.
// ABOUTME: Uses known source bytes and an independent carrier consumer.
use earph_core::{audio::MemoryBudget, ids::Epochs};
use earph_media::dsd::DsdFile;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

fn fixture(bytes: &[u8]) -> PathBuf {
    use std::io::Write;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let root = std::env::var_os("EARPH_DSD_TEST_ROOT")
        .unwrap_or_else(|| std::env::temp_dir().into_os_string());
    loop {
        let path = PathBuf::from(&root).join(format!(
            "dsd-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(bytes).unwrap();
                return path;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("cannot create retained DSD fixture: {error}"),
        }
    }
}

fn chunk(id: &[u8; 4], bytes: &[u8]) -> Vec<u8> {
    let mut out = id.to_vec();
    out.extend((bytes.len() as u64).to_be_bytes());
    out.extend(bytes);
    if !bytes.len().is_multiple_of(2) {
        out.push(0);
    }
    out
}
fn properties(rate: u32) -> Vec<u8> {
    let mut p = b"SND ".to_vec();
    p.extend(chunk(b"FS  ", &rate.to_be_bytes()));
    p.extend(chunk(
        b"CHNL",
        &[0, 2, b'S', b'L', b'F', b'T', b'S', b'R', b'G', b'T'],
    ));
    p.extend(chunk(b"CMPR", b"DSD \x03raw"));
    p
}
fn form(children: &[u8]) -> Vec<u8> {
    let mut payload = b"DSD ".to_vec();
    payload.extend(children);
    chunk(b"FRM8", &payload)
}
fn dff(canonical: &[u8], rate: u32) -> Vec<u8> {
    let mut children = chunk(b"FVER", &[1, 5, 0, 0]);
    children.extend(chunk(b"PROP", &properties(rate)));
    children.extend(chunk(b"JUNK", b"odd"));
    children.extend(chunk(b"DSD ", canonical));
    children.extend(chunk(b"TAIL", b"bounded"));
    form(&children)
}
fn read_all(path: &std::path::Path, limit: usize) -> Vec<u8> {
    let mut reader = DsdFile::open(path, Epochs::default(), MemoryBudget::default()).unwrap();
    let mut out = Vec::new();
    let mut sequence = 0;
    while let Some(frame) = reader.next_block(limit).unwrap() {
        assert_eq!(frame.sequence, sequence);
        assert_eq!(frame.source_sample_position, (out.len() / 2 * 8) as u64);
        out.extend(frame.block.payload);
        sequence += 1;
    }
    assert!(reader.next_block(limit).unwrap().is_none());
    out
}

#[test]
fn both_containers_rates_and_bit_orders_preserve_real_files() {
    use sha2::{Digest, Sha256};
    let original = [0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0];
    for rate in [2_822_400, 5_644_800] {
        for bytes in [
            dsf(&original, 1, rate, b""),
            dsf(&original, 8, rate, b"tag"),
            dff(&original, rate),
        ] {
            let path = fixture(&bytes);
            let before = Sha256::digest(&bytes);
            assert_eq!(read_all(&path, 1), original);
            assert_eq!(Sha256::digest(std::fs::read(path).unwrap()), before);
        }
    }
}

#[test]
fn optional_property_chunks_order_versions_and_exact_tag_limit_are_supported() {
    let original = [0x12, 0x34, 0x56, 0x78];
    for minor in [0, 5] {
        let mut p = b"SND ".to_vec();
        p.extend(chunk(b"CMPR", b"DSD \x00"));
        p.extend(chunk(b"NOTE", b"odd"));
        p.extend(chunk(
            b"CHNL",
            &[0, 2, b'S', b'L', b'F', b'T', b'S', b'R', b'G', b'T'],
        ));
        p.extend(chunk(b"FS  ", &2_822_400u32.to_be_bytes()));
        let mut c = chunk(b"FVER", &[1, minor, 0, 0]);
        c.extend(chunk(b"PROP", &p));
        c.extend(chunk(b"DSD ", &original));
        assert_eq!(read_all(&fixture(&form(&c)), 4096), original);
    }
    assert_eq!(
        read_all(
            &fixture(&dsf(&original, 8, 2_822_400, &vec![9; 65536])),
            4096
        ),
        original
    );
    let exact: Vec<u8> = (0..4096).flat_map(|_| [0x12, 0x34]).collect();
    assert_eq!(
        read_all(&fixture(&dsf(&exact, 8, 5_644_800, b"")), 4096),
        exact
    );
}

#[test]
fn aligned_seeks_reset_epochs_sequence_and_exact_end_is_clean() {
    let original: Vec<u8> = (0..4100)
        .flat_map(|i| [(i % 251) as u8, (i % 199) as u8])
        .collect();
    for bytes in [dsf(&original, 1, 2_822_400, b""), dff(&original, 2_822_400)] {
        let path = fixture(&bytes);
        let mut r = DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).unwrap();
        assert_eq!(r.total_samples(), 32800);
        assert_eq!(r.bit_rate_hz(), 2_822_400);
        r.next_block(1).unwrap();
        let epochs = Epochs {
            session: 3,
            stream: 8,
        };
        r.seek(32752, epochs).unwrap();
        let frame = r.next_block(8).unwrap().unwrap();
        assert_eq!(frame.block.payload, &original[8188..]);
        assert_eq!(frame.source_sample_position, 32752);
        assert_eq!(frame.sequence, 0);
        assert_eq!(frame.block.epochs, epochs);
        r.seek(r.total_samples(), epochs).unwrap();
        assert!(r.next_block(1).unwrap().is_none());
        r.seek(16, epochs).unwrap();
        assert_eq!(
            r.next_block(2).unwrap().unwrap().block.payload,
            &original[4..8]
        );
        assert!(r.seek(1, epochs).is_err());
        assert!(r.next_block(1).is_err());
        assert!(r.seek(0, epochs).is_err());
    }
}

#[test]
fn malformed_dsf_is_rejected_before_the_first_block() {
    let valid = dsf(&[0x12, 0x34, 0x56, 0x78], 1, 2_822_400, b"tag");
    let mut cases = Vec::new();
    for (offset, value) in [
        (4, 29u64),
        (12, u64::MAX),
        (20, 93),
        (32, 53),
        (64, 17),
        (64, 0),
        (84, u64::MAX),
    ] {
        let mut b = valid.clone();
        b[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
        cases.push(b);
    }
    for (offset, value) in [
        (40, 2u32),
        (44, 1),
        (48, 1),
        (52, 1),
        (56, 48_000),
        (60, 2),
        (72, 2048),
        (76, 1),
    ] {
        let mut b = valid.clone();
        b[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        cases.push(b);
    }
    let mut padding = valid.clone();
    padding[94] = 1;
    cases.push(padding);
    let mut overlap = valid.clone();
    overlap[20..28].copy_from_slice(&92u64.to_le_bytes());
    cases.push(overlap);
    let mut missing_tag = valid.clone();
    missing_tag[20..28].copy_from_slice(&0u64.to_le_bytes());
    cases.push(missing_tag);
    cases.push(dsf(&[1, 2, 3, 4], 8, 2_822_400, &vec![1; 65537]));
    let mut truncated = valid.clone();
    truncated.truncate(100);
    cases.push(truncated);
    for bytes in cases {
        let path = fixture(&bytes);
        assert!(DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).is_err());
    }
}

#[test]
fn malformed_dff_properties_tails_and_budgets_are_rejected_at_admission() {
    let base = properties(2_822_400);
    let version = chunk(b"FVER", &[1, 5, 0, 0]);
    let audio = chunk(b"DSD ", &[1, 2, 3, 4]);
    let mut cases = Vec::new();
    for version_bytes in [[2, 0, 0, 0], [1, 6, 0, 0], [1, 5, 1, 0], [1, 5, 0, 1]] {
        let mut c = chunk(b"FVER", &version_bytes);
        c.extend(chunk(b"PROP", &base));
        c.extend(&audio);
        cases.push(form(&c));
    }
    for extra in [
        chunk(b"FS  ", &2_822_400u32.to_be_bytes()),
        chunk(
            b"CHNL",
            &[0, 2, b'S', b'L', b'F', b'T', b'S', b'R', b'G', b'T'],
        ),
        chunk(b"CMPR", b"DSD \x03raw"),
    ] {
        let mut p = base.clone();
        p.extend(extra);
        let mut c = version.clone();
        c.extend(chunk(b"PROP", &p));
        c.extend(&audio);
        cases.push(form(&c));
    }
    let mut dst = base.clone();
    let offset = dst.windows(4).position(|x| x == b"DSD ").unwrap();
    dst[offset..offset + 4].copy_from_slice(b"DST ");
    let mut wrong_channel = base.clone();
    let offset = wrong_channel.windows(4).position(|x| x == b"SLFT").unwrap();
    wrong_channel[offset..offset + 4].copy_from_slice(b"LEFT");
    let mut missing = base.clone();
    missing.truncate(4);
    let mut wrong_name = base.clone();
    let offset = wrong_name.windows(4).position(|x| x == b"DSD ").unwrap();
    wrong_name[offset + 4] = 4;
    let mut wrong_fs = base.clone();
    wrong_fs[11] = 5;
    let mut wrong_count = base.clone();
    let offset = wrong_count.windows(4).position(|x| x == b"SLFT").unwrap();
    wrong_count[offset - 1] = 1;
    for p in [
        dst,
        wrong_channel,
        missing,
        properties(48_000),
        wrong_name,
        wrong_fs,
        wrong_count,
    ] {
        let mut c = version.clone();
        c.extend(chunk(b"PROP", &p));
        c.extend(&audio);
        cases.push(form(&c));
    }
    for tail in [
        vec![b'X'; 7],
        chunk(b"DST ", b"bad"),
        chunk(b"DSD ", &[1, 2, 3, 4]),
        chunk(b"PROP", &base),
    ] {
        let mut c = version.clone();
        c.extend(chunk(b"PROP", &base));
        c.extend(&audio);
        c.extend(tail);
        cases.push(form(&c));
    }
    for size in [0, 2, 3, 6] {
        let mut c = version.clone();
        c.extend(chunk(b"PROP", &base));
        c.extend(chunk(b"DSD ", &vec![1; size]));
        cases.push(form(&c));
    }
    let mut no_prop = version.clone();
    no_prop.extend(&audio);
    cases.push(form(&no_prop));
    let mut late = version.clone();
    late.extend(&audio);
    late.extend(chunk(b"PROP", &base));
    cases.push(form(&late));
    let mut many = version.clone();
    many.extend(chunk(b"PROP", &base));
    many.extend(&audio);
    for _ in 0..256 {
        many.extend(chunk(b"JUNK", b""));
    }
    cases.push(form(&many));
    let mut large = version.clone();
    large.extend(chunk(b"PROP", &base));
    large.extend(&audio);
    large.extend(chunk(b"JUNK", &vec![0; 1024 * 1024]));
    cases.push(form(&large));
    let mut bad_pad = chunk(b"JUNK", b"x");
    *bad_pad.last_mut().unwrap() = 1;
    let mut c = version.clone();
    c.extend(chunk(b"PROP", &base));
    c.extend(&audio);
    c.extend(bad_pad);
    cases.push(form(&c));
    let mut c = version;
    c.extend(chunk(b"PROP", &base));
    c.extend(&audio);
    c.extend(b"JUNK");
    c.extend(u64::MAX.to_be_bytes());
    cases.push(form(&c));
    let mut truncated = dff(&[1, 2, 3, 4], 2_822_400);
    truncated.pop();
    cases.push(truncated);
    let mut version_twice = chunk(b"FVER", &[1, 5, 0, 0]);
    version_twice.extend(chunk(b"FVER", &[1, 5, 0, 0]));
    version_twice.extend(chunk(b"PROP", &base));
    version_twice.extend(&audio);
    cases.push(form(&version_twice));
    let mut missing_pad = chunk(b"FVER", &[1, 5, 0, 0]);
    missing_pad.extend(chunk(b"PROP", &base));
    missing_pad.extend(&audio);
    missing_pad.extend(b"JUNK");
    missing_pad.extend(1u64.to_be_bytes());
    missing_pad.push(1);
    cases.push(form(&missing_pad));
    for bytes in cases {
        let path = fixture(&bytes);
        assert!(DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).is_err());
    }
}

#[cfg(target_os = "linux")]
#[test]
fn descriptors_close_after_cancel_read_failure_and_drop() {
    fn count(path: &std::path::Path) -> usize {
        std::fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| std::fs::read_link(entry.path()).ok().as_deref() == Some(path))
            .count()
    }
    let path = fixture(&dsf(&[1, 2, 3, 4], 8, 2_822_400, b""));
    assert_eq!(count(&path), 0);
    let mut r = DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).unwrap();
    assert_eq!(count(&path), 1);
    r.cancel();
    assert_eq!(count(&path), 0);
    drop(r);
    let r = DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).unwrap();
    assert_eq!(count(&path), 1);
    drop(r);
    assert_eq!(count(&path), 0);
    let mut r = DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(92)
        .unwrap();
    assert!(r.next_block(2).is_err());
    assert_eq!(count(&path), 0);
}

#[test]
fn read_failure_cancel_invalid_seek_and_budget_are_terminal() {
    let bytes = dsf(&[1, 2, 3, 4], 8, 2_822_400, b"");
    let path = fixture(&bytes);
    assert!(DsdFile::open(&path, Epochs::default(), MemoryBudget::new(24579).unwrap()).is_err());
    let mut r = DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).unwrap();
    r.cancel();
    assert!(r.next_block(2).is_err());
    assert!(r.seek(0, Epochs::default()).is_err());
    let mut r = DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).unwrap();
    assert!(r.seek(48, Epochs::default()).is_err());
    assert!(r.next_block(2).is_err());
    let mut r = DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(92)
        .unwrap();
    assert!(r.next_block(2).is_err());
    assert!(r.next_block(2).is_err());
    assert!(r.seek(0, Epochs::default()).is_err());
}

fn independently_consume(path: &std::path::Path) -> Vec<u8> {
    use std::io::Read;
    let mut input = std::io::BufReader::with_capacity(5, std::fs::File::open(path).unwrap());
    let mut output = Vec::new();
    let mut marker = 0x05;
    loop {
        let mut frame = [0; 6];
        if input.read(&mut frame[..1]).unwrap() == 0 {
            break;
        }
        input.read_exact(&mut frame[1..]).unwrap();
        assert_eq!(frame[2], marker);
        assert_eq!(frame[5], marker);
        output.extend([frame[1], frame[4], frame[0], frame[3]]);
        marker = if marker == 0x05 { 0xfa } else { 0x05 };
    }
    output
}

#[test]
fn real_producer_typed_packer_file_and_independent_consumer_agree_after_seek() {
    use earph_core::{
        audio::{ChannelLayout, DsdBlock},
        dop::{DopPacker, DopStream},
    };
    use sha2::{Digest, Sha256};
    use std::io::Write;
    let known: Vec<u8> = (0..4100)
        .flat_map(|i| [(i % 251) as u8, (255 - i % 239) as u8])
        .collect();
    for rate in [2_822_400, 5_644_800] {
        for container in [dsf(&known, 1, rate, b"tag"), dff(&known, rate)] {
            let path = fixture(&container);
            let source_hash = Sha256::digest(&container);
            let mut reader =
                DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).unwrap();
            for start in [0, 16, 32752] {
                let epochs = Epochs {
                    session: 1,
                    stream: start / 16,
                };
                reader.seek(start, epochs).unwrap();
                let mut packer = DopPacker::new(
                    DopStream {
                        bit_rate_hz: rate,
                        channels: ChannelLayout::Stereo,
                        epochs,
                        source_sample_position: start,
                    },
                    MemoryBudget::default(),
                )
                .unwrap();
                let output_path = fixture(&[]);
                let mut output = std::fs::File::create(&output_path).unwrap();
                let mut sequence = 0;
                let mut position = start / 16;
                while let Some(frame) = reader.next_block(3).unwrap() {
                    for fragment in frame.block.payload.chunks(3) {
                        let block =
                            DsdBlock::new(fragment.to_vec(), rate, ChannelLayout::Stereo, epochs)
                                .unwrap();
                        if let Some(carrier) = packer.push(&block).unwrap() {
                            assert_eq!(carrier.sequence(), sequence);
                            assert_eq!(carrier.frame_position(), position);
                            assert_eq!(carrier.epochs(), epochs);
                            assert_eq!(carrier.carrier_rate_hz(), rate / 16);
                            output.write_all(carrier.payload()).unwrap();
                            sequence += 1;
                            position += carrier.frame_count();
                        }
                    }
                }
                packer.finish().unwrap();
                output.sync_all().unwrap();
                drop(output);
                let recovered = independently_consume(&output_path);
                let expected = &known[(start / 8 * 2) as usize..];
                assert_eq!(recovered, expected);
                assert_eq!(Sha256::digest(&recovered), Sha256::digest(expected));
                assert_eq!(position, known.len() as u64 / 4);
                // Stop with a partial frame, then seek into a new epoch on the next iteration.
                reader.seek(0, epochs).unwrap();
                let pending = reader.next_block(1).unwrap().unwrap();
                let mut old = DopPacker::new(
                    DopStream {
                        bit_rate_hz: rate,
                        channels: ChannelLayout::Stereo,
                        epochs,
                        source_sample_position: 0,
                    },
                    MemoryBudget::default(),
                )
                .unwrap();
                assert!(old.push(&pending.block).unwrap().is_none());
                old.cancel();
                assert!(old.finish().is_err());
            }
            assert_eq!(Sha256::digest(std::fs::read(path).unwrap()), source_hash);
        }
    }
}

fn dsf(canonical: &[u8], order: u32, rate: u32, tag: &[u8]) -> Vec<u8> {
    let per_channel = canonical.len() / 2;
    let payload = per_channel.div_ceil(4096) * 8192;
    let data_end = 92 + payload;
    let mut out = Vec::new();
    out.extend(b"DSD ");
    out.extend(28u64.to_le_bytes());
    out.extend(((data_end + tag.len()) as u64).to_le_bytes());
    out.extend((if tag.is_empty() { 0 } else { data_end as u64 }).to_le_bytes());
    out.extend(b"fmt ");
    out.extend(52u64.to_le_bytes());
    for value in [1, 0, 2, 2, rate, order] {
        out.extend(value.to_le_bytes());
    }
    out.extend(((per_channel * 8) as u64).to_le_bytes());
    out.extend(4096u32.to_le_bytes());
    out.extend(0u32.to_le_bytes());
    out.extend(b"data");
    out.extend((12 + payload as u64).to_le_bytes());
    for group in 0..per_channel.div_ceil(4096) {
        for channel in 0..2 {
            for byte in 0..4096 {
                let i = group * 4096 + byte;
                let value = if i < per_channel {
                    canonical[2 * i + channel]
                } else {
                    0
                };
                out.push(if order == 1 {
                    value.reverse_bits()
                } else {
                    value
                });
            }
        }
    }
    out.extend(tag);
    out
}

#[test]
fn dsf_lsb_crosses_channel_blocks_without_padding_or_tags() {
    // Reversing no bytes, mixing planes, or exposing padding breaks this known sequence.
    let canonical: Vec<u8> = (0..4098)
        .flat_map(|i| [(i % 251) as u8, (255 - i % 239) as u8])
        .collect();
    let path = fixture(&dsf(&canonical, 1, 2_822_400, b"bounded trailing tag"));
    let mut reader = DsdFile::open(&path, Epochs::default(), MemoryBudget::default()).unwrap();
    let mut actual = Vec::new();
    while let Some(frame) = reader.next_block(4096).unwrap() {
        actual.extend(frame.block.payload);
    }
    assert_eq!(actual, canonical);
}
