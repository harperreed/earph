// ABOUTME: Exercises the actual DSD worker owner against private ALSA file/null routes.
// ABOUTME: Uses independent carrier decoding and grants no public or physical output authority.
use super::*;
use earph_core::{control::*, dop::*, ids::*, queue::LocalQueue, safety::*};
use std::{
    fs,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};
#[allow(dead_code)]
#[path = "../../../earph-media/tests/support/dsd.rs"]
mod containers;

fn isolate(name: &str) -> Option<PathBuf> {
    if std::env::var("EARPH_DSD_WORKER_CHILD").as_deref() == Ok(name) {
        return Some(
            std::env::var_os("EARPH_DSD_WORKER_ARTIFACTS")
                .unwrap()
                .into(),
        );
    }
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/dsd-worker")
        .join(format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&directory).unwrap();
    let config = directory.join("alsa.conf");
    fs::write(&config,format!("pcm.null {{ type null }}\npcm.capture {{ type file slave.pcm null file \"{}\" format raw }}\npcm.full {{ type file slave.pcm null file \"/dev/full\" format raw }}\npcm.capture_a {{ type file slave.pcm null file \"{}\" format raw }}\npcm.capture_b {{ type file slave.pcm null file \"{}\" format raw }}\n",directory.join("capture.dop").display(),directory.join("capture-a.dop").display(),directory.join("capture-b.dop").display())).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env("EARPH_DSD_WORKER_CHILD", name)
        .env("EARPH_DSD_WORKER_ARTIFACTS", &directory)
        .env("ALSA_CONFIG_PATH", config)
        .output()
        .unwrap();
    fs::write(directory.join("stdout.log"), &output.stdout).unwrap();
    fs::write(directory.join("stderr.log"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{} / {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    None
}
fn fixture(paths: &[PathBuf], endpoint: &str) -> (Settings, Vec<QueueEntry>, Arc<Mutex<PlanView>>) {
    let mut queue = LocalQueue::default();
    for path in paths {
        queue.append(path.to_str().unwrap()).unwrap();
    }
    let mut snapshot = earph_core::session::Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("dsd-owner").unwrap(),
        ControlLimits::default(),
    )
    .snapshot();
    snapshot.session_id = Some(SessionId::new("dsd-session").unwrap());
    snapshot.source = Some(SourceKind::LocalFile);
    snapshot.epochs = Epochs {
        session: 1,
        stream: 0,
    };
    snapshot.active_entry_id = Some(queue.entries()[0].id.clone());
    snapshot.muted = true;
    snapshot.software_gain_db = 0.0;
    snapshot.output_consented = true;
    snapshot.output = Some(OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: endpoint.into(),
        device_id: DeviceId::new("capture").unwrap(),
        profile_id: ProfileId::new("profile").unwrap(),
        period_frames: 31,
        buffer_frames: 124,
        volume_mode: VolumeMode::Fixed,
        fidelity: FidelityPolicy::StrictPreservation,
    });
    snapshot.volume_policy.arm_fixed(OutputContext {
        device_id: DeviceId::new("capture").unwrap(),
        profile_id: ProfileId::new("profile").unwrap(),
    });
    let entries = queue.snapshot().entries;
    let view = Arc::new(Mutex::new(PlanView::default()));
    view.lock().unwrap().publish(queue.snapshot());
    (
        Settings {
            snapshot,
            playing: true,
            pcm_processing: false,
        },
        entries,
        view,
    )
}
fn consume(bytes: &[u8]) -> Vec<u8> {
    assert!(bytes.len().is_multiple_of(6));
    let mut payload = Vec::new();
    for (i, frame) in bytes.as_chunks::<6>().0.iter().enumerate() {
        let marker = if i % 2 == 0 { 0x05 } else { 0xfa };
        assert_eq!((frame[2], frame[5]), (marker, marker));
        payload.extend([frame[1], frame[4], frame[0], frame[3]]);
    }
    payload
}
#[test]
fn real_worker_capture_preserves_both_rates_orders_short_periods_and_queue_joins() {
    let Some(directory) = isolate(
        "worker::dsd_tests::real_worker_capture_preserves_both_rates_orders_short_periods_and_queue_joins",
    ) else {
        return;
    };
    let known: Vec<u8> = (0..4102)
        .flat_map(|i| [(i % 251) as u8, (255 - i % 239) as u8])
        .collect();
    for rate in [2_822_400, 5_644_800] {
        for (kind, bytes) in [
            ("lsb", containers::dsf(&known, 1, rate, b"tag")),
            ("msb", containers::dsf(&known, 8, rate, b"")),
            ("dff", containers::dff(&known, rate)),
        ] {
            let source = directory.join(format!("{kind}-{rate}.data"));
            fs::write(&source, bytes).unwrap();
            let (settings, entries, view) = fixture(&[source.clone(), source], "capture");
            let ids: Vec<_> = entries.iter().map(|entry| entry.id.clone()).collect();
            let (events, receiver) = mpsc::sync_channel(1024);
            let mut playback = dsd::DsdPlayback::prepare(
                settings,
                entries,
                Arc::new(AtomicBool::new(false)),
                view,
                Arc::new(Mutex::new(None)),
                &events,
            )
            .unwrap();
            let ready = playback.observe().unwrap();
            assert!(ready.output.is_none());
            assert_eq!(ready.qualification, DsdQualification::Unverified);
            playback.settings.snapshot.muted = false;
            for _ in 0..1000 {
                if playback
                    .step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
                    .unwrap()
                {
                    break;
                }
            }
            let observed = playback.observe().unwrap();
            assert!(observed.output.is_none());
            assert_eq!(observed.source_entry_id, ids[1]);
            assert_eq!(observed.source_position_bits, (known.len() / 2 * 8) as u64);
            assert_eq!(
                observed.last_boundary.as_ref().unwrap().reason,
                DsdCloseReason::SourceSubmitted
            );
            let wire = fs::read(directory.join("capture.dop")).unwrap();
            assert_eq!(
                consume(&wire),
                [known.as_slice(), known.as_slice()].concat()
            );
            assert_eq!(
                wire.len() / 6,
                observed.last_boundary.unwrap().submitted_carrier_frames as usize
            );
            assert!(receiver.try_iter().any(|event| matches!(
                event,
                Event::Item {
                    status: PreparationStatus::DsdReady { .. },
                    ..
                }
            )));
            fs::write(directory.join(format!("{kind}-{rate}.dop")), wire).unwrap();
        }
    }
}
#[test]
fn close_pause_seek_mute_cancel_and_failure_keep_accepted_bits_separate_from_decoding() {
    let Some(directory) = isolate(
        "worker::dsd_tests::close_pause_seek_mute_cancel_and_failure_keep_accepted_bits_separate_from_decoding",
    ) else {
        return;
    };
    let known: Vec<u8> = (0..4098)
        .flat_map(|i| [(i % 251) as u8, (255 - i % 239) as u8])
        .collect();
    let source = directory.join("source.dff");
    fs::write(&source, containers::dff(&known, 2_822_400)).unwrap();
    let (settings, entries, view) = fixture(std::slice::from_ref(&source), "capture");
    let (events, _receiver) = mpsc::sync_channel(1024);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut playback = dsd::DsdPlayback::prepare(
        settings,
        entries,
        cancel.clone(),
        view,
        Arc::new(Mutex::new(None)),
        &events,
    )
    .unwrap();
    playback.settings.snapshot.muted = false;
    playback
        .step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
        .unwrap();
    let accepted = playback.observe().unwrap().source_position_bits;
    assert!(accepted > 0);
    let mut pause = playback.settings.clone();
    pause.playing = false;
    pause.snapshot.epochs.stream += 1;
    playback.reposition(pause, None, &events).unwrap();
    let paused = playback.observe().unwrap();
    assert!(paused.output.is_none());
    assert_eq!(paused.source_position_bits, accepted);
    let first = fs::read(directory.join("capture.dop")).unwrap();
    fs::write(directory.join("before-pause.dop"), &first).unwrap();
    assert_eq!(consume(&first), known[..(accepted / 8 * 2) as usize]);
    let mut seek = playback.settings.clone();
    seek.snapshot.epochs.stream += 1;
    playback.reposition(seek, Some(16), &events).unwrap();
    assert_eq!(fs::read(directory.join("capture.dop")).unwrap(), first);
    playback.settings.playing = true;
    for _ in 0..1000 {
        if playback
            .step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
            .unwrap()
        {
            break;
        }
    }
    assert_eq!(
        consume(&fs::read(directory.join("capture.dop")).unwrap()),
        known[4..]
    );
    let (settings, entries, view) = fixture(&[source], "full");
    let mut failed = dsd::DsdPlayback::prepare(
        settings,
        entries,
        cancel.clone(),
        view,
        Arc::new(Mutex::new(None)),
        &events,
    )
    .unwrap();
    failed.settings.snapshot.muted = false;
    let failure = (0..1000)
        .find_map(|_| {
            failed
                .step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
                .err()
        })
        .expect("/dev/full must fail");
    assert_eq!(failure.code, ErrorCode::OutputFault);
    failed.close(DsdCloseReason::Failed).ok();
    let prefix = failed.observe().unwrap();
    assert!(prefix.output.is_none());
    assert!(prefix.last_boundary.is_some());
    cancel.store(true, Ordering::Release);
    assert!(failed.step(&events).unwrap());
}

#[test]
fn mixed_rate_and_processing_refuse_before_open_while_bad_append_keeps_the_prefix() {
    let Some(directory) = isolate(
        "worker::dsd_tests::mixed_rate_and_processing_refuse_before_open_while_bad_append_keeps_the_prefix",
    ) else {
        return;
    };
    let known: Vec<u8> = (0..1026)
        .flat_map(|i| [(i % 251) as u8, (255 - i % 239) as u8])
        .collect();
    let first = directory.join("first.data");
    fs::write(&first, containers::dsf(&known, 1, 2_822_400, b"")).unwrap();
    let other = directory.join("other.data");
    fs::write(&other, containers::dff(&known, 5_644_800)).unwrap();
    let pcm = directory.join("pcm.data");
    let mut wave = b"RIFF".to_vec();
    wave.extend(40u32.to_le_bytes());
    wave.extend(b"WAVEfmt ");
    wave.extend(16u32.to_le_bytes());
    wave.extend(1u16.to_le_bytes());
    wave.extend(2u16.to_le_bytes());
    wave.extend(48000u32.to_le_bytes());
    wave.extend(192000u32.to_le_bytes());
    wave.extend(4u16.to_le_bytes());
    wave.extend(16u16.to_le_bytes());
    wave.extend(b"data");
    wave.extend(4u32.to_le_bytes());
    wave.extend([0; 4]);
    fs::write(&pcm, wave).unwrap();
    earph_media::LocalFile::open(&pcm, Epochs::default()).unwrap();
    let (events, _receiver) = mpsc::sync_channel(1024);
    for second in [&other, &pcm] {
        let (settings, entries, view) = fixture(&[first.clone(), second.clone()], "capture");
        assert_eq!(
            dsd::DsdPlayback::prepare(
                settings,
                entries,
                Arc::new(AtomicBool::new(false)),
                view,
                Arc::new(Mutex::new(None)),
                &events
            )
            .err()
            .unwrap()
            .code,
            ErrorCode::FormatUnsupported
        );
    }
    for mode in [VolumeMode::Software, VolumeMode::Hardware] {
        let (mut settings, entries, view) = fixture(std::slice::from_ref(&first), "capture");
        settings.snapshot.output.as_mut().unwrap().volume_mode = mode;
        settings.snapshot.output.as_mut().unwrap().fidelity = FidelityPolicy::ExpertOverride;
        assert_eq!(
            dsd::DsdPlayback::prepare(
                settings,
                entries,
                Arc::new(AtomicBool::new(false)),
                view,
                Arc::new(Mutex::new(None)),
                &events
            )
            .err()
            .unwrap()
            .code,
            ErrorCode::FidelityPolicyConflict
        );
    }
    let (mut settings, entries, view) = fixture(std::slice::from_ref(&first), "capture");
    settings.pcm_processing = true;
    assert_eq!(
        dsd::DsdPlayback::prepare(
            settings,
            entries,
            Arc::new(AtomicBool::new(false)),
            view,
            Arc::new(Mutex::new(None)),
            &events
        )
        .err()
        .unwrap()
        .code,
        ErrorCode::FidelityPolicyConflict
    );
    assert!(!directory.join("capture.dop").exists());
    let (settings, entries, view) = fixture(std::slice::from_ref(&first), "capture");
    let mut playback = dsd::DsdPlayback::prepare(
        settings,
        entries,
        Arc::new(AtomicBool::new(false)),
        view.clone(),
        Arc::new(Mutex::new(None)),
        &events,
    )
    .unwrap();
    playback.settings.snapshot.muted = false;
    playback
        .step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
        .unwrap();
    let before = playback.observe().unwrap();
    let mut appended = LocalQueue::default();
    appended.append(first.to_str().unwrap()).unwrap();
    appended.append(other.to_str().unwrap()).unwrap();
    view.lock().unwrap().publish(appended.snapshot());
    assert_eq!(
        playback
            .step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
            .unwrap_err()
            .code,
        ErrorCode::FormatUnsupported
    );
    playback.close(DsdCloseReason::Failed).unwrap();
    let after = playback.observe().unwrap();
    assert_eq!(after.source_position_bits, before.source_position_bits);
    assert_eq!(after.last_boundary.unwrap().submitted_carrier_frames, 31);
    assert_eq!(
        consume(&fs::read(directory.join("capture.dop")).unwrap()),
        known[..124]
    );
}

#[test]
fn independent_streams_mute_and_cancellation_keep_distinct_native_boundaries() {
    let Some(directory) = isolate(
        "worker::dsd_tests::independent_streams_mute_and_cancellation_keep_distinct_native_boundaries",
    ) else {
        return;
    };
    let a: Vec<u8> = (0..258)
        .flat_map(|i| [(i % 251) as u8, (255 - i % 239) as u8])
        .collect();
    let b: Vec<u8> = a.iter().map(|byte| byte ^ 0x5a).collect();
    let path_a = directory.join("a.dsf");
    let path_b = directory.join("b.dff");
    fs::write(&path_a, containers::dsf(&a, 8, 2_822_400, b"")).unwrap();
    fs::write(&path_b, containers::dff(&b, 5_644_800)).unwrap();
    let (events, _receiver) = mpsc::sync_channel(1024);
    let (settings_a, entries_a, view_a) = fixture(&[path_a], "capture_a");
    let (mut settings_b, entries_b, view_b) = fixture(&[path_b], "capture_b");
    settings_b.snapshot.epochs.stream = 7;
    let cancel = Arc::new(AtomicBool::new(false));
    let mut first = dsd::DsdPlayback::prepare(
        settings_a,
        entries_a,
        cancel.clone(),
        view_a,
        Arc::new(Mutex::new(None)),
        &events,
    )
    .unwrap();
    let mut second = dsd::DsdPlayback::prepare(
        settings_b,
        entries_b,
        Arc::new(AtomicBool::new(false)),
        view_b,
        Arc::new(Mutex::new(None)),
        &events,
    )
    .unwrap();
    first.settings.snapshot.muted = false;
    second.settings.snapshot.muted = false;
    first
        .step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
        .unwrap();
    second
        .step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
        .unwrap();
    let mut muted = first.settings.clone();
    muted.snapshot.muted = true;
    muted.snapshot.epochs.stream += 1;
    first.apply_settings(muted, &events).unwrap();
    let boundary = first.observe().unwrap().last_boundary.unwrap();
    assert_eq!(boundary.reason, DsdCloseReason::Mute);
    assert_eq!(boundary.output_epochs.stream, 0);
    assert_eq!(boundary.submitted_carrier_frames, 31);
    let before_mute = fs::read(directory.join("capture-a.dop")).unwrap();
    fs::write(directory.join("before-mute-a.dop"), &before_mute).unwrap();
    assert_eq!(consume(&before_mute), a[..124]);
    first.settings.snapshot.muted = false;
    first
        .step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
        .unwrap();
    cancel.store(true, Ordering::Release);
    assert!(first.step(&events).unwrap());
    let stopped = first.observe().unwrap();
    assert!(stopped.output.is_none());
    let last = stopped.last_boundary.unwrap();
    assert_eq!(last.output_epochs.stream, 1);
    assert_eq!(last.reason, DsdCloseReason::Cancelled);
    assert_eq!(last.submitted_carrier_frames, 31);
    assert_eq!(stopped.source_position_bits, 992);
    assert_eq!(
        consume(&fs::read(directory.join("capture-a.dop")).unwrap()),
        a[124..248]
    );
    for _ in 0..100 {
        if second
            .step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
            .unwrap()
        {
            break;
        }
    }
    assert_eq!(
        consume(&fs::read(directory.join("capture-b.dop")).unwrap()),
        b
    );
    assert_eq!(
        second
            .observe()
            .unwrap()
            .last_boundary
            .unwrap()
            .output_epochs
            .stream,
        7
    );
}

#[test]
fn cancellation_after_read_discards_pending_carrier_before_native_open() {
    let Some(directory) = isolate(
        "worker::dsd_tests::cancellation_after_read_discards_pending_carrier_before_native_open",
    ) else {
        return;
    };
    let source = directory.join("source.dsf");
    fs::write(
        &source,
        containers::dsf(&vec![0x69; 1024], 1, 2_822_400, b""),
    )
    .unwrap();
    let (settings, entries, view) = fixture(&[source], "capture");
    let (events, _receiver) = mpsc::sync_channel(1024);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut playback = dsd::DsdPlayback::prepare(
        settings,
        entries,
        cancel.clone(),
        view.clone(),
        Arc::new(Mutex::new(None)),
        &events,
    )
    .unwrap();
    playback.settings.snapshot.muted = false;
    assert!(!playback.fill_pending(&events).unwrap());
    let decoded = playback.observe().unwrap();
    assert_eq!(decoded.source_position_bits, 0);
    assert_eq!(decoded.decoded_position_bits, 496);
    assert!(decoded.output.is_none());
    cancel.store(true, Ordering::Release);
    assert!(playback.step(&events).unwrap());
    let cancelled = playback.observe().unwrap();
    assert_eq!(cancelled.source_position_bits, 0);
    assert!(cancelled.output.is_none());
    assert!(!directory.join("capture.dop").exists());
    drop(playback);
    let view = view.lock().unwrap();
    assert!(view.active.is_none());
    assert!(view.reserved.is_none());
}

#[test]
fn a_cancelled_dsd_control_still_acknowledges_the_matching_stop_as_closed() {
    let Some(directory) = isolate(
        "worker::dsd_tests::a_cancelled_dsd_control_still_acknowledges_the_matching_stop_as_closed",
    ) else {
        return;
    };
    let source = directory.join("source.dsf");
    fs::write(
        &source,
        containers::dsf(&[0x69, 0x96, 0xa5, 0x5a], 1, 2_822_400, b""),
    )
    .unwrap();
    let (settings, entries, view) = fixture(&[source], "capture");
    let (events, receiver) = mpsc::sync_channel(1024);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut current = dsd::DsdPlayback::prepare(
        settings,
        entries,
        cancel.clone(),
        view.clone(),
        Arc::new(Mutex::new(None)),
        &events,
    )
    .unwrap();
    let mut pause = current.settings.clone();
    pause.playing = false;
    pause.snapshot.epochs.stream += 1;
    cancel.store(true, Ordering::Release);
    let error = current
        .reposition(pause.clone(), None, &events)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::OperationCancelled);
    receiver.try_iter().for_each(drop);
    let mut active = Some(ActiveSource::Dsd(Box::new(current)));
    let mut stopped = None;
    let mut terminal = None;
    action_failure(
        &mut active,
        &mut stopped,
        &mut terminal,
        Some(pause.clone()),
        error,
        FailureContext::default(),
        &events,
    );
    pause.snapshot.epochs.stream += 1;
    let request_id = RequestId::new("stop-cancelled-dsd-control").unwrap();
    pause.snapshot.operation = Some(request_id.clone());
    stop_source(
        StopRequest {
            request_id,
            settings: pause,
        },
        &mut active,
        &mut stopped,
        &mut terminal,
        &view,
        &events,
    )
    .unwrap();
    assert!(receiver.try_iter().any(|event|matches!(event,Event::Stopped {observation:Some(LocalPlaybackObservation::Dsd(observation)),..} if observation.output.is_none() && observation.source_position_bits==0)));
    assert!(active.is_none());
    assert!(terminal.is_none());
    assert!(!directory.join("capture.dop").exists());
}

