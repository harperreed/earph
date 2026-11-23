// ABOUTME: Displays typed owner observations without turning requested settings into device evidence.
// ABOUTME: Keeps source positions, output accounting, processing, and native measurement boundaries distinct.
use super::{GuiController, UiIntent};
use crate::StateView;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, controller: &mut GuiController) {
    ui.heading("Signal Path / Recovery");
    if let Some(state) = controller.state() {
        observation(ui, state);
    } else {
        ui.label("No owner snapshot has been observed.");
    }
    if ui.button("Refresh actual state").clicked() {
        controller.handle(UiIntent::Refresh);
    }
    ui.label("Reconnect in Connect. Raw receipt details below are local data, not a redacted support bundle.");
    if ui
        .add_enabled(
            controller.control_edit_available(),
            egui::Button::new("Inspect actual graph"),
        )
        .clicked()
        && let Some(state) = controller.state()
    {
        controller.handle(UiIntent::ProfileAction {
            basis: std::sync::Arc::new(state.clone()),
            command: earph_core::control::Command::GraphInspect {},
        });
    }
    if let Some(observed) = &controller.profiles.graph {
        ui.label(format!(
            "Graph inspection owner {:?}; revision {}; requests {:?}",
            observed.basis.owner_id, observed.basis.revision, observed.request_ids
        ));
        super::dsp::inspection(ui, &observed.value);
    }
    super::diagnostics::show(ui, controller);
}

