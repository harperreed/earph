// ABOUTME: Streams the qualified stereo DSF and uncompressed DFF subset.
// ABOUTME: Keeps source bit positions separate from PCM and carrier frames.
use crate::{BUDGET, INVALID, UNSUPPORTED, admission};
use earph_core::{
    EngineError,
    audio::{ChannelLayout, DsdBlock, MemoryBudget},
    dop::DSD_WORKING_BYTES,
    ids::Epochs,
};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

/// A bounded worker-side reader for stereo DSD64/128. No output API is exposed.
/// Both DSD source bytes and DoP carrier words are excluded from PCM DSP.
/// ```compile_fail
/// use earph_core::audio::DsdBlock;
/// fn wrong(graph: &mut earph_dsp::PreparedGraph, block: &DsdBlock) {
///     graph.process(block, &mut []).unwrap();
/// }
/// ```
/// ```compile_fail
/// use earph_core::dop::DopBlock;
/// fn wrong(graph: &mut earph_dsp::PreparedGraph, block: &DopBlock) {
///     graph.process(block, &mut []).unwrap();
/// }
/// ```
pub struct DsdFile {
    file: Option<File>,
    data_start: u64,
    total_samples: u64,
    rate: u32,
    position: u64,
    sequence: u64,
    epochs: Epochs,
    scratch: [u8; 4096],
    storage: Storage,
}
enum Storage {
    Dsf { lsb: bool },
    Dff,
}
pub struct DecodedDsdFrame {
    pub block: DsdBlock,
    pub sequence: u64,
    pub source_sample_position: u64,
}
impl DsdFile {
    pub fn open(path: &Path, epochs: Epochs, budget: MemoryBudget) -> Result<Self, EngineError> {
        budget.admit(DSD_WORKING_BYTES)?;
        let mut file = admission::open_regular(path)?;
        let len = file.metadata().map_err(|_| INVALID)?.len();
        let magic = read::<4>(&mut file)?;
        file.rewind().map_err(|_| INVALID)?;
        if &magic == b"FRM8" {
            let (data_start, total_samples, rate) = admit_dff(&mut file, len)?;
            return Ok(Self {
                file: Some(file),
                data_start,
                total_samples,
                rate,
                position: 0,
                sequence: 0,
                epochs,
                scratch: [0; 4096],
                storage: Storage::Dff,
            });
        }
        let h = read::<92>(&mut file)?;
        if &h[..4] != b"DSD " {
            return Err(UNSUPPORTED);
        }
        if le64(&h[4..12]) != 28
            || le64(&h[12..20]) != len
            || &h[28..32] != b"fmt "
            || le64(&h[32..40]) != 52
            || le32(&h[40..44]) != 1
            || le32(&h[44..48]) != 0
            || le32(&h[48..52]) != 2
            || le32(&h[52..56]) != 2
            || le32(&h[72..76]) != 4096
            || le32(&h[76..80]) != 0
            || &h[80..84] != b"data"
        {
            return Err(UNSUPPORTED);
        }
        let rate = le32(&h[56..60]);
        let order = le32(&h[60..64]);
        let total_samples = le64(&h[64..72]);
        if !matches!(rate, 2_822_400 | 5_644_800)
            || !matches!(order, 1 | 8)
            || total_samples == 0
            || !total_samples.is_multiple_of(16)
        {
            return Err(UNSUPPORTED);
        }
        let valid = total_samples / 8;
        let groups = valid.checked_add(4095).ok_or(INVALID)? / 4096;
        let payload = groups.checked_mul(8192).ok_or(INVALID)?;
        let end = 92u64.checked_add(payload).ok_or(INVALID)?;
        if le64(&h[84..92]) != payload.checked_add(12).ok_or(INVALID)? || end > len {
            return Err(INVALID);
        }
        let metadata = le64(&h[20..28]);
        if (metadata == 0 && end != len) || (metadata != 0 && (metadata != end || metadata >= len))
        {
            return Err(INVALID);
        }
        if len - end > 65536 {
            return Err(BUDGET);
        }
        let remainder = valid % 4096;
        if remainder != 0 {
            let mut padding = [0; 4096];
            for channel in 0..2 {
                let offset = 92 + (groups - 1) * 8192 + channel * 4096 + remainder;
                file.seek(SeekFrom::Start(offset)).map_err(|_| INVALID)?;
                let count = (4096 - remainder) as usize;
                file.read_exact(&mut padding[..count])
                    .map_err(|_| INVALID)?;
                if padding[..count].iter().any(|v| *v != 0) {
                    return Err(INVALID);
                }
            }
        }
        Ok(Self {
            file: Some(file),
            data_start: 92,
            total_samples,
            rate,
            position: 0,
            sequence: 0,
            epochs,
            scratch: [0; 4096],
            storage: Storage::Dsf { lsb: order == 1 },
        })
    }
    pub fn next_block(
        &mut self,
        max_bytes_per_channel: usize,
    ) -> Result<Option<DecodedDsdFrame>, EngineError> {
        let result = self.read_block(max_bytes_per_channel);
        if result.is_err() {
            self.file = None;
        }
        result
    }
    pub fn total_samples(&self) -> u64 {
        self.total_samples
    }
    pub fn bit_rate_hz(&self) -> u32 {
        self.rate
    }
    /// Seek in one-bit samples per channel. Every failed seek retires the reader.
    pub fn seek(&mut self, position: u64, epochs: Epochs) -> Result<(), EngineError> {
        let result = self.seek_inner(position, epochs);
        if result.is_err() {
            self.file = None;
        }
        result
    }
    fn seek_inner(&mut self, position: u64, epochs: Epochs) -> Result<(), EngineError> {
        let file = self.file.as_mut().ok_or(INVALID)?;
        if !position.is_multiple_of(16) || position > self.total_samples {
            return Err(INVALID);
        }
        if position < self.total_samples {
            let byte = position / 8;
            let offset = match self.storage {
                Storage::Dsf { .. } => self
                    .data_start
                    .checked_add((byte / 4096).checked_mul(8192).ok_or(INVALID)?)
                    .and_then(|n| n.checked_add(byte % 4096)),
                Storage::Dff => byte
                    .checked_mul(2)
                    .and_then(|n| self.data_start.checked_add(n)),
            }
            .ok_or(INVALID)?;
            file.seek(SeekFrom::Start(offset)).map_err(|_| INVALID)?;
        }
        self.scratch.fill(0);
        self.position = position;
        self.epochs = epochs;
        self.sequence = 0;
        Ok(())
    }
    pub fn cancel(&mut self) {
        self.file = None;
    }
    fn read_block(
        &mut self,
        max_bytes_per_channel: usize,
    ) -> Result<Option<DecodedDsdFrame>, EngineError> {
        let file = self.file.as_mut().ok_or(INVALID)?;
        if max_bytes_per_channel == 0 || max_bytes_per_channel > 4096 {
            return Err(BUDGET);
        }
        if self.position == self.total_samples {
            return Ok(None);
        }
        let count =
            ((self.total_samples - self.position) / 8).min(max_bytes_per_channel as u64) as usize;
        let start = self.position / 8;
        let mut payload = vec![0; count * 2];
        match self.storage {
            Storage::Dff => {
                let offset = start
                    .checked_mul(2)
                    .and_then(|n| self.data_start.checked_add(n))
                    .ok_or(INVALID)?;
                file.seek(SeekFrom::Start(offset)).map_err(|_| INVALID)?;
                file.read_exact(&mut payload).map_err(|_| INVALID)?;
            }
            Storage::Dsf { lsb } => {
                for channel in 0..2 {
                    let mut copied = 0;
                    while copied < count {
                        let byte = start + copied as u64;
                        let part = (4096 - (byte % 4096) as usize).min(count - copied);
                        let offset = self
                            .data_start
                            .checked_add((byte / 4096).checked_mul(8192).ok_or(INVALID)?)
                            .and_then(|v| v.checked_add(channel as u64 * 4096 + byte % 4096))
                            .ok_or(INVALID)?;
                        file.seek(SeekFrom::Start(offset)).map_err(|_| INVALID)?;
                        file.read_exact(&mut self.scratch[..part])
                            .map_err(|_| INVALID)?;
                        for (i, value) in self.scratch[..part].iter().enumerate() {
                            payload[(copied + i) * 2 + channel] =
                                if lsb { value.reverse_bits() } else { *value };
                        }
                        copied += part;
                    }
                }
            }
        }
        let frame = DecodedDsdFrame {
            block: DsdBlock::new(payload, self.rate, ChannelLayout::Stereo, self.epochs)?,
            sequence: self.sequence,
            source_sample_position: self.position,
        };
        self.position = self.position.checked_add(count as u64 * 8).ok_or(INVALID)?;
        self.sequence = self.sequence.checked_add(1).ok_or(BUDGET)?;
        Ok(Some(frame))
    }
}