#[test]
fn stale_settings_do_not_retire_a_replacement_dsd_owner() {
    let Some(directory) =
        isolate("worker::dsd_tests::stale_settings_do_not_retire_a_replacement_dsd_owner")
    else {
        return;
    };
    let source = directory.join("source.dff");
    fs::write(
        &source,
        containers::dff(&[0x69, 0x96, 0xa5, 0x5a], 2_822_400),
    )
    .unwrap();
    let (settings, entries, view) = fixture(&[source], "capture");
    let (events, receiver) = mpsc::sync_channel(1024);
    let mut current = dsd::DsdPlayback::prepare(
        settings,
        entries,
        Arc::new(AtomicBool::new(false)),
        view.clone(),
        Arc::new(Mutex::new(None)),
        &events,
    )
    .unwrap();
    let mut old = current.settings.clone();
    old.snapshot.session_id = Some(SessionId::new("old-source").unwrap());
    let error = current.apply_settings(old.clone(), &events).unwrap_err();
    assert_eq!(error.code, ErrorCode::StaleSession);
    receiver.try_iter().for_each(drop);
    let mut active = Some(ActiveSource::Dsd(Box::new(current)));
    let mut stopped = None;
    let mut terminal = None;
    action_failure(
        &mut active,
        &mut stopped,
        &mut terminal,
        Some(old),
        error,
        FailureContext::default(),
        &events,
    );
    assert!(matches!(active, Some(ActiveSource::Dsd(_))));
    assert!(stopped.is_none());
    assert!(terminal.is_none());
    assert!(receiver.try_recv().is_err());
    assert!(!directory.join("capture.dop").exists());
}