fn observation(ui: &mut egui::Ui, state: &StateView) {
    ui.label(format!("Requested output: {:?}", state.output));
    if let Some(dsd) = &state.dsd_playback {
        ui.label(format!("DSD source format: {:?}; accepted source position: {} of {} bits per channel; decoded source position: {} bits",dsd.source_format,dsd.source_position_bits,dsd.total_source_bits,dsd.decoded_position_bits));
        ui.label(format!(
            "DoP carrier format: {:?}; live output: {:?}; previous closed boundary: {:?}",
            dsd.carrier_format, dsd.output, dsd.last_boundary
        ));
        ui.label("Endpoint qualification: unverified. Carrier negotiation establishes no audible position or physical transition safety.");
        return;
    }
    ui.label(format!(
        "Configured profile intent: {:?}",
        state.configured_profile
    ));
    ui.label(format!(
        "Decoded position: {} source frames",
        state.decoded_position_frames
    ));
    ui.label(format!(
        "Audible position: {}",
        frames(state.audible_position_frames)
    ));
    let Some(playback) = state.playback_facts() else {
        ui.label("Playback observation: unavailable");
        ui.label("Backend format: unknown");
        ui.label("Native device identity: unknown");
        ui.label("Output activity: unavailable");
        return;
    };
    if let Some(network) = state.network_observation() {
        ui.label(format!(
            "HTTP FLAC media: {}; lifecycle: {:?}",
            network.binding.media_id.as_str(),
            network.lifecycle
        ));
        ui.label(format!("Received FLAC format: {:?}", network.format));
        ui.label(format!("Output format: {:?}", network.output_format));
        ui.label(format!(
            "Total source frames: {}",
            network
                .total_frames
                .map_or_else(|| "unknown".into(), |n| n.to_string())
        ));
        ui.label(format!(
            "Source position: {} frames; audible source position: {}",
            playback.source_position_frames,
            frames(playback.audible_source_position_frames)
        ));
        ui.label(format!(
            "Source epochs: {:?}; output epochs: {:?}; graph {}; device {}",
            network.source_epochs, network.output_epochs, network.graph_epoch, network.device_epoch
        ));
        if let Some(boundary) = network.replacement_boundary.as_deref() {
            let facts =
                earph_core::diagnostics::DiagnosticNetworkReplacementBoundary::from(boundary);
            replacement_boundary(ui, &facts);
        }
        ui.label("Network source is nonseekable; next, previous, profile application, and strict preservation are unavailable.");
    } else if let Some(local) = &state.playback {
        ui.label(format!("Playback format: {:?}", playback.format));
        ui.label(format!(
            "Source entry: {}; position: {} source frames",
            local.source_entry_id.as_str(),
            playback.source_position_frames
        ));
        ui.label(format!(
            "Audible source entry: {}; position: {}",
            local
                .audible_entry_id
                .as_ref()
                .map_or("unknown", |id| id.as_str()),
            frames(playback.audible_source_position_frames)
        ));
    }
    ui.label(format!(
        "Submitted output: {} frames; consumed output: {}; backend delay: {}",
        playback.submitted_frames,
        frames(playback.consumed_frames),
        frames(playback.backend_delay_frames)
    ));
    ui.label(format!(
        "Consumption boundary: {}",
        playback.consumption_basis
    ));
    ui.label(format!(
        "Playback modified: {}; input peak: {:?}; output peak: {:?}",
        playback.modified, playback.input_peak, playback.output_peak
    ));
    if let Some(backend) = playback.backend {
        ui.label(format!(
            "Backend: {:?}; client format: {:?}",
            backend.backend, backend.client_format
        ));
        ui.label(format!(
            "Backend format: {}",
            backend
                .device_format
                .as_ref()
                .map_or_else(|| "unknown".into(), |format| format!("{format:?}"))
        ));
        ui.label(format!(
            "Native device identity: {}",
            backend
                .device_identity_sha256
                .as_ref()
                .map_or("unknown", |hash| hash.as_str())
        ));
        ui.label(format!(
            "Backend measurement boundary: {}; preservation: {:?}",
            backend.measurement_boundary, backend.preservation
        ));
    } else {
        ui.label("Backend format: unknown");
        ui.label("Native device identity: unknown");
    }
    if let Some(processing) = playback.processing {
        ui.label(format!(
            "Active processing hash: {}",
            processing.graph_hash.as_str()
        ));
        ui.label(format!("Processing timing: {:?}", processing.timing));
        ui.label(format!(
            "Processing memory: {} bytes; audio payload: {:?}; saturation count: {}",
            processing.resident_bytes, processing.audio_payload_bytes, processing.saturation_count
        ));
        ui.label(format!(
            "Graph peak: {:?}; prequantization peak: {:?}; failure node: {:?}",
            processing.graph_peak, processing.prequantization_peak, processing.failure_node
        ));
        if let Some(streaming) = &processing.streaming {
            ui.label(format!("Streaming frame accounting: {streaming:?}"));
        }
        if let Some(boundary) = &processing.source_boundary {
            ui.label(format!("Source replacement boundary: {boundary:?}"));
        }
    } else {
        ui.label("Active processing: unavailable");
    }
    if let Some(activity) = playback.output_activity {
        ui.label(format!(
            "Output activity scope: {:?}; origin: {:?}",
            activity.scope, activity.origin_epochs
        ));
        ui.label(format!(
            "Output activity boundary: {:?}",
            activity.measurement_boundary
        ));
        ui.label(format!(
            "Native opens: {}; SDK write attempts: {}; accepted frames: {}; SDK start attempts: {}",
            activity.native_open_successes,
            activity.sdk_write_attempts,
            activity.accepted_frames,
            activity.sdk_start_attempts
        ));
        ui.label(if activity.counter_overflow {
            "Activity counters overflowed; counts are lower bounds."
        } else {
            "Activity counters have not overflowed."
        });
        ui.label(format!(
            "Output close confirmed: {}",
            activity.close_confirmed
        ));
    } else {
        ui.label("Output activity: unavailable");
    }
    ui.label("These observations do not measure downstream DAC or acoustic behavior.");
}

pub(super) fn replacement_boundary(
    ui: &mut egui::Ui,
    boundary: &earph_core::diagnostics::DiagnosticNetworkReplacementBoundary,
) {
    ui.label(format!(
        "Network graph replacement: accepted source frontier {} frames; discarded output {} frames",
        boundary.source_position_frames, boundary.discarded_output_frames
    ));
    ui.label(format!(
        "Discarded output format: {:?}",
        boundary.previous_output_format
    ));
    ui.label(format!(
        "Retained replacement epochs: source {:?}; previous output {:?}; replacement output {:?}",
        boundary.source_epochs, boundary.previous_output_epochs, boundary.output_epochs
    ));
}

