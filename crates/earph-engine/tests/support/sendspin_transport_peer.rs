// ABOUTME: Owns the pinned authenticated Sendspin peer for bridge integration tests.
// ABOUTME: Bounds retained output and reaps only the fixture child it creates.
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    net::IpAddr,
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
pub struct Peer {
    child: Child,
    lines: mpsc::Receiver<serde_json::Value>,
    reader: Option<JoinHandle<()>>,
}
impl Peer {
    pub fn new(root: &Path, ip: IpAddr) -> Self {
        let mut child = Command::new(
            std::env::var_os("EARPH_SENDSPIN_PEER").expect("explicit pinned peer launcher"),
        )
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../earph-network/tests/support/sendspin_peer.py"
        ))
        .arg(ip.to_string())
        .arg("commit_close")
        .arg(root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(fs::File::create(root.join("sendspin-peer-stderr.log")).unwrap())
        .spawn()
        .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::sync_channel(16);
        let mut log = fs::File::create(root.join("sendspin-peer-stdout.log")).unwrap();
        let reader = thread::spawn(move || {
            let mut source = BufReader::new(stdout);
            let mut total = 0;
            loop {
                let mut line = vec![];
                loop {
                    let chunk = source.fill_buf().unwrap();
                    if chunk.is_empty() {
                        return;
                    }
                    let end = chunk.iter().position(|b| *b == b'\n').map(|n| n + 1);
                    let n = end.unwrap_or(chunk.len());
                    assert!(line.len() + n <= 32000);
                    line.extend_from_slice(&chunk[..n]);
                    source.consume(n);
                    if end.is_some() {
                        break;
                    }
                }
                total += line.len();
                assert!(total <= 128000);
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
    pub fn read(&self) -> serde_json::Value {
        self.lines
            .recv_timeout(Duration::from_secs(12))
            .expect("pinned peer stage deadline")
    }
    pub fn finish(&mut self) {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "inspect retained peer logs");
                break;
            }
            assert!(Instant::now() < end);
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
        if let Some(t) = self.reader.take() {
            let _ = t.join();
        }
    }
}