fn completed_worker(directory: &std::path::Path) -> (Worker, Settings, SourceTerminalToken) {
    let source = directory.join("completed.dff");
    fs::write(
        &source,
        containers::dff(&[0x69, 0x96, 0xa5, 0x5a], 2_822_400),
    )
    .unwrap();
    let (mut settings, entries, _view) = fixture(&[source], "capture");
    settings.snapshot.transport = TransportState::Buffering;
    let mut worker = Worker::spawn();
    worker.view.lock().unwrap().entries = entries.clone();
    worker
        .send(Action::Start {
            settings: settings.clone(),
            entries,
            cancel: Arc::new(AtomicBool::new(false)),
        })
        .unwrap();
    loop {
        match worker.events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Ready {
                observation: LocalPlaybackObservation::Dsd(_),
                ..
            } => break,
            Event::Failed { error, .. } => panic!("DSD preparation failed: {error:?}"),
            _ => {}
        }
    }
    settings.snapshot.epochs.stream += 1;
    worker
        .send(Action::SeekDsd {
            settings: settings.clone(),
            position_bits: 16,
        })
        .unwrap();
    let token = loop {
        match worker.events.recv_timeout(Duration::from_secs(5)).unwrap() {
            Event::Completed {
                token,
                observation: LocalPlaybackObservation::Dsd(observation),
                ..
            } => {
                assert_eq!(observation.source_position_bits, 16);
                break token;
            }
            Event::Failed { error, .. } => panic!("DSD completion failed: {error:?}"),
            _ => {}
        }
    };
    (worker, settings, token)
}

