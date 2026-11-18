// ABOUTME: Keeps bounded socket parsing outside the playback control actor.
// ABOUTME: Uses one request slot and bounded deadlines without blocking output progress.
use crate::transport;
use earph_core::{
    EngineError, ErrorCode,
    control::{CommandEnvelope, ControlLimits, decode_command},
};
use std::{
    os::unix::net::UnixListener,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(crate) struct Pending {
    pub command: CommandEnvelope,
    pub reply: SyncSender<Vec<u8>>,
}
pub(crate) struct Server {
    pub requests: Receiver<Pending>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}
impl Server {
    pub fn spawn(listener: UnixListener) -> Result<Self, EngineError> {
        listener.set_nonblocking(true).map_err(|_| {
            EngineError::new(
                ErrorCode::InternalFault,
                "cannot prepare local IPC listener",
            )
        })?;
        let (sender, requests) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let join = thread::spawn(move || {
            while !stopping.load(Ordering::Acquire) {
                let (mut stream, _) = match listener.accept() {
                    Ok(value) => value,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                let result = transport::read_frame(&mut stream)
                    .and_then(|bytes| decode_command(&bytes, &ControlLimits::default()));
                let bytes = match result {
                    Ok(command) => {
                        let (reply, receive) = mpsc::sync_channel(1);
                        if sender.try_send(Pending { command, reply }).is_err() {
                            error(EngineError::new(
                                ErrorCode::SourceBusy,
                                "local control queue is full",
                            ))
                        } else {
                            receive.recv_timeout(Duration::from_secs(1)).unwrap_or_else(|_| error(EngineError::new(ErrorCode::DeviceUnavailable, "control result is uncertain; reconcile through fresh state")))
                        }
                    }
                    Err(value) => error(value),
                };
                let _ = transport::write_frame(&mut stream, &bytes);
            }
        });
        Ok(Self {
            requests,
            stop,
            join: Some(join),
        })
    }
}
fn error(error: EngineError) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"error":error})).unwrap_or_default()
}
impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
