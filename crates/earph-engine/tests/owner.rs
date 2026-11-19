// ABOUTME: Exercises the private owner with real Unix sockets and filesystem state.
// ABOUTME: Verifies single ownership, durable queue recovery, and failed commits.
use earph_core::{ErrorCode, control::*, ids::*};
use earph_engine::{EngineOwner, request};
use serde_json::Value;
mod support;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    thread,
};
use support::runtime;

fn command(id: &str, owner: Option<OwnerId>, revision: u64, payload: Command) -> Vec<u8> {
    serde_json::to_vec(&CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new(id).unwrap(),
        zone_id: ZoneId::new("main").unwrap(),
        owner_id: owner,
        session_id: None,
        expected_revision: Some(revision),
        payload,
    })
    .unwrap()
}
fn query(runtime: &std::path::Path, id: &str) -> Value {
    serde_json::from_slice(&request(runtime, &command(id, None, 0, Command::State)).unwrap())
        .unwrap()
}
fn owner_id(state: &Value) -> OwnerId {
    serde_json::from_value(state["receipt"]["result"]["value"]["owner_id"].clone()).unwrap()
}
fn shutdown(runtime: &std::path::Path, id: OwnerId, revision: u64) {
    let reply: Value = serde_json::from_slice(
        &request(
            runtime,
            &command("shutdown", Some(id), revision, Command::OwnerShutdown),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(reply["receipt"]["status"], "applied");
}

#[test]
fn real_owner_persists_queue_and_restarts_paused_without_output() {
    let runtime = runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    assert_eq!(
        EngineOwner::bind(&runtime).err().unwrap().code,
        ErrorCode::SourceBusy
    );
    let thread = thread::spawn(move || owner.serve().unwrap());
    let state = query(&runtime, "first-state");
    let id = owner_id(&state);
    let append = command(
        "append",
        Some(id.clone()),
        0,
        Command::QueueAppend {
            path: "雪.flac".into(),
        },
    );
    let applied = request(&runtime, &append).unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&applied).unwrap()["receipt"]["status"],
        "applied"
    );
    assert_eq!(request(&runtime, &append).unwrap(), applied);
    shutdown(&runtime, id.clone(), 1);
    thread.join().unwrap();
    let old_files: Vec<_> = fs::read_dir(&runtime)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let thread = thread::spawn(move || owner.serve().unwrap());
    let state = query(&runtime, "restored-state");
    assert_ne!(owner_id(&state), id);
    assert_eq!(state["receipt"]["result"]["value"]["transport"], "paused");
    assert_eq!(state["receipt"]["result"]["value"]["muted"], true);
    assert_eq!(state["receipt"]["result"]["value"]["source"], Value::Null);
    assert_eq!(
        state["receipt"]["result"]["value"].get("playback"),
        Some(&Value::Null)
    );
    let queue: Value = serde_json::from_slice(
        &request(
            &runtime,
            &command(
                "queue",
                None,
                0,
                Command::QueueGet {
                    offset: 0,
                    limit: 64,
                },
            ),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        queue["receipt"]["result"]["value"]["entries"][0]["path"],
        "雪.flac"
    );
    let stale: Value = serde_json::from_slice(&request(&runtime, &append).unwrap()).unwrap();
    assert_eq!(stale["receipt"]["error"]["code"], "STALE_SESSION");
    assert!(old_files.iter().all(|path| path.exists()));
    shutdown(&runtime, owner_id(&state), 2);
    thread.join().unwrap();
    assert_eq!(
        fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
        0o700
    );
    for entry in fs::read_dir(&runtime).unwrap() {
        assert_eq!(
            entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[test]
fn owner_drop_releases_its_lock_while_a_real_child_waits_before_exec() {
    use std::{
        io::{Read, Write},
        os::unix::{net::UnixStream, process::CommandExt},
        process::{Command, Stdio},
        time::Duration,
    };
    let runtime = runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let (mut ready, notification) = UnixStream::pair().unwrap();
    let (mut release, continuation) = UnixStream::pair().unwrap();
    ready
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_earph-engine"));
    command
        .args(["render-graph", "--help"])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: The child uses only fd syscalls before exec; each error stores an OS error inline.
    unsafe {
        command.pre_exec(move || {
            if rustix::io::write(&notification, b"r")? != 1 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            let mut byte = [0];
            if rustix::io::read(&continuation, &mut byte)? != 1 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            Ok(())
        });
    }
    let child = thread::spawn(move || command.status());
    let entered = ready.read_exact(&mut [0]);
    drop(owner);
    let replacement = EngineOwner::bind(&runtime);
    // Release and reap the child before any assertion can unwind the parent's fixture.
    let released = release.write_all(b"x");
    let status = child.join().unwrap().unwrap();
    entered.unwrap();
    released.unwrap();
    assert!(status.success());
    let replacement = replacement.expect("a pre-exec child retained the retired owner's lock");
    assert_eq!(
        EngineOwner::bind(&runtime).err().unwrap().code,
        ErrorCode::SourceBusy,
        "the replacement owner must still hold exclusive ownership"
    );
    drop(replacement);
}

#[test]
fn public_or_symlinked_runtime_is_rejected_without_changing_permissions() {
    let runtime = runtime();
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        EngineOwner::bind(&runtime).err().unwrap().code,
        ErrorCode::AuthDenied
    );
    assert_eq!(
        fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
        0o755
    );
    let symlink = runtime.with_extension("link");
    std::os::unix::fs::symlink(&runtime, &symlink).unwrap();
    assert_eq!(
        EngineOwner::bind(&symlink).err().unwrap().code,
        ErrorCode::AuthDenied
    );
}

#[test]
fn corrupt_checkpoint_is_preserved_and_refuses_startup() {
    let runtime = runtime();
    fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
    let checkpoint = runtime.join("queue.json");
    fs::write(&checkpoint, b"corrupt").unwrap();
    fs::set_permissions(&checkpoint, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        EngineOwner::bind(&runtime).err().unwrap().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(fs::read(checkpoint).unwrap(), b"corrupt");
}

fn socket(runtime: &std::path::Path) -> PathBuf {
    let endpoint: Value =
        serde_json::from_slice(&fs::read(runtime.join("endpoint.json")).unwrap()).unwrap();
    runtime.join(endpoint["socket"].as_str().unwrap())
}
fn raw_reply(stream: &mut std::os::unix::net::UnixStream) -> Value {
    use std::io::Read;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(3)))
        .unwrap();
    let mut header = [0; 4];
    stream.read_exact(&mut header).unwrap();
    let size = u32::from_be_bytes(header) as usize;
    assert!(size <= 1_048_576);
    let mut bytes = vec![0; size];
    stream.read_exact(&mut bytes).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn malformed_and_oversized_frames_leave_owner_available() {
    use std::io::Write;
    let runtime = runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let thread = thread::spawn(move || owner.serve().unwrap());
    let mut oversized = std::os::unix::net::UnixStream::connect(socket(&runtime)).unwrap();
    oversized.write_all(&1_048_577_u32.to_be_bytes()).unwrap();
    assert_eq!(
        raw_reply(&mut oversized)["error"]["code"],
        "RESOURCE_BUDGET_EXCEEDED"
    );
    let mut malformed = std::os::unix::net::UnixStream::connect(socket(&runtime)).unwrap();
    malformed.write_all(&2_u32.to_be_bytes()).unwrap();
    malformed.write_all(b"{}").unwrap();
    assert_eq!(raw_reply(&mut malformed)["error"]["code"], "ASSET_INVALID");
    let state = query(&runtime, "state-after-errors");
    shutdown(&runtime, owner_id(&state), 0);
    thread.join().unwrap();
}

#[test]
fn trickled_bytes_hit_an_absolute_deadline_and_release_owner() {
    use std::{
        io::Write,
        time::{Duration, Instant},
    };
    let runtime = runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let thread = thread::spawn(move || owner.serve().unwrap());
    let mut stalled = std::os::unix::net::UnixStream::connect(socket(&runtime)).unwrap();
    stalled.write_all(&100_u32.to_be_bytes()).unwrap();
    let mut writer = stalled.try_clone().unwrap();
    let start = Instant::now();
    let trickle = thread::spawn(move || {
        for _ in 0..15 {
            if writer.write_all(b" ").is_err() {
                break;
            }
            thread::sleep(Duration::from_millis(150));
        }
    });
    assert_eq!(
        raw_reply(&mut stalled)["error"]["code"],
        "DEVICE_UNAVAILABLE"
    );
    assert!(start.elapsed() < Duration::from_secs(2));
    trickle.join().unwrap();
    let state = query(&runtime, "state-after-timeout");
    shutdown(&runtime, owner_id(&state), 0);
    thread.join().unwrap();
}

#[test]
fn failed_storage_returns_failed_receipt_and_stops_owner() {
    let runtime = runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(runtime.join("queue.json"))
        .unwrap();
    let thread = thread::spawn(move || owner.serve());
    let state = query(&runtime, "state-before-failure");
    let reply: Value = serde_json::from_slice(
        &request(
            &runtime,
            &command(
                "append",
                Some(owner_id(&state)),
                0,
                Command::QueueAppend {
                    path: "first.wav".into(),
                },
            ),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(reply["receipt"]["status"], "failed");
    assert_eq!(reply["receipt"]["revision"], 0);
    assert!(thread.join().unwrap().is_err());
    assert!(runtime.join("queue.json").is_dir());
}

#[test]
fn directory_append_is_sorted_nonrecursive_and_retains_original_bytes() {
    let runtime = runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let music = runtime.join("music");
    fs::DirBuilder::new().mode(0o700).create(&music).unwrap();
    fs::write(music.join("z.FLAC"), b"original-z").unwrap();
    fs::write(music.join("a.wav"), b"original-a").unwrap();
    fs::write(music.join("ignored.txt"), b"text").unwrap();
    fs::create_dir(music.join("nested")).unwrap();
    fs::write(music.join("nested/hidden.wav"), b"nested").unwrap();
    let thread = thread::spawn(move || owner.serve().unwrap());
    let state = query(&runtime, "directory-state");
    let append = command(
        "directory",
        Some(owner_id(&state)),
        0,
        Command::QueueAppendDirectory {
            path: music.to_str().unwrap().into(),
        },
    );
    let reply: Value = serde_json::from_slice(&request(&runtime, &append).unwrap()).unwrap();
    assert_eq!(reply["receipt"]["status"], "applied");
    let queue: Value = serde_json::from_slice(
        &request(
            &runtime,
            &command(
                "queue",
                None,
                0,
                Command::QueueGet {
                    offset: 0,
                    limit: 64,
                },
            ),
        )
        .unwrap(),
    )
    .unwrap();
    let entries = queue["receipt"]["result"]["value"]["entries"]
        .as_array()
        .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["path"], music.join("a.wav").to_str().unwrap());
    assert_eq!(entries[1]["path"], music.join("z.FLAC").to_str().unwrap());
    assert_eq!(fs::read(music.join("a.wav")).unwrap(), b"original-a");
    assert_eq!(fs::read(music.join("z.FLAC")).unwrap(), b"original-z");
    assert_eq!(
        request(&runtime, &append).unwrap(),
        serde_json::to_vec(&reply).unwrap()
    );
    shutdown(&runtime, owner_id(&state), 1);
    thread.join().unwrap();
}

#[test]
fn directory_capacity_and_scan_limits_reject_without_partial_append() {
    for (name, count) in [("wav", 257), ("txt", 4097)] {
        let runtime = runtime();
        let owner = EngineOwner::bind(&runtime).unwrap();
        let music = runtime.join("music");
        fs::DirBuilder::new().mode(0o700).create(&music).unwrap();
        for index in 0..count {
            fs::write(music.join(format!("{index}.{name}")), []).unwrap();
        }
        let thread = thread::spawn(move || owner.serve().unwrap());
        let state = query(&runtime, "directory-state");
        let reply: Value = serde_json::from_slice(
            &request(
                &runtime,
                &command(
                    "directory",
                    Some(owner_id(&state)),
                    0,
                    Command::QueueAppendDirectory {
                        path: music.to_str().unwrap().into(),
                    },
                ),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            reply["receipt"]["error"]["code"],
            "RESOURCE_BUDGET_EXCEEDED"
        );
        assert_eq!(reply["receipt"]["revision"], 0);
        let queue: Value = serde_json::from_slice(
            &request(
                &runtime,
                &command(
                    "queue",
                    None,
                    0,
                    Command::QueueGet {
                        offset: 0,
                        limit: 64,
                    },
                ),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(queue["receipt"]["result"]["value"]["total"], 0);
        shutdown(&runtime, owner_id(&state), 0);
        thread.join().unwrap();
    }
}

#[test]
fn changing_a_runtime_alias_cannot_redirect_a_locked_owners_checkpoint() {
    use std::os::unix::fs::symlink;
    let a = runtime();
    let b = runtime();
    let alias_parent = runtime();
    for dir in [&a, &b] {
        fs::DirBuilder::new().mode(0o700).create(dir).unwrap();
    }
    fs::DirBuilder::new()
        .mode(0o777)
        .create(&alias_parent)
        .unwrap();
    fs::set_permissions(&alias_parent, fs::Permissions::from_mode(0o777)).unwrap();
    symlink(&a, alias_parent.join("alias")).unwrap();
    let unstable = alias_parent.join("alias/run");
    let owner_a = EngineOwner::bind(&unstable).unwrap();
    let thread_a = thread::spawn(move || owner_a.serve().unwrap());
    fs::rename(&alias_parent, alias_parent.with_extension("preserved")).unwrap();
    fs::DirBuilder::new()
        .mode(0o777)
        .create(&alias_parent)
        .unwrap();
    fs::set_permissions(&alias_parent, fs::Permissions::from_mode(0o777)).unwrap();
    symlink(&b, alias_parent.join("alias")).unwrap();
    let owner_b = EngineOwner::bind(&unstable).unwrap();
    let thread_b = thread::spawn(move || owner_b.serve().unwrap());
    let real_a = a.join("run");
    let real_b = b.join("run");
    let id_a = owner_id(&query(&real_a, "find-a"));
    let id_b = owner_id(&query(&real_b, "find-b"));
    let reply: Value = serde_json::from_slice(
        &request(
            &real_a,
            &command(
                "append-a",
                Some(id_a.clone()),
                0,
                Command::QueueAppend {
                    path: "owned-by-a.wav".into(),
                },
            ),
        )
        .unwrap(),
    )
    .unwrap();
    let stayed_in_a = real_a.join("queue.json").exists() && !real_b.join("queue.json").exists();
    shutdown(&real_a, id_a, 1);
    shutdown(&real_b, id_b, 0);
    thread_a.join().unwrap();
    thread_b.join().unwrap();
    assert_eq!(reply["receipt"]["status"], "applied");
    assert!(
        stayed_in_a,
        "a checkpoint escaped its locked canonical runtime"
    );
}

#[test]
fn another_user_cannot_replace_a_runtime_through_a_writable_parent() {
    let parent = runtime();
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o777)).unwrap();
    let child = parent.join("private");
    assert_eq!(
        EngineOwner::bind(&child).err().unwrap().code,
        ErrorCode::AuthDenied
    );
    assert!(!child.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn full_connection_backlog_cannot_block_the_client_forever() {
    use rustix::net::{self, AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
    use std::{sync::mpsc, time::Duration};
    let runtime = runtime();
    fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
    let path = runtime.join("o-saturated.sock");
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    net::listen(&listener, 1).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(
        runtime.join("endpoint.json"),
        br#"{"schema_version":1,"socket":"o-saturated.sock"}"#,
    )
    .unwrap();
    fs::set_permissions(
        runtime.join("endpoint.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let address = SocketAddrUnix::new(path).unwrap();
    let mut connections = Vec::new();
    let mut full = false;
    for _ in 0..16 {
        let stream = net::socket_with(
            AddressFamily::UNIX,
            SocketType::STREAM,
            SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        match net::connect(&stream, &address) {
            Ok(()) => connections.push(stream),
            Err(rustix::io::Errno::AGAIN) => {
                full = true;
                break;
            }
            Err(error) => panic!("unexpected connect result: {error}"),
        }
    }
    assert!(
        full,
        "the native connection backlog must be full before testing the client"
    );
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        sender
            .send(request(
                &runtime,
                &command("state", None, 0, Command::State),
            ))
            .unwrap();
    });
    let result = receiver.recv_timeout(Duration::from_secs(2));
    // Release any blocked native connect before asserting, including on failure.
    drop(listener);
    drop(connections);
    worker.join().unwrap();
    assert!(
        result.is_ok(),
        "client connect must return within the deadline"
    );
    assert!(result.unwrap().is_err());
}

#[test]
fn maximum_escaped_queue_restores_and_returns_bounded_pages() {
    let runtime = runtime();
    fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
    let mut queue = earph_core::queue::LocalQueue::default();
    for _ in 0..256 {
        queue.append("\u{1}".repeat(1024)).unwrap();
    }
    let checkpoint = earph_core::session::SessionCheckpoint {
        local_volume_policy: None,
        configured_profile: None,
        profile_catalog: vec![],
        profile_associations: vec![],
        graph_intent: None,
        volume_policy: Default::default(),
        output: None,
        schema_version: 1,
        revision: 0,
        queue: queue.snapshot(),
    };
    let bytes = serde_json::to_vec(&checkpoint).unwrap();
    assert!(bytes.len() > 1_048_576 && bytes.len() <= 2_097_152);
    let path = runtime.join("queue.json");
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let thread = thread::spawn(move || owner.serve().unwrap());
    let state = query(&runtime, "large-state");
    for offset in [0, 64, 128, 192] {
        let bytes = request(
            &runtime,
            &command(
                &format!("page-{offset}"),
                None,
                0,
                Command::QueueGet { offset, limit: 64 },
            ),
        )
        .unwrap();
        assert!(bytes.len() <= 1_048_576);
        let reply: Value = serde_json::from_slice(&bytes).unwrap();
        let page = &reply["receipt"]["result"]["value"];
        assert_eq!(page["total"], 256);
        assert_eq!(page["offset"], offset);
        assert_eq!(page["entries"].as_array().unwrap().len(), 64);
    }
    shutdown(&runtime, owner_id(&state), 0);
    thread.join().unwrap();
    assert!(EngineOwner::bind(&runtime).is_ok());
}