fn frames(value: Option<u64>) -> String {
    value.map_or_else(|| "unknown".into(), |n| format!("{n} frames"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network_fixture;
    use earph_core::{
        audio::{ChannelLayout, PcmFormat, PcmRepresentation},
        control::{ControlLimits, OutputBackend},
        graph_control::PlaybackTiming,
        ids::{Epochs, OwnerId, QueueEntryId, ZoneId},
        session::*,
    };

    #[test]
    fn http_widgets_show_received_format_unknown_total_and_separate_output_progress() {
        let session = network_fixture::ready(None);
        let state: StateView =
            serde_json::from_value(serde_json::to_value(session.snapshot()).unwrap()).unwrap();
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| observation(ui, &state));
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        let labels: Vec<_> = tree
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().or_else(|| n.value()))
            .collect();
        for expected in [
            "HTTP FLAC media: staged-1; lifecycle: Ready",
            "Total source frames: unknown",
            "Network source is nonseekable; next, previous, profile application, and strict preservation are unavailable.",
            "Submitted output: 0 frames; consumed output: unknown; backend delay: unknown",
        ] {
            assert!(labels.contains(&expected), "missing {expected}: {labels:?}");
        }
        assert!(labels.iter().any(|s| s.starts_with("Received FLAC format:")
            && s.contains("48000")
            && s.contains("Signed24")));
        assert!(labels.iter().any(|s| s.starts_with("Output format:")
            && s.contains("96000")
            && s.contains("Signed16")));
        assert!(!labels.iter().any(|s| s.starts_with("Source entry:")));
    }

    #[test]
    fn network_replacement_widgets_show_source_frontier_and_previous_output_discard_domain() {
        let session = network_fixture::replacement();
        let state: StateView =
            serde_json::from_value(serde_json::to_value(session.snapshot()).unwrap()).unwrap();
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| observation(ui, &state));
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        let labels: Vec<_> = tree
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().or_else(|| n.value()))
            .collect();
        assert!(labels.contains(&"Network graph replacement: accepted source frontier 23 frames; discarded output 19 frames"));
        assert!(
            labels
                .iter()
                .any(|label| label.starts_with("Discarded output format:")
                    && label.contains("48000")
                    && label.contains("Signed16"))
        );
        assert!(
            !labels
                .iter()
                .any(|label| label.starts_with("Source replacement boundary:"))
        );
    }

    #[test]
    fn retired_http_widgets_keep_final_facts_without_a_live_session_or_grant() {
        let mut session = network_fixture::ready(None);
        let command = network_fixture::command(
            &session,
            "retire",
            serde_json::json!({"method":"sources.grants.revoke","expected_generation":1}),
        );
        session.submit_with_network_effects(
            command,
            40,
            |_| unreachable!(),
            |_, _| Ok(()),
            |_| Ok(()),
        );
        let closing = session.closing_network_source().unwrap();
        session
            .complete_network_retirement(
                earph_core::network_source::NetworkRetirementAck {
                    binding: closing.context.binding.clone(),
                    source_epochs: closing.context.source_epochs,
                    output_epochs: closing.output_epochs,
                    graph_epoch: closing.graph_epoch,
                    device_epoch: closing.device_epoch,
                    observation: None,
                },
                50,
            )
            .unwrap();
        let state: StateView =
            serde_json::from_value(serde_json::to_value(session.snapshot()).unwrap()).unwrap();
        assert!(state.session_id.is_none());
        assert!(state.source.is_none());
        assert!(session.network_grants().is_none());
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| observation(ui, &state));
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        assert!(
            tree.nodes
                .iter()
                .any(|(_, n)| n.label().or_else(|| n.value())
                    == Some("HTTP FLAC media: staged-1; lifecycle: Completed"))
        );
    }

    #[test]
    fn core_observations_keep_native_format_counters_and_source_positions_distinct() {
        let mut state = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("view-fixture").unwrap(),
            ControlLimits::default(),
        )
        .snapshot();
        state.decoded_position_frames = 101;
        state.audible_position_frames = Some(11);
        let format =
            PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
        let native =
            PcmFormat::new(96000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
        state.observation = (PlaybackObservation {
            format,
            submitted_frames: 47,
            consumed_frames: Some(31),
            backend_delay_frames: None,
            consumption_basis: "controlled serialization boundary",
            source_entry_id: QueueEntryId::new("decoded-entry").unwrap(),
            source_position_frames: 23,
            audible_entry_id: Some(QueueEntryId::new("audible-entry").unwrap()),
            audible_source_position_frames: Some(7),
            input_peak: None,
            output_peak: Some(0.5),
            modified: true,
            backend: Some(BackendObservation {
                backend: OutputBackend::CoreAudio,
                device_identity_sha256: Some("a".repeat(64).try_into().unwrap()),
                audio_storage_bytes: 1024,
                client_format: format.into(),
                device_format: Some(native.into()),
                measurement_boundary: "controlled native format; no hardware qualification",
                preservation: PreservationState::Modified,
            }),
            processing: Some(PlaybackProcessingObservation {
                resident_bytes: 2048,
                audio_payload_bytes: Some(1024),
                failure_node: None,
                graph_hash: "b".repeat(64),
                timing: PlaybackTiming::SameRate {
                    latency_frames: 13,
                    tail_frames: 17,
                },
                streaming: None,
                source_boundary: None,
                saturation_count: 3,
                graph_peak: Some(0.75),
                prequantization_peak: Some(0.5),
            }),
            output_activity: Some(OutputActivity {
                scope: OutputActivityScope::OutputInstanceAcrossNativeReopens,
                measurement_boundary: OutputActivityBoundary::CoreAudioSdkEntryCalls,
                origin_epochs: Epochs {
                    session: 5,
                    stream: 9,
                },
                native_open_successes: 2,
                sdk_write_attempts: u64::MAX,
                accepted_frames: 53,
                sdk_start_attempts: 1,
                counter_overflow: true,
                close_confirmed: false,
            }),
        })
        .into();
        let state: StateView =
            serde_json::from_value(serde_json::to_value(state).unwrap()).unwrap();
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| observation(ui, &state));
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        let labels: Vec<_> = tree
            .nodes
            .iter()
            .filter_map(|(_, node)| node.label().or_else(|| node.value()))
            .collect();
        for expected in [
            "Decoded position: 101 source frames",
            "Audible position: 11 frames",
            "Source entry: decoded-entry; position: 23 source frames",
            "Audible source entry: audible-entry; position: 7 frames",
            "Submitted output: 47 frames; consumed output: 31 frames; backend delay: unknown",
            "Native opens: 2; SDK write attempts: 18446744073709551615; accepted frames: 53; SDK start attempts: 1",
            "Activity counters overflowed; counts are lower bounds.",
            "Output close confirmed: false",
        ] {
            assert!(
                labels.contains(&expected),
                "missing distinct fact: {expected}; {labels:?}"
            );
        }
        assert!(
            labels
                .iter()
                .any(|line| line.starts_with("Playback format:")
                    && line.contains("48000")
                    && line.contains("Signed16"))
        );
        assert!(labels.iter().any(|line| line.starts_with("Backend format:")
            && line.contains("96000")
            && line.contains("Signed24")));
        assert!(
            labels
                .iter()
                .any(|line| line.starts_with("Backend measurement boundary:")
                    && line.contains("no hardware qualification")
                    && line.contains("Modified"))
        );
        assert!(
            labels
                .iter()
                .any(|line| line.starts_with("Native device identity:")
                    && line.contains(&"a".repeat(64)))
        );
        assert!(
            labels
                .iter()
                .any(|line| line.starts_with("Active processing hash:")
                    && line.contains(&"b".repeat(64)))
        );
        assert!(
            labels
                .iter()
                .any(|line| line.starts_with("Processing timing:")
                    && line.contains("13")
                    && line.contains("17"))
        );
    }
}
