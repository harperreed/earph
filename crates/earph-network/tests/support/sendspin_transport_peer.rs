// ABOUTME: Owns the pinned Python WebSocket peer and its bounded retained logs.
// ABOUTME: Reaps the exact fixture child and joins its reader on every exit.
#![allow(dead_code)]
use serde_json::Value;
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::IpAddr,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
pub struct Peer {
    child: Child,
    lines: mpsc::Receiver<Value>,
    reader: Option<JoinHandle<()>>,
}

impl Peer {
    pub fn new(ip: IpAddr, mode: &str, directory: &std::path::Path) -> Self {
        Self::spawn([ip.to_string(), mode.to_owned()], directory, "peer")
    }
    pub fn readback(directory: &std::path::Path) -> Self {
        Self::spawn(
            ["--readback".into(), directory.to_str().unwrap().to_owned()],
            directory,
            "restart",
        )
    }
    fn spawn(arguments: [String; 2], directory: &std::path::Path, name: &str) -> Self {
        let mut child = Command::new(
            std::env::var_os("EARPH_SENDSPIN_PEER").expect("set pinned peer interpreter"),
        )
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/sendspin_peer.py"
        ))
        .args(arguments)
        .arg(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(
            fs::File::create(directory.join(format!("{name}-stderr.log"))).unwrap(),
        ))
        .spawn()
        .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::sync_channel(32);
        let mut log = fs::File::create(directory.join(format!("{name}-stdout.log"))).unwrap();
        let reader = thread::spawn(move || {
            let mut source = BufReader::new(stdout);
            let mut total = 0;
            loop {
                let mut line = Vec::new();
                loop {
                    let available = source.fill_buf().unwrap();
                    if available.is_empty() {
                        return;
                    }
                    let end = available.iter().position(|b| *b == b'\n').map(|i| i + 1);
                    let count = end.unwrap_or(available.len());
                    assert!(line.len() + count <= 32000);
                    line.extend_from_slice(&available[..count]);
                    source.consume(count);
                    if end.is_some() {
                        break;
                    }
                }
                total += line.len();
                assert!(total < 128000);
                log.write_all(&line).unwrap();
                tx.try_send(serde_json::from_slice(&line).unwrap()).unwrap();
            }
        });
        Self {
            child,
            lines,
            reader: Some(reader),
        }
    }
    pub fn read(&self) -> Value {
        self.lines
            .recv_timeout(Duration::from_secs(10))
            .expect("real peer stage deadline")
    }
    pub fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "peer failed; inspect retained stderr");
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        self.reader.take().unwrap().join().unwrap();
    }
}
impl Drop for Peer {
    fn drop(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