#[test]
fn a_late_bit_seek_preserves_the_retained_source_completion_token() {
    let Some(directory) = isolate(
        "worker::dsd_tests::a_late_bit_seek_preserves_the_retained_source_completion_token",
    ) else {
        return;
    };
    let (mut worker, mut settings, token) = completed_worker(&directory);
    settings.snapshot.epochs.stream += 1;
    worker
        .send(Action::SeekDsd {
            settings: settings.clone(),
            position_bits: 0,
        })
        .unwrap();
    match worker.events.recv_timeout(Duration::from_secs(5)).unwrap() {
        Event::Completed {
            epochs,
            token: actual,
            observation: LocalPlaybackObservation::Dsd(observation),
        } => {
            assert_eq!(epochs, settings.snapshot.epochs);
            assert_eq!(actual, token);
            assert_eq!(observation.source_position_bits, 16);
            assert_eq!(
                observation.completion_basis,
                Some(DsdCompletionBasis::SourceSubmitted)
            );
        }
        _ => panic!("late DSD seek lost the proven closed source completion"),
    }
    assert!(!directory.join("capture.dop").exists());
}

#[test]
fn stop_after_dsd_eof_retains_the_closed_prefix_until_the_exact_token_is_acknowledged() {
    let Some(directory) = isolate(
        "worker::dsd_tests::stop_after_dsd_eof_retains_the_closed_prefix_until_the_exact_token_is_acknowledged",
    ) else {
        return;
    };
    let (mut worker, mut settings, token) = completed_worker(&directory);
    let mut wrong = token.clone();
    wrong.origin_epochs.stream += 1;
    worker
        .send(Action::AcknowledgeSourceTerminal(wrong))
        .unwrap();
    settings.snapshot.epochs.stream += 1;
    settings.snapshot.transport = TransportState::Paused;
    settings.playing = false;
    let request_id = RequestId::new("stop-completed-dsd").unwrap();
    settings.snapshot.operation = Some(request_id.clone());
    worker
        .send(Action::Stop(StopRequest {
            request_id,
            settings: settings.clone(),
        }))
        .unwrap();
    match worker.events.recv_timeout(Duration::from_secs(5)).unwrap() {
        Event::Completed {
            epochs,
            token: actual,
            observation: LocalPlaybackObservation::Dsd(observation),
        } => {
            assert_eq!(epochs, settings.snapshot.epochs);
            assert_eq!(actual, token);
            assert_eq!(observation.source_position_bits, 16);
            assert!(observation.output.is_none());
        }
        _ => panic!("Stop lost the retained DSD closed prefix"),
    }
    worker
        .send(Action::AcknowledgeSourceTerminal(token))
        .unwrap();
    settings.snapshot.epochs.stream += 1;
    worker.send(Action::Settings(settings)).unwrap();
    assert!(matches!(
        worker.events.recv_timeout(Duration::from_millis(30)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));
    assert!(!directory.join("capture.dop").exists());
}

#[test]
fn scripted_takeover_close_failure_keeps_the_old_dsd_prefix_and_source_identity() {
    let Some(directory) = isolate(
        "worker::dsd_tests::scripted_takeover_close_failure_keeps_the_old_dsd_prefix_and_source_identity",
    ) else {
        return;
    };
    let known: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
    let old_path = directory.join("old.dff");
    let replacement_path = directory.join("replacement.dff");
    fs::write(&old_path, containers::dff(&known, 2_822_400)).unwrap();
    fs::write(
        &replacement_path,
        containers::dff(&[0x69, 0x96, 0xa5, 0x5a], 2_822_400),
    )
    .unwrap();
    let (settings, entries, view) = fixture(&[old_path, replacement_path], "capture");
    let old_entry = entries[0].id.clone();
    let replacement_entry = entries[1].id.clone();
    let (events, receiver) = mpsc::sync_channel(1024);
    let mut old = dsd::DsdPlayback::prepare(
        settings,
        entries.clone(),
        Arc::new(AtomicBool::new(false)),
        view.clone(),
        Arc::new(Mutex::new(None)),
        &events,
    )
    .unwrap();
    old.settings.snapshot.muted = false;
    old.settings.snapshot.transport = TransportState::Playing;
    old.step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
        .unwrap();
    let original = old.settings.clone();
    let mut replacement = original.clone();
    replacement.snapshot.session_id = Some(SessionId::new("replacement-dsd-session").unwrap());
    replacement.snapshot.epochs.session += 1;
    replacement.snapshot.device_epoch += 1;
    replacement.snapshot.active_entry_id = Some(replacement_entry);
    replacement.snapshot.muted = true;
    replacement.snapshot.transport = TransportState::Preparing;
    let mut playback = Some(ActiveSource::Dsd(Box::new(old)));
    let mut terminal = None;
    let mut retirement = DsdRetirement::default();
    let failure = EngineError::new(ErrorCode::OutputFault, "scripted takeover close failure");
    receiver.try_iter().for_each(drop);

    // File/null supplies real accepted bytes. Only the close result is scripted here.
    // This ownership test establishes no native close-error or physical-output behavior.
    let result = retire_for_start(&mut playback, &mut retirement, &events, |current| {
        let ActiveSource::Dsd(current) = current else {
            panic!("expected the retiring DSD owner");
        };
        current.close(DsdCloseReason::Failed)?;
        Err(failure)
    });
    assert_eq!(result, Err(failure));
    retain_source_failure(
        &mut terminal,
        &replacement,
        result.unwrap_err(),
        None,
        &events,
    );
    assert!(playback.is_none());
    let retained = match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
        Event::DsdRetirementFailed(failure) => failure,
        _ => panic!("takeover discarded the old DSD accepted prefix and failed-close boundary"),
    };
    retained.validate().unwrap();
    let token = retained.token.clone();
    let observation = &retained.observation;
    assert_eq!(retained.error, failure);
    assert_eq!(
        token.session_id,
        *original.snapshot.session_id.as_ref().unwrap()
    );
    assert_eq!(token.origin_epochs, original.snapshot.epochs);
    assert_eq!(token.device_epoch, original.snapshot.device_epoch);
    assert_eq!(token.final_entry_id, old_entry);
    assert!(!token.matches_source(&replacement.snapshot));
    assert_eq!(observation.source_entry_id, old_entry);
    assert_eq!(observation.source_epochs, original.snapshot.epochs);
    assert_eq!(observation.source_position_bits, 496);
    assert!(observation.output.is_none());
    let boundary = observation.last_boundary.as_ref().unwrap();
    assert_eq!(boundary.output_epochs, original.snapshot.epochs);
    assert_eq!(boundary.source_entry_id, old_entry);
    assert_eq!(boundary.source_position_bits, 496);
    assert_eq!(boundary.submitted_carrier_frames, 31);
    assert_eq!(boundary.reason, DsdCloseReason::Failed);
    let captured = fs::read(directory.join("capture.dop")).unwrap();
    assert_eq!(consume(&captured), known[..124]);

    let current_token = match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
        Event::Failed {
            epochs,
            token: Some(token),
            error,
            observation: None,
        } => {
            assert_eq!(epochs, replacement.snapshot.epochs);
            assert_eq!(error, failure);
            assert!(token.matches_source(&replacement.snapshot));
            assert_ne!(token.session_id, retained.token.session_id);
            token
        }
        _ => panic!("replacement Start must fail without claiming the old native output"),
    };
    assert_eq!(terminal.as_ref().unwrap().token, current_token);
    let mut wrong = token.clone();
    wrong.origin_epochs.stream += 1;
    acknowledge_source_terminal(&mut terminal, &wrong);
    assert!(retirement.failure.is_some());
    assert!(terminal.is_some());
    replacement.snapshot.epochs.stream += 1;
    let failed_stop_id = RequestId::new("stop-before-replacement-ack").unwrap();
    replacement.snapshot.operation = Some(failed_stop_id.clone());
    let mut stopped = None;
    stop_source(
        StopRequest {
            request_id: failed_stop_id.clone(),
            settings: replacement.clone(),
        },
        &mut playback,
        &mut stopped,
        &mut terminal,
        &view,
        &events,
    )
    .unwrap();
    match receiver.recv_timeout(Duration::from_secs(5)).unwrap() {
        Event::StopFailed {
            request,
            token,
            error,
            observation: None,
        } => {
            assert_eq!(request.request_id, failed_stop_id);
            assert!(request.matches_snapshot(&replacement.snapshot));
            assert_eq!(token, current_token);
            assert_eq!(error, failure);
        }
        _ => panic!("current Stop must retain its own failure without the old DSD observation"),
    }
    assert_eq!(retirement.failure.as_ref().unwrap().token, token);
    acknowledge_source_terminal(&mut terminal, &current_token);
    assert!(terminal.is_none());
    assert!(retirement.failure.is_some());

    replacement.snapshot.epochs.stream += 1;
    let request_id = RequestId::new("stop-failed-replacement").unwrap();
    replacement.snapshot.operation = Some(request_id.clone());
    stop_source(
        StopRequest {
            request_id,
            settings: replacement.clone(),
        },
        &mut playback,
        &mut stopped,
        &mut terminal,
        &view,
        &events,
    )
    .unwrap();
    assert!(matches!(
        receiver.recv_timeout(Duration::from_secs(5)).unwrap(),
        Event::Stopped {
            epochs,
            observation: None,
        } if epochs == replacement.snapshot.epochs
    ));
    assert!(terminal.is_none());
    assert_eq!(retirement.failure.as_ref().unwrap().token, token);
    assert_eq!(
        retire_for_start(&mut playback, &mut retirement, &events, |_| {
            panic!("an unacknowledged retirement cannot admit another source")
        }),
        Err(DSD_RETIREMENT_PENDING)
    );

    // A full event queue retains the same immutable record and permits exact redelivery.
    let (bounded, drain) = mpsc::sync_channel(1);
    bounded
        .send(Event::End {
            epochs: Epochs::default(),
        })
        .unwrap();
    let mut blocked = DsdRetirement {
        failure: Some(retained.clone()),
        delivered: false,
    };
    blocked.report(&bounded);
    blocked.acknowledge(&token);
    assert!(!blocked.delivered);
    assert!(blocked.failure.is_some());
    drop(drain.recv().unwrap());
    for _ in 0..2 {
        blocked.report(&bounded);
        match drain.recv().unwrap() {
            Event::DsdRetirementFailed(actual) => assert!(Arc::ptr_eq(&actual, &retained)),
            _ => panic!("retirement redelivery changed its identity"),
        }
        blocked.acknowledge(&wrong);
        assert!(blocked.failure.is_some());
    }
    crate::host::tests::assert_dsd_retirement_evidence(retained, &directory);
    retirement.acknowledge(&token);
    assert!(retirement.failure.is_none());
    assert!(
        retire_for_start(&mut playback, &mut retirement, &events, |_| {
            panic!("there is no remaining native owner")
        })
        .is_ok()
    );
    blocked.acknowledge(&token);
    assert!(blocked.failure.is_none());
    assert_eq!(fs::read(directory.join("capture.dop")).unwrap(), captured);
}

