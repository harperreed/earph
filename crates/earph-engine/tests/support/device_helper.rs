// ABOUTME: Provides controlled owned processes for discovery supervisor tests only.
// ABOUTME: Records actual input, environment names, descriptors, and PID without native audio calls.
use std::{
    fs,
    io::{Read, Write},
    time::Duration,
};
fn main() {
    let executable = std::env::current_exe().unwrap();
    let name = executable.file_name().unwrap().to_str().unwrap();
    fs::write(
        executable.with_extension("pid"),
        std::process::id().to_string(),
    )
    .unwrap();
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
    fs::write(executable.with_extension("input"), input).unwrap();
    let mut keys = std::env::vars_os()
        .map(|(key, _)| key.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    keys.sort();
    fs::write(executable.with_extension("environment"), keys.join("\n")).unwrap();
    #[cfg(target_os = "linux")]
    {
        let descriptors = fs::read_dir("/proc/self/fd")
            .unwrap()
            .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        fs::write(
            executable.with_extension("descriptors"),
            descriptors.join("\n"),
        )
        .unwrap();
    }
    if name.contains("crash") {
        std::process::exit(23);
    }
    if name.contains("oversize") {
        let _ = std::io::stdout().write_all(&vec![b'x'; 300 * 1024]);
    } else if name.contains("stderr") {
        let _ = std::io::stderr().write_all(&vec![b'x'; 20 * 1024]);
    } else if name.contains("malformed") {
        let _ = std::io::stdout().write_all(b"wrong frame");
        return;
    } else if !name.contains("wait") {
        let bytes=br#"{"schema_version":1,"result":{"Ok":{"kind":"list","devices":[],"complete":true,"failures":[]}}}"#;
        std::io::stdout()
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        std::io::stdout().write_all(bytes).unwrap();
        if name.contains("trailing") {
            std::io::stdout().write_all(b"extra").unwrap();
        }
        return;
    }
    loop {
        std::thread::sleep(Duration::from_millis(20));
    }
}
