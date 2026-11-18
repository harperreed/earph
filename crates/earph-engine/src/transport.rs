// ABOUTME: Frames one bounded command and response on each Unix connection.
// ABOUTME: Uses absolute deadlines so trickled bytes cannot retain the owner forever.
use earph_core::{EngineError, ErrorCode};
use rustix::event::{self, PollFd, PollFlags, Timespec};
use std::{
    io::{Read, Write},
    os::fd::AsFd,
    os::unix::net::UnixStream,
    path::Path,
    time::{Duration, Instant},
};

pub(crate) const MAX_BYTES: usize = 1_048_576;
pub(crate) const DEADLINE: Duration = Duration::from_secs(1);
const IO: EngineError = EngineError::new(
    ErrorCode::DeviceUnavailable,
    "local IPC disconnected or exceeded its deadline",
);
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "IPC frame exceeds byte limit",
);

pub(crate) fn connect(path: &Path) -> Result<UnixStream, EngineError> {
    use rustix::net::{self, AddressFamily, SocketAddrUnix, SocketType};
    #[cfg(target_os = "linux")]
    let socket = net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        net::SocketFlags::NONBLOCK | net::SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|_| IO)?;
    #[cfg(not(target_os = "linux"))]
    let socket = {
        let socket = net::socket(AddressFamily::UNIX, SocketType::STREAM, None).map_err(|_| IO)?;
        rustix::io::fcntl_setfd(&socket, rustix::io::FdFlags::CLOEXEC).map_err(|_| IO)?;
        rustix::io::ioctl_fionbio(&socket, true).map_err(|_| IO)?;
        socket
    };
    let address = SocketAddrUnix::new(path).map_err(|_| IO)?;
    match net::connect(&socket, &address) {
        Ok(()) => {}
        Err(rustix::io::Errno::INPROGRESS) => {
            wait_ready(&socket, PollFlags::OUT, Instant::now() + DEADLINE)?;
            net::sockopt::socket_error(&socket)
                .map_err(|_| IO)?
                .map_err(|_| IO)?;
        }
        // A full Unix backlog may report EAGAIN without initiating a connection.
        Err(_) => return Err(IO),
    }
    let stream = UnixStream::from(socket);
    Ok(stream)
}

fn wait_ready(fd: &impl AsFd, interest: PollFlags, deadline: Instant) -> Result<(), EngineError> {
    loop {
        let remaining = deadline.checked_duration_since(Instant::now()).ok_or(IO)?;
        let timeout = Timespec::try_from(remaining).map_err(|_| IO)?;
        let mut descriptors = [PollFd::new(fd, interest)];
        match event::poll(&mut descriptors, Some(&timeout)) {
            Ok(0) => return Err(IO),
            Ok(_) => return Ok(()),
            Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(IO),
        }
    }
}

pub(crate) fn read_frame(stream: &mut UnixStream) -> Result<Vec<u8>, EngineError> {
    stream.set_nonblocking(true).map_err(|_| IO)?;
    let deadline = Instant::now() + DEADLINE;
    let mut header = [0; 4];
    read_exact(stream, &mut header, deadline)?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > MAX_BYTES {
        return Err(BUDGET);
    }
    let mut bytes = vec![0; length];
    read_exact(stream, &mut bytes, deadline)?;
    Ok(bytes)
}
fn read_exact(
    stream: &mut UnixStream,
    bytes: &mut [u8],
    deadline: Instant,
) -> Result<(), EngineError> {
    let mut offset = 0;
    while offset < bytes.len() {
        if Instant::now() >= deadline {
            return Err(IO);
        }
        match stream.read(&mut bytes[offset..]) {
            Ok(0) => return Err(IO),
            Ok(count) => offset += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                wait_ready(stream, PollFlags::IN, deadline)?
            }
            Err(_) => return Err(IO),
        }
    }
    Ok(())
}

pub(crate) fn write_frame(stream: &mut UnixStream, bytes: &[u8]) -> Result<(), EngineError> {
    if bytes.is_empty() || bytes.len() > MAX_BYTES {
        return Err(BUDGET);
    }
    stream.set_nonblocking(true).map_err(|_| IO)?;
    let deadline = Instant::now() + DEADLINE;
    for part in [&(bytes.len() as u32).to_be_bytes()[..], bytes] {
        let mut offset = 0;
        while offset < part.len() {
            if Instant::now() >= deadline {
                return Err(IO);
            }
            match stream.write(&part[offset..]) {
                Ok(0) => return Err(IO),
                Ok(count) => offset += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    wait_ready(stream, PollFlags::OUT, deadline)?
                }
                Err(_) => return Err(IO),
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_reader_drains_buffered_reply_after_sender_closes() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender
            .write_all(&[0, 0, 0, 4, b'd', b'a', b't', b'a'])
            .unwrap();
        drop(sender);
        assert_eq!(read_frame(&mut receiver).unwrap(), b"data");
    }

    #[test]
    fn frame_reader_accepts_independent_big_endian_wire_bytes() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.write_all(&[0, 0, 0, 3, b'a', b'b', b'c']).unwrap();
        assert_eq!(read_frame(&mut receiver).unwrap(), b"abc");
    }

    #[test]
    fn empty_oversized_and_incomplete_frames_are_typed_failures() {
        for length in [0_u32, 1_048_577] {
            let (mut sender, mut receiver) = UnixStream::pair().unwrap();
            sender.write_all(&length.to_be_bytes()).unwrap();
            assert_eq!(
                read_frame(&mut receiver).unwrap_err().code,
                ErrorCode::ResourceBudgetExceeded
            );
        }
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.write_all(&[0, 0]).unwrap();
        sender.shutdown(std::net::Shutdown::Write).unwrap();
        assert_eq!(
            read_frame(&mut receiver).unwrap_err().code,
            ErrorCode::DeviceUnavailable
        );
    }

    #[test]
    fn maximum_frame_round_trips_without_truncation() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let worker = std::thread::spawn(move || {
            let mut bytes = vec![7; 1_048_576];
            bytes[1_048_575] = 255;
            write_frame(&mut sender, &bytes).unwrap();
        });
        let received = read_frame(&mut receiver).unwrap();
        worker.join().unwrap();
        assert_eq!(received.len(), 1_048_576);
        assert_eq!(received[0], 7);
        assert_eq!(received[1_048_575], 255);
    }
}