fn read<const N: usize>(file: &mut File) -> Result<[u8; N], EngineError> {
    let mut bytes = [0; N];
    file.read_exact(&mut bytes).map_err(|_| INVALID)?;
    Ok(bytes)
}
fn le32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("fixed field"))
}
fn le64(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("fixed field"))
}

struct Chunk {
    id: [u8; 4],
    size: u64,
    start: u64,
    end: u64,
    next: u64,
}
struct Scan {
    chunks: usize,
    metadata_bytes: u64,
}
impl Scan {
    fn chunk(
        &mut self,
        file: &mut File,
        position: u64,
        parent_end: u64,
        account_bytes: bool,
    ) -> Result<Chunk, EngineError> {
        self.chunks += 1;
        if self.chunks > 256 {
            return Err(BUDGET);
        }
        let start = position
            .checked_add(12)
            .filter(|n| *n <= parent_end)
            .ok_or(INVALID)?;
        file.seek(SeekFrom::Start(position)).map_err(|_| INVALID)?;
        let header = read::<12>(file)?;
        let id = header[..4].try_into().expect("fixed identifier");
        let size = u64::from_be_bytes(header[4..].try_into().expect("fixed size"));
        let end = start
            .checked_add(size)
            .filter(|n| *n <= parent_end)
            .ok_or(INVALID)?;
        let next = end
            .checked_add(size & 1)
            .filter(|n| *n <= parent_end)
            .ok_or(INVALID)?;
        if size & 1 != 0 {
            file.seek(SeekFrom::Start(end)).map_err(|_| INVALID)?;
            if read::<1>(file)?[0] != 0 {
                return Err(INVALID);
            }
        }
        if account_bytes && &id != b"DSD " {
            self.metadata_bytes = self
                .metadata_bytes
                .checked_add(next - position)
                .ok_or(BUDGET)?;
            if self.metadata_bytes > 1024 * 1024 {
                return Err(BUDGET);
            }
        }
        if matches!(&id, b"DST " | b"DSTF" | b"DSTC" | b"DSTI") {
            return Err(UNSUPPORTED);
        }
        Ok(Chunk {
            id,
            size,
            start,
            end,
            next,
        })
    }
}