#[test]
fn cancelled_dsd_close_failure_survives_before_the_queued_start_is_read() {
    let Some(directory) = isolate(
        "worker::dsd_tests::cancelled_dsd_close_failure_survives_before_the_queued_start_is_read",
    ) else {
        return;
    };
    let known: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
    let source = directory.join("cancelled-before-start.dff");
    fs::write(&source, containers::dff(&known, 2_822_400)).unwrap();
    let (settings, entries, view) = fixture(&[source], "capture");
    let (events, receiver) = mpsc::sync_channel(1024);
    let cancel = Arc::new(AtomicBool::new(false));
    let mut old = dsd::DsdPlayback::prepare(
        settings,
        entries,
        cancel.clone(),
        view,
        Arc::new(Mutex::new(None)),
        &events,
    )
    .unwrap();
    old.settings.snapshot.muted = false;
    old.step_authorized(&events, &dsd::SubmissionAuthority::SoftwareCapture)
        .unwrap();
    let original = old.settings.clone();
    receiver.try_iter().for_each(drop);

    // Start admission publishes cancellation before its action is read by the worker.
    // Actual file/null bytes precede this explicit scripted close failure.
    let mut replacement = original.clone();
    replacement.snapshot.epochs.session += 1;
    replacement.snapshot.session_id = Some(SessionId::new("after-cancel-session").unwrap());
    let (mut admission, queued) = super::tests::admission();
    admission.cancel = Some(cancel.clone());
    admission
        .send(Action::Start {
            settings: replacement.clone(),
            entries: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
        })
        .unwrap();
    assert!(cancel.load(Ordering::Acquire));
    old.close(DsdCloseReason::Failed).unwrap();
    let first_close_error = EngineError::new(
        ErrorCode::OutputFault,
        "scripted first cancellation close failure",
    );
    let mut playback = Some(ActiveSource::Dsd(Box::new(old)));
    let mut terminal = None;
    let mut stopped = None;
    let mut retirement = DsdRetirement::default();
    action_failure(
        &mut playback,
        &mut stopped,
        &mut terminal,
        None,
        first_close_error,
        FailureContext {
            retirement: Some(&mut retirement),
            ..FailureContext::default()
        },
        &events,
    );
    assert!(playback.is_none());

    // Current terminal adoption cannot acknowledge historical evidence, even with equal tokens.
    let current_token = terminal.as_ref().unwrap().token.clone();
    assert_eq!(retirement.failure.as_ref().unwrap().token, current_token);
    acknowledge_source_terminal(&mut terminal, &current_token);
    assert!(terminal.is_none());
    assert!(
        retirement.failure.is_some(),
        "current source acknowledgement erased unadopted retirement evidence"
    );

    // The queued Start clears ordinary terminal and stopped state before taking ownership.
    let Action::Start {
        settings: queued_settings,
        ..
    } = queued.recv().unwrap()
    else {
        panic!("expected the real admitted replacement Start");
    };
    assert_eq!(queued_settings.snapshot.epochs, replacement.snapshot.epochs);
    terminal = None;
    stopped = None;
    assert_eq!(
        retire_for_start(&mut playback, &mut retirement, &events, |_| {
            panic!("the cancelled native owner has already retired")
        }),
        Err(DSD_RETIREMENT_PENDING),
        "queued Start erased the failed cancellation boundary before host adoption"
    );
    assert!(stopped.is_none());
    assert!(terminal.is_none());
    let retained = retirement.failure.as_ref().unwrap().clone();
    assert_eq!(retained.error, first_close_error);
    assert_eq!(
        retained.token.session_id,
        *original.snapshot.session_id.as_ref().unwrap()
    );
    assert_eq!(retained.token.origin_epochs, original.snapshot.epochs);
    assert!(!retained.token.matches_source(&replacement.snapshot));
    assert_eq!(retained.observation.source_position_bits, 496);
    assert_eq!(
        retained.observation.last_boundary.as_ref().unwrap().reason,
        DsdCloseReason::Failed
    );
    assert_eq!(
        retained
            .observation
            .last_boundary
            .as_ref()
            .unwrap()
            .submitted_carrier_frames,
        31
    );
    assert!(receiver.try_iter().any(|event| matches!(event, Event::DsdRetirementFailed(actual) if Arc::ptr_eq(&actual, &retained))));
    let captured = fs::read(directory.join("capture.dop")).unwrap();
    assert_eq!(consume(&captured), known[..124]);
    let mut wrong = retained.token.clone();
    wrong.origin_epochs.stream += 1;
    retirement.acknowledge(&wrong);
    assert!(retirement.failure.is_some());
    crate::host::tests::assert_dsd_retirement_evidence(retained.clone(), &directory);
    retirement.acknowledge(&retained.token);
    assert!(retirement.failure.is_none());
    assert_eq!(fs::read(directory.join("capture.dop")).unwrap(), captured);
}
