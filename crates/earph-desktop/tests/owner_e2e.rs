// ABOUTME: Exercises the desktop model against a real standalone owner process.
// ABOUTME: Uses generated files and an explicit isolated software sink without physical playback.
mod common;
use common::*;
use earph_core::{control::Command, ids::RequestId};
use earph_desktop::{DesktopClient, OwnerClient, Presentation, Reply, Request, ResultView};
use serde_json::json;
use std::{
    fs::{self, File},
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command as ProcessCommand},
    thread,
    time::{Duration, Instant},
};

struct ProcessOwner {
    child: Child,
    runtime: PathBuf,
    artifacts: PathBuf,
}
impl ProcessOwner {
    fn start() -> Self {
        Self::with_configuration(None)
    }
    fn with_configuration(configuration: Option<&std::path::Path>) -> Self {
        let executable = std::env::current_exe().unwrap();
        let binary = executable
            .parent()
            .and_then(|dependencies| dependencies.parent())
            .expect("test executable must reside in the Cargo profile directory")
            .join("earph-engine");
        assert!(
            binary.is_absolute() && binary.is_file(),
            "build earph-engine before running the desktop owner E2E"
        );
        let artifacts = PathBuf::from("/tmp").join(unique());
        fs::create_dir(&artifacts).unwrap();
        fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o700)).unwrap();
        let runtime = artifacts.join("owner");
        let mut command = ProcessCommand::new(&binary);
        command
            .args(["owner", "--runtime"])
            .arg(&runtime)
            .stdout(File::create(artifacts.join("owner.stdout.log")).unwrap())
            .stderr(File::create(artifacts.join("owner.stderr.log")).unwrap());
        if let Some(configuration) = configuration {
            command.env("ALSA_CONFIG_PATH", configuration);
        }
        let child = command.spawn().unwrap();
        let mut owner = Self {
            child,
            runtime,
            artifacts,
        };
        let deadline = Instant::now() + Duration::from_secs(8);
        while !fs::read(owner.artifacts.join("owner.stdout.log")).is_ok_and(|bytes| {
            serde_json::from_slice::<serde_json::Value>(&bytes)
                .is_ok_and(|value| value["status"] == "owner")
        }) {
            assert!(
                owner.child.try_wait().unwrap().is_none(),
                "actual owner exited at startup"
            );
            assert!(
                Instant::now() < deadline,
                "actual owner startup exceeded deadline"
            );
            thread::sleep(Duration::from_millis(1));
        }
        owner
    }
    fn stop(&mut self) -> Result<(), String> {
        if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err(format!("owner exit: {status}"))
            };
        }
        let client = OwnerClient::attach(self.runtime.to_str().unwrap().to_owned())
            .map_err(|error| format!("{error:?}"))?;
        let reply = client
            .request(&query(Command::State))
            .map_err(|error| format!("{error:?}"))?;
        let Reply::Receipt(receipt) = reply else {
            return Err("missing owner state receipt".into());
        };
        let request = Request::mutation(
            RequestId::new(unique()).unwrap(),
            receipt.state().ok_or("missing state")?,
            Command::OwnerShutdown,
        )
        .unwrap();
        client
            .request(&request)
            .map_err(|error| format!("{error:?}"))?;
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            if let Some(status) = self.child.try_wait().map_err(|error| error.to_string())? {
                return if status.success() {
                    Ok(())
                } else {
                    Err(format!("owner exit: {status}"))
                };
            }
            if Instant::now() >= deadline {
                return Err("owned process did not exit after shutdown".into());
            }
            thread::sleep(Duration::from_millis(1));
        }
    }
}
impl Drop for ProcessOwner {
    fn drop(&mut self) {
        if let Err(error) = self.stop() {
            let _ = fs::write(self.artifacts.join("cleanup-error.txt"), error);
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
fn settle(client: &mut DesktopClient, request: Request) -> RequestId {
    let id = request.envelope().request_id.clone();
    client.submit(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        client.poll();
        if !client.model().record(&id).unwrap().in_flight {
            return id;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
}
fn refresh(client: &mut DesktopClient) -> std::sync::Arc<earph_desktop::StateView> {
    settle(client, query(Command::State));
    client.model().latest_state().unwrap().state.clone()
}
fn wav() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&52u32.to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&48000u32.to_le_bytes());
    bytes.extend_from_slice(&96000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    for sample in [0i16, 1, -1, 17, -17, 32767, -32768, 0] {
        bytes.extend_from_slice(&sample.to_le_bytes());
    }
    bytes
}

#[test]
fn standalone_owner_queue_and_saved_intent_flow_remains_muted_without_any_source_start() {
    let mut owner = ProcessOwner::start();
    let fixture = owner.artifacts.join("generated.wav");
    let source = wav();
    fs::write(&fixture, &source).unwrap();
    let mut client = DesktopClient::attach(owner.runtime.to_str().unwrap().to_owned()).unwrap();
    let initial = refresh(&mut client);
    assert!(initial.muted && !initial.output_consented && initial.output.is_none());
    let caps = settle(&mut client, query(Command::Capabilities));
    let Some(Ok(Reply::Receipt(reply))) = &client.model().record(&caps).unwrap().outcome else {
        panic!("capabilities")
    };
    let Some(ResultView::Capabilities(caps)) = &reply.result else {
        panic!("typed capabilities")
    };
    assert!(caps.profiles.save && caps.profiles.manual_resolution);
    assert!(caps.profiles.apply);
    assert_eq!(caps.profiles.exact_associations, cfg!(target_os = "macos"));
    assert_eq!(
        caps.profiles.exact_association_backends,
        if cfg!(target_os = "macos") {
            vec![earph_core::control::OutputBackend::CoreAudio]
        } else {
            vec![]
        }
    );
    assert!(caps.profiles.application.fixed_requires_current_arming);
    assert!(caps.profiles.package.new_ids_only && caps.profiles.package.requires_export_review);
    let append = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &initial,
        Command::QueueAppend {
            path: fixture.to_str().unwrap().into(),
        },
    )
    .unwrap();
    let append_id = settle(&mut client, append);
    assert_eq!(
        client.model().record(&append_id).unwrap().presentation,
        Presentation::Applied
    );
    let queue_id = settle(
        &mut client,
        query(Command::QueueGet {
            offset: 0,
            limit: 64,
        }),
    );
    let Some(Ok(Reply::Receipt(reply))) = &client.model().record(&queue_id).unwrap().outcome else {
        panic!("queue")
    };
    let Some(ResultView::Queue(queue)) = &reply.result else {
        panic!("typed queue")
    };
    assert_eq!(queue.entries.len(), 1);
    assert_eq!(queue.entries[0].path, fixture.to_str().unwrap());
    assert_eq!(
        queue.entries[0].status,
        earph_core::queue::PreparationStatus::Unprepared
    );
    let state = refresh(&mut client);
    let profile =
        Request::mutation(RequestId::new(unique()).unwrap(), &state, profile_payload()).unwrap();
    let profile_id = settle(&mut client, profile);
    assert_eq!(
        client.model().record(&profile_id).unwrap().presentation,
        Presentation::Accepted
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while client.model().record(&profile_id).unwrap().presentation == Presentation::Accepted {
        client.reconcile(&profile_id).unwrap();
        loop {
            client.poll();
            if !client.model().record(&profile_id).unwrap().in_flight {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
    }
    assert_eq!(
        client.model().record(&profile_id).unwrap().presentation,
        Presentation::Applied
    );
    let final_state = refresh(&mut client);
    assert!(final_state.muted && !final_state.output_consented && final_state.output.is_none());
    assert_eq!(final_state.source, None);
    assert_eq!(final_state.playback, None);
    assert_eq!(fs::read(&fixture).unwrap(), source);
    let (model, shutdown) = client.disconnect();
    assert!(!model.attached);
    shutdown.join().unwrap();
    assert!(
        owner.child.try_wait().unwrap().is_none(),
        "controller detach must leave the owner alive"
    );
    owner.stop().unwrap();
    assert!(owner.child.try_wait().unwrap().unwrap().success());
    assert!(
        fs::read(owner.artifacts.join("owner.stderr.log"))
            .unwrap()
            .is_empty()
    );
    fs::write(owner.artifacts.join("e2e-result.json"), serde_json::to_vec_pretty(&json!({"status":"pass","owner_pid":owner.child.id(),"runtime":owner.runtime,"fixture_bytes_unchanged":true,"source_start":false,"native_output":false,"owner_exit":0})).unwrap()).unwrap();
    println!("E2E artifacts: {}", owner.artifacts.display());
}

fn terminal(client: &mut DesktopClient, request: Request) -> earph_desktop::ReceiptView {
    let bytes = request.bytes().to_vec();
    let id = settle(client, request);
    let deadline = Instant::now() + Duration::from_secs(8);
    while client.model().record(&id).unwrap().presentation == Presentation::Accepted {
        assert!(Instant::now() < deadline, "exact request stayed Accepted");
        client.reconcile(&id).unwrap();
        loop {
            client.poll();
            let record = client.model().record(&id).unwrap();
            assert_eq!(record.request.bytes(), bytes);
            if !record.in_flight {
                break;
            }
            assert!(Instant::now() < deadline, "exact request did not settle");
            thread::sleep(Duration::from_millis(1));
        }
    }
    let Some(Ok(Reply::Receipt(receipt))) = &client.model().record(&id).unwrap().outcome else {
        panic!("actual owner receipt: {:?}", client.model().record(&id));
    };
    receipt.clone()
}
fn mutation(client: &mut DesktopClient, command: Command) -> earph_desktop::ReceiptView {
    let state = refresh(client);
    terminal(
        client,
        Request::mutation(RequestId::new(unique()).unwrap(), &state, command).unwrap(),
    )
}
fn saved_reference(
    client: &mut DesktopClient,
) -> earph_core::profile_control::ProfileCatalogReference {
    let receipt = mutation(client, profile_payload());
    assert_eq!(receipt.status, earph_desktop::ReplyStatus::Applied);
    let Some(ResultView::ProfileSaved(record)) = receipt.result else {
        panic!("typed saved profile");
    };
    earph_core::profile_control::ProfileCatalogReference {
        id: record.profile.id.clone(),
        revision: record.profile.revision,
        profile_sha256: record.profile_sha256,
    }
}

#[test]
#[cfg(target_os = "linux")]
fn standalone_discovery_reads_owned_hints_with_exact_replay_and_no_playback_changes() {
    use earph_core::{control::OutputBackend, devices::DeviceReadBoundary};
    use earph_desktop::{DeviceObservationView, ReplyStatus};
    let fixtures = PathBuf::from("/tmp").join(unique());
    fs::create_dir(&fixtures).unwrap();
    fs::set_permissions(&fixtures, fs::Permissions::from_mode(0o700)).unwrap();
    let configuration = fixtures.join("discovery-alsa.conf");
    let configuration_bytes = b"ctl.hw { @args [ CARD ] @args.CARD { type string } type hw card $CARD }\npcm.null { type null hint { show on description \"Owned duplicate label\" } }\npcm.owned_other { type null hint { show on description \"Owned duplicate label\" } }\n";
    fs::write(&configuration, configuration_bytes).unwrap();
    let mut owner = ProcessOwner::with_configuration(Some(&configuration));
    let mut client = DesktopClient::attach(owner.runtime.to_str().unwrap().into()).unwrap();
    let before = refresh(&mut client);
    let caps = terminal(&mut client, query(Command::Capabilities));
    let Some(ResultView::Capabilities(caps)) = caps.result else {
        panic!("typed capabilities");
    };
    assert_eq!(caps.devices.backends, vec![OutputBackend::Alsa]);
    assert_eq!(caps.devices.methods, ["devices.list", "devices.inspect"]);
    assert!(caps.devices.asynchronous_replay);
    let mut retained = vec![];
    for (index, payload) in [
        Command::DevicesList {
            backend: OutputBackend::Alsa,
        },
        Command::DevicesInspect {
            backend: OutputBackend::Alsa,
            endpoint: "null".into(),
        },
        Command::DevicesInspect {
            backend: OutputBackend::Alsa,
            endpoint: "not-an-advertised-hint".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let request = query(payload);
        let exact = request.bytes().to_vec();
        let id = settle(&mut client, request.clone());
        let record = client.model().record(&id).unwrap();
        assert_eq!(record.presentation, Presentation::Accepted);
        let Some(Ok(Reply::Receipt(accepted))) = &record.outcome else {
            panic!("accepted receipt");
        };
        assert!(accepted.result.is_none());
        fs::write(
            owner
                .artifacts
                .join(format!("discovery-{index}-request.json")),
            &exact,
        )
        .unwrap();
        fs::write(
            owner
                .artifacts
                .join(format!("discovery-{index}-accepted.json")),
            record.outcome.as_ref().unwrap().as_ref().unwrap().bytes(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(8);
        while client.model().record(&id).unwrap().presentation == Presentation::Accepted {
            client.reconcile(&id).unwrap();
            while client.model().record(&id).unwrap().in_flight {
                client.poll();
                assert!(
                    Instant::now() < deadline,
                    "actual helper discovery stayed pending"
                );
                thread::sleep(Duration::from_millis(1));
            }
            assert_eq!(client.model().record(&id).unwrap().request.bytes(), exact);
        }
        let record = client.model().record(&id).unwrap();
        let Some(Ok(Reply::Receipt(receipt))) = &record.outcome else {
            panic!("actual terminal receipt: {record:?}");
        };
        assert_eq!(receipt.status, ReplyStatus::Applied);
        assert_eq!(receipt.revision, before.revision);
        assert_eq!(receipt.epochs, before.epochs);
        let Some(ResultView::Devices(result)) = &receipt.result else {
            panic!("typed discovery result");
        };
        assert_eq!(Some(&result.owner_id), before.owner_id.as_ref());
        assert_eq!(result.backend, OutputBackend::Alsa);
        assert_eq!(result.read_boundary, DeviceReadBoundary::AlsaPcmHints);
        match (&result.observation, index) {
            (
                DeviceObservationView::List {
                    devices,
                    complete,
                    failures,
                },
                0,
            ) => {
                assert!(*complete && failures.is_empty());
                for endpoint in ["null", "owned_other"] {
                    let device = devices
                        .iter()
                        .find(|device| device.endpoint == endpoint)
                        .expect("explicit owned hint");
                    let selection = device.selection.as_ref().unwrap();
                    assert_eq!(selection.backend, OutputBackend::Alsa);
                    assert_eq!(selection.endpoint, endpoint);
                }
                let null = devices
                    .iter()
                    .find(|device| device.endpoint == "null")
                    .unwrap();
                let other = devices
                    .iter()
                    .find(|device| device.endpoint == "owned_other")
                    .unwrap();
                assert_eq!(null.description, other.description);
            }
            (
                DeviceObservationView::Inspection {
                    endpoint,
                    advertisement,
                    current_properties,
                    complete,
                    failures,
                },
                1 | 2,
            ) => {
                assert!(*complete && failures.is_empty());
                assert_eq!(
                    endpoint,
                    if index == 1 {
                        "null"
                    } else {
                        "not-an-advertised-hint"
                    }
                );
                assert_eq!(advertisement.is_some(), index == 1);
                assert!(
                    current_properties.is_none(),
                    "ALSA hint cannot establish current properties"
                );
            }
            observation => panic!("wrong native observation: {observation:?}"),
        }
        let terminal_bytes = record
            .outcome
            .as_ref()
            .unwrap()
            .as_ref()
            .unwrap()
            .bytes()
            .to_vec();
        let replay = OwnerClient::attach(owner.runtime.to_str().unwrap().into())
            .unwrap()
            .request(&request)
            .unwrap();
        let record = client.model().record(&id).unwrap();
        assert_eq!(record.request.bytes(), exact);
        assert_eq!(replay.bytes(), terminal_bytes);
        assert_eq!(
            earph_engine::request(&owner.runtime, &exact).unwrap(),
            terminal_bytes
        );
        fs::write(
            owner
                .artifacts
                .join(format!("discovery-{index}-terminal.json")),
            &terminal_bytes,
        )
        .unwrap();
        retained.push(id.as_str().to_owned());
    }
    let after = refresh(&mut client);
    assert_eq!(after.owner_id, before.owner_id);
    assert_eq!(after.session_id, before.session_id);
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.epochs, before.epochs);
    assert_eq!(after.device_epoch, before.device_epoch);
    assert_eq!(after.graph_epoch, before.graph_epoch);
    assert_eq!(after.transport, before.transport);
    assert_eq!(after.source, before.source);
    assert_eq!(after.output, before.output);
    assert_eq!(after.playback, before.playback);
    assert_eq!(after.software_gain_db, before.software_gain_db);
    assert_eq!(after.local_volume_policy, before.local_volume_policy);
    assert_eq!(after.volume_policy, before.volume_policy);
    assert!(after.muted && !after.output_consented);
    assert_eq!(fs::read(&configuration).unwrap(), configuration_bytes);
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
    owner.stop().unwrap();
    assert_eq!(owner.child.try_wait().unwrap().unwrap().code(), Some(0));
    assert!(
        fs::read(owner.artifacts.join("owner.stderr.log"))
            .unwrap()
            .is_empty()
    );
    fs::write(owner.artifacts.join("discovery-e2e-result.json"), serde_json::to_vec_pretty(&json!({"status":"pass","owner_pid":owner.child.id(),"runtime":owner.runtime,"fixture":configuration,"retained_requests":retained,"accepted_then_terminal":true,"exact_terminal_replay":true,"unknown_properties_preserved":true,"playback_state_unchanged":true,"source_start":false,"native_output":false,"owner_exit":0})).unwrap()).unwrap();
    println!(
        "Discovery E2E fixtures: {}; owner artifacts: {}",
        fixtures.display(),
        owner.artifacts.display()
    );
}

#[test]
fn standalone_owner_packages_roundtrip_through_the_actual_desktop_consumer() {
    use earph_core::profile_control::{
        PackageContentReview, PackageExportReview, PackageRedistribution,
    };
    let mut source = ProcessOwner::start();
    let mut client = DesktopClient::attach(source.runtime.to_str().unwrap().into()).unwrap();
    let reference = saved_reference(&mut client);
    let before = refresh(&mut client);
    let preview = mutation(
        &mut client,
        Command::ProfilesPackagePreview {
            profile: reference.clone(),
        },
    );
    assert_eq!(preview.status, earph_desktop::ReplyStatus::Applied);
    assert!(
        preview.revision > before.revision,
        "package preview is an asynchronous revisioned operation"
    );
    let Some(ResultView::ProfilePackage(preview)) = preview.result else {
        panic!("typed package preview");
    };
    assert_eq!(
        preview.operation,
        earph_desktop::ProfilePackageStageView::Previewed
    );
    assert_eq!(preview.review_sources.len(), preview.manifest.objects.len());
    let destination = source.artifacts.join("exported");
    let exported = mutation(
        &mut client,
        Command::ProfilesExport {
            directory: destination.to_str().unwrap().into(),
            profile: reference.clone(),
            expected_manifest_sha256: preview.manifest_sha256,
            export_review: PackageExportReview::ReviewedExactCandidate,
            content_review: PackageContentReview::ReviewedNoPrivateData,
            redistribution: PackageRedistribution::RefuseMeasurementBytes,
        },
    );
    assert_eq!(exported.status, earph_desktop::ReplyStatus::Applied);
    let inspected = mutation(
        &mut client,
        Command::ProfilesPackageInspect {
            directory: destination.to_str().unwrap().into(),
        },
    );
    let Some(ResultView::ProfilePackage(inspected)) = inspected.result else {
        panic!("typed package inspection");
    };
    assert_eq!(inspected.manifest, preview.manifest);
    let mut target = ProcessOwner::start();
    let mut target_client = DesktopClient::attach(target.runtime.to_str().unwrap().into()).unwrap();
    let initial = refresh(&mut target_client);
    let import = Command::ProfilesImportPackage {
        directory: destination.to_str().unwrap().into(),
        expected_manifest_sha256: inspected.manifest_sha256,
        profile: reference.clone(),
    };
    let receipt = mutation(&mut target_client, import.clone());
    assert_eq!(receipt.status, earph_desktop::ReplyStatus::Applied);
    let Some(ResultView::ProfileSaved(record)) = receipt.result else {
        panic!("typed imported profile");
    };
    assert_eq!(record.profile_sha256, reference.profile_sha256);
    let collision = mutation(&mut target_client, import);
    assert_eq!(collision.status, earph_desktop::ReplyStatus::Rejected);
    let Some(ResultView::ProfilePackageIssue(issue)) = collision.result else {
        panic!("typed package collision issue");
    };
    assert_eq!(
        issue.reason,
        earph_desktop::ProfilePackageReasonView::IdentityCollision
    );
    assert_eq!(
        issue.filesystem_state,
        earph_desktop::PackageFilesystemStateView::Unchanged
    );
    let after = refresh(&mut target_client);
    assert_eq!(after.local_volume_policy, initial.local_volume_policy);
    assert!(
        after.configured_profile.is_none() && after.output.is_none() && after.playback.is_none()
    );
    assert!(after.muted && !after.output_consented);
    for object in &inspected.manifest.objects {
        let name = object.file_name();
        assert_eq!(
            fs::read(destination.join(&name)).unwrap(),
            fs::read(target.runtime.join("profiles").join(name)).unwrap()
        );
    }
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
    let (_, shutdown) = target_client.disconnect();
    shutdown.join().unwrap();
    source.stop().unwrap();
    target.stop().unwrap();
    println!(
        "Package E2E artifacts: {} {}",
        source.artifacts.display(),
        target.artifacts.display()
    );
}

#[test]
#[cfg(target_os = "linux")]
fn muted_real_profile_preparation_and_application_preserve_actual_backend_observations() {
    use earph_core::{
        control::{OutputBackend, OutputConfiguration, OutputRoute, SourceKind},
        ids::SessionId,
        safety::{FidelityPolicy, VolumeMode},
    };
    let artifacts = PathBuf::from("/tmp").join(unique());
    fs::create_dir(&artifacts).unwrap();
    fs::set_permissions(&artifacts, fs::Permissions::from_mode(0o700)).unwrap();
    let capture = artifacts.join("capture.pcm");
    let configuration = artifacts.join("alsa.conf");
    fs::write(&configuration, format!("pcm.null {{ type null }}\npcm.earph_capture {{ type file slave.pcm null file \"{}\" format raw }}\n", capture.display())).unwrap();
    let fixture = artifacts.join("zero.wav");
    let mut samples = wav();
    samples[22..24].copy_from_slice(&2u16.to_le_bytes());
    samples[28..32].copy_from_slice(&192000u32.to_le_bytes());
    samples[32..34].copy_from_slice(&4u16.to_le_bytes());
    samples[44..].fill(0);
    fs::write(&fixture, &samples).unwrap();
    let mut owner = ProcessOwner::with_configuration(Some(&configuration));
    let mut client = DesktopClient::attach(owner.runtime.to_str().unwrap().into()).unwrap();
    let reference = saved_reference(&mut client);
    let output = OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: "earph_capture".into(),
        device_id: earph_core::ids::DeviceId::new("offline").unwrap(),
        profile_id: reference.id.clone(),
        period_frames: 64,
        buffer_frames: 256,
        volume_mode: VolumeMode::Software,
        fidelity: FidelityPolicy::CompatibleConversion,
    };
    assert_eq!(
        mutation(
            &mut client,
            Command::OutputSelect {
                output: output.clone()
            }
        )
        .status,
        earph_desktop::ReplyStatus::Applied
    );
    assert_eq!(
        mutation(
            &mut client,
            Command::OutputConsent {
                device_id: output.device_id.clone(),
                profile_id: output.profile_id.clone()
            }
        )
        .status,
        earph_desktop::ReplyStatus::Applied
    );
    assert_eq!(
        mutation(
            &mut client,
            Command::QueueAppend {
                path: fixture.to_str().unwrap().into()
            }
        )
        .status,
        earph_desktop::ReplyStatus::Applied
    );
    let before = refresh(&mut client);
    let session = SessionId::new(unique()).unwrap();
    let start = Request::start(
        RequestId::new(unique()).unwrap(),
        &before,
        session.clone(),
        Command::Start {
            source: SourceKind::LocalFile,
            device_id: output.device_id.clone(),
            takeover: false,
            seekable: true,
        },
    )
    .unwrap();
    assert!(before.session_id.is_none());
    let exact = start.bytes().to_vec();
    let id = settle(&mut client, start);
    let record = client.model().record(&id).unwrap();
    assert_eq!(record.presentation, Presentation::Accepted);
    assert_eq!(record.request.bytes(), exact);
    let deadline = Instant::now() + Duration::from_secs(8);
    let opened = loop {
        let state = refresh(&mut client);
        if state.playback.as_ref().is_some_and(|p| p.backend.is_some()) {
            break state;
        }
        assert!(
            Instant::now() < deadline,
            "actual muted backend never opened: {:?}",
            state.error
        );
        thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(opened.session_id.as_ref(), Some(&session));
    assert!(opened.muted);
    client.reconcile(&id).unwrap();
    while client.model().record(&id).unwrap().in_flight {
        client.poll();
        assert!(
            Instant::now() < deadline,
            "muted Start replay did not return"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let record = client.model().record(&id).unwrap();
    assert_eq!(record.presentation, Presentation::Accepted);
    assert_eq!(record.request.bytes(), exact);
    let backend = opened.playback.as_ref().unwrap().backend.as_ref().unwrap();
    assert_eq!(backend.backend, OutputBackend::Alsa);
    assert!(backend.device_identity_sha256.is_none());
    let prepared = mutation(
        &mut client,
        Command::ProfilesPrepare {
            id: reference.id.clone(),
            expected_profile_revision: reference.revision,
            profile_sha256: reference.profile_sha256,
        },
    );
    assert_eq!(
        prepared.status,
        earph_desktop::ReplyStatus::Applied,
        "{prepared:?}"
    );
    let Some(ResultView::GraphPrepared(prepared)) = prepared.result else {
        panic!("actual typed prepared profile");
    };
    let binding = prepared.preview.profile_application.as_ref().unwrap();
    assert_eq!(binding.profile, reference);
    assert_eq!(binding.volume_mode, VolumeMode::Software);
    assert_eq!(binding.fidelity, FidelityPolicy::CompatibleConversion);
    assert_eq!(
        binding.device_identity_sha256,
        backend.device_identity_sha256
    );
    assert_eq!(
        prepared.preview.backend.as_ref().unwrap().client_format,
        backend.client_format
    );
    let applied = mutation(
        &mut client,
        Command::ProfilesApply {
            id: reference.id.clone(),
            expected_profile_revision: reference.revision,
            profile_sha256: reference.profile_sha256,
            token: prepared.token.clone(),
            graph_hash: prepared.preview.graph_hash.as_str().into(),
        },
    );
    assert_eq!(
        applied.status,
        earph_desktop::ReplyStatus::Applied,
        "{applied:?}"
    );
    assert!(applied.activation_frame.is_some());
    let state = refresh(&mut client);
    assert_eq!(state.configured_profile.as_ref(), Some(&reference));
    assert!(state.muted && !state.output_consented);
    assert_eq!(state.local_volume_policy, opened.local_volume_policy);
    assert_eq!(state.volume_policy.software_cap_db, -20.0);
    assert_eq!(state.software_gain_db, opened.software_gain_db);
    let labels = widgets::signal_path_labels(owner.runtime.to_str().unwrap());
    for text in [
        "Playback format:",
        "48000",
        "Backend: Alsa",
        "Backend measurement boundary:",
        "Active processing hash:",
        "Submitted output: 0 frames",
        "Native device identity: unknown",
        "Output activity: unavailable",
    ] {
        assert!(
            labels.iter().any(|label| label.contains(text)),
            "missing actual Signal Path fact: {text}; {labels:?}"
        );
    }
    assert_eq!(refresh(&mut client).revision, state.revision);
    assert_eq!(fs::read(&fixture).unwrap(), samples);
    match fs::metadata(&capture) {
        Ok(metadata) => assert_eq!(
            metadata.len(),
            0,
            "muted fixture unexpectedly submitted output bytes"
        ),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
    let stopped = mutation(&mut client, Command::Stop);
    assert_eq!(stopped.status, earph_desktop::ReplyStatus::Applied);
    client.reconcile(&id).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while client.model().record(&id).unwrap().in_flight {
        client.poll();
        assert!(
            Instant::now() < deadline,
            "stopped Start replay did not return"
        );
        thread::sleep(Duration::from_millis(1));
    }
    let record = client.model().record(&id).unwrap();
    assert_eq!(record.presentation, Presentation::Cancelled);
    assert_eq!(record.request.bytes(), exact);
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
    owner.stop().unwrap();
    assert_eq!(owner.child.try_wait().unwrap().unwrap().code(), Some(0));
    println!(
        "Muted live E2E fixtures: {}; owner artifacts: {}",
        artifacts.display(),
        owner.artifacts.display()
    );
}
