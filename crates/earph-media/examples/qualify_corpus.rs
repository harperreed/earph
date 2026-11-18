// ABOUTME: Checks an optional private corpus without publishing paths or track metadata.
// ABOUTME: Compares canonical PCM hashes and verifies source files remain unchanged.
use earph_core::ids::Epochs;
use earph_media::LocalFile;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufReader, Read},
    path::Path,
};

fn hash_file(path: &Path) -> Result<String, String> {
    let mut file = BufReader::new(File::open(path).map_err(|_| "cannot open corpus file")?);
    let mut digest = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|_| "cannot hash corpus file")?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Ok(hex(digest))
}
fn hex(digest: Sha256) -> String {
    digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn run(root: &Path) -> Result<Value, String> {
    let manifest = File::open(root.join("manifest.json")).map_err(|_| "missing corpus manifest")?;
    if manifest
        .metadata()
        .map_err(|_| "cannot inspect manifest")?
        .len()
        > 2 * 1024 * 1024
    {
        return Err("manifest exceeds budget".into());
    }
    let manifest: Value =
        serde_json::from_reader(manifest).map_err(|_| "invalid corpus manifest")?;
    let mut passed = 0;
    let mut rejected_dop = 0;
    let mut outside_scope = 0;
    let mut failures = 0;
    let mut unsupported_extended = 0;
    for (index, track) in manifest["tracks"]
        .as_array()
        .ok_or("missing track list")?
        .iter()
        .enumerate()
    {
        let relative = Path::new(track["path"].as_str().ok_or("missing corpus path")?);
        if relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err("invalid corpus path".into());
        }
        let path = root.join(relative);
        if track["codec"]
            .as_str()
            .is_some_and(|s| s.starts_with("pcm_s"))
            || track["codec"] == "flac"
        {
            if !matches!(track["channels"].as_u64(), Some(1 | 2))
                || !matches!(track["bits_per_sample"].as_u64(), Some(16 | 24 | 32))
                || !matches!(
                    path.extension().and_then(|s| s.to_str()),
                    Some("wav" | "flac" | "aiff")
                )
            {
                outside_scope += 1;
                continue;
            }
            let original = hash_file(&path)?;
            if original != track["sha256"].as_str().ok_or("missing source hash")? {
                return Err("source hash differs before decoding".into());
            }
            let is_dop = relative
                .components()
                .any(|c| c.as_os_str() == "07-dop-transport-not-pcm");
            let mut signature = [0; 4];
            File::open(&path)
                .map_err(|_| "cannot open corpus file")?
                .read_exact(&mut signature)
                .map_err(|_| "missing file signature")?;
            let is_rf64 = signature == *b"RF64";
            let result = (|| -> Result<(), String> {
                let mut file = LocalFile::open(&path, Epochs::default())
                    .map_err(|e| format!("{:?}", e.code))?;
                if is_dop {
                    return Err("DoP carrier accepted as PCM".into());
                }
                let bits = file.format().representation().bytes() * 8;
                let channels = file.format().channels().count();
                let mut frames = 0;
                let mut digest = Sha256::new();
                while let Some(block) = file.next_block().map_err(|e| format!("{:?}", e.code))? {
                    for i in 0..block.header().frame_count as usize * channels {
                        let value = block.integer_sample(i).ok_or("noninteger sample")?;
                        digest.update(((value << (32 - bits)) as i32).to_le_bytes());
                    }
                    frames += block.header().frame_count;
                }
                if frames
                    != track["decoded_frames"]
                        .as_u64()
                        .ok_or("missing frame count")?
                    || hex(digest)
                        != track["decoded_pcm_sha256"]
                            .as_str()
                            .ok_or("missing PCM hash")?
                {
                    return Err("decoded hash or frames differ".into());
                }
                Ok(())
            })();
            match result {
                Ok(()) => passed += 1,
                Err(reason) if is_dop && reason == "FidelityPolicyConflict" => rejected_dop += 1,
                Err(reason) if is_rf64 && reason == "FormatUnsupported" => {
                    unsupported_extended += 1
                }
                Err(reason) => {
                    failures += 1;
                    eprintln!("corpus case {index} failed: {reason}");
                }
            }
            if hash_file(&path)? != original {
                return Err("source hash differs after decoding".into());
            }
        } else {
            outside_scope += 1;
        }
    }
    Ok(
        json!({"schema_version":1,"status":if failures==0 {"pass"} else {"fail"},"pcm_cases_passed":passed,"dop_cases_rejected":rejected_dop,"outside_qualified_scope":outside_scope,"unsupported_extended_containers":unsupported_extended,"failures":failures,"source_files_unchanged":true,"physical_output_exercised":false}),
    )
}

fn main() {
    let result = std::env::args_os()
        .nth(1)
        .ok_or("provide the private corpus directory".to_owned())
        .and_then(|root| run(Path::new(&root)));
    match result {
        Ok(value) => {
            println!("{value}");
            if value["status"] != "pass" {
                std::process::exit(1);
            }
        }
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    }
}
