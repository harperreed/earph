// ABOUTME: Bounds pipe messages, child lifetime, output volume, and reader ownership.
// ABOUTME: Stops only the fixture child and joins every owned thread on all outcomes.
#![allow(dead_code)]
use serde_json::Value;
use std::{
    io::{BufRead, BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
const LIMIT: usize = 400_000;
pub struct Bridge {
    child: Child,
    input: Option<SyncSender<Vec<u8>>>,
    written: Receiver<bool>,
    output: Option<Receiver<Result<Value, ()>>>,
    threads: Vec<JoinHandle<()>>,
}
impl Bridge {
    pub fn new() -> Self {
        let launcher =
            std::env::var_os("EARPH_SENDSPIN_PEER").expect("set pinned offline Python launcher");
        let mut child = Command::new(launcher)
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/session_peer.py"
            ))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (mut stdin, stdout, stderr) = (
            child.stdin.take().unwrap(),
            child.stdout.take().unwrap(),
            child.stderr.take().unwrap(),
        );
        let (tx, rx) = mpsc::sync_channel::<Vec<u8>>(1);
        let (done_tx, written) = mpsc::sync_channel(1);
        let writer = thread::spawn(move || {
            while let Ok(bytes) = rx.recv() {
                let ok = stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_ok();
                if done_tx.send(ok).is_err() || !ok {
                    break;
                }
            }
        });
        let (out_tx, output) = mpsc::sync_channel(8);
        let reader = thread::spawn(move || {
            let mut input = BufReader::new(stdout);
            let mut total = 0usize;
            loop {
                let mut line = Vec::new();
                let result = (|| {
                    loop {
                        let bytes = input.fill_buf().map_err(|_| ())?;
                        if bytes.is_empty() {
                            return Err(());
                        }
                        let end = bytes.iter().position(|b| *b == b'\n').map(|n| n + 1);
                        let count = end.unwrap_or(bytes.len());
                        if line.len() + count > LIMIT {
                            return Err(());
                        }
                        line.extend_from_slice(&bytes[..count]);
                        input.consume(count);
                        if end.is_some() {
                            break;
                        }
                    }
                    total = total.checked_add(line.len()).ok_or(())?;
                    if total > 8_000_000 {
                        return Err(());
                    }
                    serde_json::from_slice(&line).map_err(|_| ())
                })();
                let failed = result.is_err();
                if out_tx.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let errors = thread::spawn(move || {
            let mut source = stderr;
            let mut buffer = [0; 4096];
            let mut total = 0;
            loop {
                match source.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        total += n;
                        if total > 64_000 {
                            break;
                        }
                    }
                }
            }
        });
        Self {
            child,
            input: Some(tx),
            written,
            output: Some(output),
            threads: vec![writer, reader, errors],
        }
    }
    pub fn send(&self, value: Value) {
        let mut bytes = serde_json::to_vec(&value).unwrap();
        bytes.push(b'\n');
        assert!(bytes.len() <= LIMIT);
        self.input
            .as_ref()
            .unwrap()
            .try_send(bytes)
            .expect("bounded fixture write queue");
        assert!(
            self.written
                .recv_timeout(Duration::from_secs(10))
                .expect("fixture write deadline"),
            "fixture write failed"
        );
    }
    pub fn read(&self) -> Value {
        self.output
            .as_ref()
            .unwrap()
            .recv_timeout(Duration::from_secs(10))
            .expect("fixture response deadline")
            .expect("bounded fixture output or peer exit")
    }
    pub fn finish(&mut self) {
        self.input.take();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                assert!(status.success(), "pinned driver failed");
                break;
            }
            assert!(Instant::now() < deadline, "fixture exit deadline");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Bridge {
    fn drop(&mut self) {
        let _ = self.child.kill();
        self.input.take();
        self.output.take();
        let _ = self.child.wait();
        for thread in self.threads.drain(..) {
            thread.join().expect("fixture thread panicked");
        }
    }
}
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn unhex(text: &str) -> Vec<u8> {
    assert!(text.len().is_multiple_of(2));
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