fn admit_dff(file: &mut File, length: u64) -> Result<(u64, u64, u32), EngineError> {
    let header = read::<16>(file)?;
    let size = u64::from_be_bytes(header[4..12].try_into().expect("fixed size"));
    if &header[..4] != b"FRM8" || &header[12..] != b"DSD " {
        return Err(UNSUPPORTED);
    }
    let end = 12u64.checked_add(size).ok_or(INVALID)?;
    if end != length || size < 4 {
        return Err(INVALID);
    }
    let mut scan = Scan {
        chunks: 1,
        metadata_bytes: 4,
    };
    let version = scan.chunk(file, 16, end, true)?;
    if &version.id != b"FVER" || version.size != 4 {
        return Err(UNSUPPORTED);
    }
    file.seek(SeekFrom::Start(version.start))
        .map_err(|_| INVALID)?;
    let v = read::<4>(file)?;
    if v[0] != 1 || v[1] > 5 || v[2] != 0 || v[3] != 0 {
        return Err(UNSUPPORTED);
    }
    let mut position = version.next;
    let mut rate = None;
    let mut audio = None;
    while position < end {
        let c = scan.chunk(file, position, end, true)?;
        match &c.id {
            b"FVER" => return Err(INVALID),
            b"PROP" => {
                if rate.is_some() || audio.is_some() {
                    return Err(INVALID);
                }
                rate = Some(sound_properties(file, &c, &mut scan)?);
            }
            b"DSD " => {
                if audio.is_some() || rate.is_none() || c.size == 0 || !c.size.is_multiple_of(4) {
                    return Err(INVALID);
                }
                let samples = (c.size / 2).checked_mul(8).ok_or(INVALID)?;
                audio = Some((c.start, samples));
            }
            _ => {}
        }
        position = c.next;
    }
    let (start, samples) = audio.ok_or(INVALID)?;
    Ok((start, samples, rate.ok_or(INVALID)?))
}

fn sound_properties(file: &mut File, parent: &Chunk, scan: &mut Scan) -> Result<u32, EngineError> {
    if parent.size < 4 {
        return Err(INVALID);
    }
    file.seek(SeekFrom::Start(parent.start))
        .map_err(|_| INVALID)?;
    if &read::<4>(file)? != b"SND " {
        return Err(UNSUPPORTED);
    }
    let mut position = parent.start.checked_add(4).ok_or(INVALID)?;
    let mut rate = None;
    let mut channels = false;
    let mut compression = false;
    while position < parent.end {
        // The PROP extent already accounts for these child bytes.
        let c = scan.chunk(file, position, parent.end, false)?;
        file.seek(SeekFrom::Start(c.start)).map_err(|_| INVALID)?;
        match &c.id {
            b"FS  " => {
                if rate.is_some() || c.size != 4 {
                    return Err(INVALID);
                }
                let value = u32::from_be_bytes(read::<4>(file)?);
                if !matches!(value, 2_822_400 | 5_644_800) {
                    return Err(UNSUPPORTED);
                }
                rate = Some(value);
            }
            b"CHNL" => {
                if channels || c.size != 10 {
                    return Err(INVALID);
                }
                let value = read::<10>(file)?;
                if &value != b"\x00\x02SLFTSRGT" {
                    return Err(UNSUPPORTED);
                }
                channels = true;
            }
            b"CMPR" => {
                if compression || c.size < 5 || c.size > 260 {
                    return Err(INVALID);
                }
                let value = read::<5>(file)?;
                if &value[..4] != b"DSD " {
                    return Err(UNSUPPORTED);
                }
                if c.size != 5 + u64::from(value[4]) {
                    return Err(INVALID);
                }
                compression = true;
            }
            _ => {}
        }
        position = c.next;
    }
    if !channels || !compression {
        return Err(INVALID);
    }
    rate.ok_or(INVALID)
}
