// ABOUTME: Admits regular local files with bounded container metadata.
// ABOUTME: Checks sizes before codec libraries allocate embedded tags or artwork.
use crate::{BUDGET, INVALID, UNSUPPORTED};
use earph_core::EngineError;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
};

pub fn open(path: &Path) -> Result<File, EngineError> {
    let mut file = open_regular(path)?;
    let length = file.metadata().map_err(|_| INVALID)?.len();
    preflight(&mut file, length)?;
    file.rewind().map_err(|_| INVALID)?;
    Ok(file)
}

/// Classifies only container magic. The selected reader must admit its own reopened file.
pub fn is_dsd(path: &Path) -> Result<bool, EngineError> {
    let mut file = open_regular(path)?;
    let mut magic = [0; 4];
    file.read_exact(&mut magic).map_err(|_| INVALID)?;
    Ok(matches!(&magic, b"DSD " | b"FRM8"))
}

pub(crate) fn open_regular(path: &Path) -> Result<File, EngineError> {
    if !std::fs::metadata(path).map_err(|_| INVALID)?.is_file() {
        return Err(INVALID);
    }
    let fd = rustix::fs::open(
        path,
        rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NONBLOCK | rustix::fs::OFlags::CLOEXEC,
        rustix::fs::Mode::empty(),
    )
    .map_err(|_| INVALID)?;
    let file = File::from(fd);
    let metadata = file.metadata().map_err(|_| INVALID)?;
    if !metadata.is_file() {
        return Err(INVALID);
    }
    Ok(file)
}

fn preflight(file: &mut File, file_len: u64) -> Result<(), EngineError> {
    let mut magic = [0; 4];
    file.read_exact(&mut magic).map_err(|_| INVALID)?;
    if magic == *b"fLaC" {
        let mut position = 4_u64;
        for _ in 0..256 {
            let mut header = [0; 4];
            file.read_exact(&mut header).map_err(|_| INVALID)?;
            let size = u32::from_be_bytes([0, header[1], header[2], header[3]]) as u64;
            position = position.checked_add(4 + size).ok_or(BUDGET)?;
            if position > file_len {
                return Err(INVALID);
            }
            if position > 1024 * 1024 {
                return Err(BUDGET);
            }
            file.seek(SeekFrom::Start(position)).map_err(|_| INVALID)?;
            if header[0] & 128 != 0 {
                return Ok(());
            }
        }
        return Err(BUDGET);
    }
    let big_endian = match &magic {
        b"RIFF" => false,
        b"FORM" => true,
        _ => return Err(UNSUPPORTED),
    };
    let mut form = [0; 8];
    file.read_exact(&mut form).map_err(|_| INVALID)?;
    if (!big_endian && &form[4..] != b"WAVE")
        || (big_endian && &form[4..] != b"AIFF" && &form[4..] != b"AIFC")
    {
        return Err(UNSUPPORTED);
    }
    let length = if big_endian {
        u32::from_be_bytes(form[..4].try_into().map_err(|_| INVALID)?)
    } else {
        u32::from_le_bytes(form[..4].try_into().map_err(|_| INVALID)?)
    };
    let container_end = u64::from(length).checked_add(8).ok_or(INVALID)?;
    if container_end > file_len {
        return Err(INVALID);
    }
    let mut position = 12_u64;
    for _ in 0..256 {
        let mut header = [0; 8];
        file.read_exact(&mut header).map_err(|_| INVALID)?;
        let size = if big_endian {
            u32::from_be_bytes(header[4..].try_into().map_err(|_| INVALID)?)
        } else {
            u32::from_le_bytes(header[4..].try_into().map_err(|_| INVALID)?)
        } as u64;
        let end = position.checked_add(8 + size).ok_or(INVALID)?;
        if end > file_len || end > container_end {
            return Err(INVALID);
        }
        if (!big_endian && &header[..4] == b"data") || (big_endian && &header[..4] == b"SSND") {
            return Ok(());
        }
        position = end.checked_add(size % 2).ok_or(INVALID)?;
        if position > 1024 * 1024 {
            return Err(BUDGET);
        }
        file.seek(SeekFrom::Start(position)).map_err(|_| INVALID)?;
    }
    Err(BUDGET)
}
