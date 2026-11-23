// ABOUTME: Verifies bounded PCM ownership and ordered stream lifecycle decisions.
// ABOUTME: Exercises real wire producers without pretending to observe an audio device.
use earph_sendspin::{message::*, player::*, timing::*};
use serde_json::json;

fn binding() -> PlayerBinding {
    PlayerBinding::new(Binding::new([9; 32], 4, 2), 7, 8)
}
fn timeline(limits: TimelineLimits) -> PlayerTimeline {
    let mut t = PlayerTimeline::new(binding(), limits);
    t.set_participation(true, true).unwrap();
    t
}
fn deliver(
    t: &mut PlayerTimeline,
    kind: &str,
    payload: serde_json::Value,
) -> Result<TimelineAction, PlayerError> {
    let body = serde_json::to_vec(&json!({"type":kind,"payload":payload})).unwrap();
    let mut producer = Outgoing::new(0, body, MessageLimits::default()).unwrap();
    let complete = Reassembler::new(MessageLimits::default())
        .push(
            &producer.next_record().unwrap(),
            ReceiveStamp::new(100).unwrap(),
        )
        .unwrap()
        .unwrap();
    t.apply(
        PlayerEvent::decode(&complete, true).unwrap().unwrap(),
        binding(),
        true,
        None,
    )
}
fn start(t: &mut PlayerTimeline) {
    deliver(t,"stream/start",json!({"server_transmitted":1,"player":{"codec":"pcm","sample_rate":48000,"channels":2,"bit_depth":16}})).unwrap();
}
fn audio(
    t: &mut PlayerTimeline,
    timestamp: i64,
    frames: usize,
) -> Result<TimelineAction, PlayerError> {
    let mut body = timestamp.to_be_bytes().to_vec();
    body.extend_from_slice(&1_u32.to_be_bytes());
    body.extend(std::iter::repeat_n([0, 128, 255, 127], frames).flatten());
    let mut producer = Outgoing::new(4, body, MessageLimits::default()).unwrap();
    let complete = Reassembler::new(MessageLimits::default())
        .push(
            &producer.next_record().unwrap(),
            ReceiveStamp::new(100).unwrap(),
        )
        .unwrap()
        .unwrap();
    t.apply(
        PlayerEvent::decode(&complete, true).unwrap().unwrap(),
        binding(),
        true,
        None,
    )
}

#[test]
fn same_format_start_retains_pcm_and_end_clears_without_finishing() {
    let mut t = timeline(TimelineLimits::default());
    start(&mut t);
    audio(&mut t, 1000, 1).unwrap();
    let before = t.usage();
    let generation = t.generation();
    start(&mut t);
    assert_eq!(t.usage(), before);
    assert_eq!(t.generation(), generation);
    let chunk = t.take_chunk().unwrap();
    assert_eq!(chunk.samples(), &[i16::MIN, i16::MAX]);
    assert_eq!(chunk.format(), StreamFormat::PCM);
    assert_eq!(chunk.original_frames(), 1);
    assert_eq!(chunk.arrival().microseconds(), 100);
    assert_eq!(t.usage().allocated_frames, 1);
    assert!(!format!("{chunk:?}").contains("-32768"));
    assert!(matches!(
        deliver(&mut t, "stream/end", json!({})).unwrap(),
        TimelineAction::Ended
    ));
    assert!(!t.is_current(&chunk));
    assert_eq!(t.usage().allocated_frames, 1);
    t.release_chunk(chunk).unwrap();
    assert_eq!(t.usage(), TimelineUsage::default());
    assert_eq!(
        audio(&mut t, 2000, 1).unwrap_err(),
        PlayerError::InactiveStream
    );
    start(&mut t);
    assert_ne!(t.generation(), generation);
}

#[test]
fn full_clear_retains_format_and_keeps_transferred_allocation_charged() {
    let mut t = timeline(TimelineLimits::new(1000, 10, 4).unwrap());
    start(&mut t);
    audio(&mut t, 1000, 3).unwrap();
    let chunk = t.take_chunk().unwrap();
    audio(&mut t, 2000, 3).unwrap();
    deliver(&mut t, "stream/clear", json!({"server_transmitted":2})).unwrap();
    assert_eq!(t.format(), Some(StreamFormat::PCM));
    assert_eq!(t.usage().wire_bytes, 0);
    assert_eq!(t.usage().allocated_frames, 3);
    assert!(!t.is_current(&chunk));
    audio(&mut t, 3000, 7).unwrap();
    assert_eq!(
        audio(&mut t, 4000, 1).unwrap_err(),
        PlayerError::LocalBudget
    );
    t.release_chunk(chunk).unwrap();
    audio(&mut t, 4000, 1).unwrap();
}

#[test]
fn partial_clear_keeps_straddling_whole_chunk_and_trims_replacement_by_rational_endpoint() {
    let mut t = timeline(TimelineLimits::default());
    start(&mut t);
    audio(&mut t, 1000, 480).unwrap();
    audio(&mut t, 11000, 480).unwrap();
    let event = PlayerEvent::Clear {
        server_transmitted_us: 2,
        from_timestamp_us: Some(11000),
        arrival: ReceiveStamp::new(100).unwrap(),
    };
    let result = t
        .apply(
            event,
            binding(),
            true,
            Some(ServerPosition::new(11500, 1).unwrap()),
        )
        .unwrap();
    assert!(matches!(
        result,
        TimelineAction::Cleared(ClearOutcome {
            cutoff: CutoffEvidence::RetainedChunk,
            ..
        })
    ));
    audio(&mut t, 11000, 100).unwrap();
    let kept = t.take_chunk().unwrap();
    assert_eq!(kept.original_frames(), 480);
    assert_eq!(kept.samples().len(), 960);
    t.release_chunk(kept).unwrap();
    let replacement = t.take_chunk().unwrap();
    assert_eq!(replacement.server_timestamp_us(), 11000);
    assert_eq!(replacement.trimmed_frames(), 25);
    assert_eq!(replacement.frames(), 75);
    assert_eq!(
        replacement.position(),
        ServerPosition::new(11000, 25).unwrap()
    );
    t.release_chunk(replacement).unwrap();
    // An earlier chunk crosses the cutoff. Keep all of its samples.
    let mut t = timeline(TimelineLimits::default());
    start(&mut t);
    audio(&mut t, 1000, 480).unwrap();
    let event = PlayerEvent::Clear {
        server_transmitted_us: 2,
        from_timestamp_us: Some(5000),
        arrival: ReceiveStamp::new(100).unwrap(),
    };
    let result = t.apply(event, binding(), true, None).unwrap();
    assert!(matches!(
        result,
        TimelineAction::Cleared(ClearOutcome {
            cutoff: CutoffEvidence::NotRetained,
            ..
        })
    ));
    audio(&mut t, 5000, 480).unwrap();
    let kept = t.take_chunk().unwrap();
    assert_eq!(kept.frames(), 480);
    t.release_chunk(kept).unwrap();
    let replacement = t.take_chunk().unwrap();
    assert_eq!(replacement.trimmed_frames(), 288);
}

#[test]
fn protocol_history_and_allocation_have_independent_exact_release() {
    let mut t = timeline(TimelineLimits::new(34, 2, 2).unwrap());
    start(&mut t);
    audio(&mut t, 1000, 1).unwrap();
    let chunk = t.take_chunk().unwrap();
    t.release_chunk(chunk).unwrap();
    assert_eq!(t.usage().allocated_frames, 0);
    assert_eq!(t.usage().wire_bytes, 17);
    audio(&mut t, 2000, 1).unwrap();
    assert_eq!(
        audio(&mut t, 3000, 1).unwrap_err(),
        PlayerError::LocalBudget
    );
    t.advance_server_time(ServerPosition::new(1020, 0).unwrap(), 0)
        .unwrap();
    assert_eq!(t.usage().wire_bytes, 34);
    t.advance_server_time(ServerPosition::new(1021, 0).unwrap(), 0)
        .unwrap();
    assert_eq!(t.usage().wire_bytes, 17);
    audio(&mut t, 3000, 1).unwrap();
    assert_eq!(t.usage().allocated_frames, 2);
    t.advance_server_time(ServerPosition::new(5000, 0).unwrap(), 0)
        .unwrap();
    assert_eq!(t.usage().wire_bytes, 0);
    assert_eq!(t.usage().allocated_frames, 2);
    assert_eq!(
        audio(&mut t, 6000, 1).unwrap_err(),
        PlayerError::LocalBudget
    );
}

#[test]
fn tiny_chunks_use_cumulative_rational_positions_and_reject_real_overlap() {
    let mut t = timeline(TimelineLimits::default());
    start(&mut t);
    for timestamp in [1000, 1020, 1041, 1062, 1083, 1104] {
        audio(&mut t, timestamp, 1).unwrap();
    }
    assert_eq!(audio(&mut t, 1104, 1).unwrap_err(), PlayerError::Overlap);
    for frame in 0..6 {
        let chunk = t.take_chunk().unwrap();
        assert_eq!(chunk.position(), ServerPosition::new(1000, frame).unwrap());
        t.release_chunk(chunk).unwrap();
    }
}

#[test]
fn zero_frames_unavailable_audio_and_cancellation_stay_bounded() {
    let mut t = timeline(TimelineLimits::new(26, 1, 2).unwrap());
    start(&mut t);
    assert!(matches!(
        audio(&mut t, 1000, 0).unwrap(),
        TimelineAction::NoAudio
    ));
    assert!(t.take_chunk().is_none());
    assert_eq!(t.usage().wire_bytes, 13);
    audio(&mut t, 1000, 0).unwrap();
    assert_eq!(
        audio(&mut t, 1000, 0).unwrap_err(),
        PlayerError::LocalBudget
    );
    t.cancel().unwrap();
    assert_eq!(t.usage(), TimelineUsage::default());
    t.set_participation(true, true).unwrap();
    start(&mut t);
    let event = PlayerEvent::Audio {
        frame: earph_sendspin::PlayerAudioFrame {
            server_timestamp_us: 1000,
            send_ahead_us: 1,
            payload: &[0; 4],
        },
        arrival: ReceiveStamp::new(100).unwrap(),
    };
    assert!(matches!(
        t.apply(event, binding(), false, None).unwrap(),
        TimelineAction::Discarded
    ));
    assert_eq!(t.usage().allocated_frames, 0);
    assert_eq!(t.usage().wire_bytes, 17);
    let generation = t.generation();
    t.set_participation(false, true).unwrap();
    assert_ne!(t.generation(), generation);
    assert_eq!(t.usage(), TimelineUsage::default());
    assert!(TimelineLimits::new(1048577, 1, 1).is_err());
    assert!(TimelineLimits::new(1, 262145, 1).is_err());
    assert!(TimelineLimits::new(1, 1, 513).is_err());
    assert_eq!(
        t.apply(
            PlayerEvent::End {
                arrival: ReceiveStamp::new(100).unwrap()
            },
            PlayerBinding::new(Binding::new([8; 32], 4, 2), 7, 8),
            false,
            None
        )
        .unwrap_err(),
        PlayerError::BindingMismatch
    );
}

#[test]
fn completion_accounting_uses_original_timestamp_after_subframe_grid_alignment() {
    let mut t = timeline(TimelineLimits::default());
    start(&mut t);
    audio(&mut t, 1000, 1).unwrap();
    audio(&mut t, 1020, 1).unwrap();
    for _ in 0..2 {
        let chunk = t.take_chunk().unwrap();
        t.release_chunk(chunk).unwrap();
    }
    // Original completion is 1020 + 20 5/6 us, although render position starts at 1020 5/6.
    t.advance_server_time(ServerPosition::new(1041, 0).unwrap(), 0)
        .unwrap();
    assert_eq!(t.usage(), TimelineUsage::default());
}

#[test]
fn successive_partial_clears_preserve_known_irretractable_audio() {
    let mut t = timeline(TimelineLimits::default());
    start(&mut t);
    audio(&mut t, 1000, 1).unwrap();
    let clear = PlayerEvent::Clear {
        server_transmitted_us: 1,
        from_timestamp_us: Some(1000),
        arrival: ReceiveStamp::new(100).unwrap(),
    };
    t.apply(
        clear,
        binding(),
        true,
        Some(ServerPosition::new(2000, 0).unwrap()),
    )
    .unwrap();
    t.apply(clear, binding(), true, None).unwrap();
    assert_eq!(audio(&mut t, 1000, 1).unwrap(), TimelineAction::NoAudio);
    assert!(t.take_chunk().is_none());
}

#[test]
fn fixed_connection_delay_protects_released_accounting() {
    let mut t = timeline(TimelineLimits::default());
    start(&mut t);
    audio(&mut t, 1000, 1).unwrap();
    t.advance_server_time(ServerPosition::new(500, 0).unwrap(), 100)
        .unwrap();
    assert!(
        t.advance_server_time(ServerPosition::new(600, 0).unwrap(), 0)
            .is_err()
    );
}

#[test]
fn server_clock_order_protects_released_accounting() {
    let mut t = timeline(TimelineLimits::default());
    start(&mut t);
    audio(&mut t, 1000, 1).unwrap();
    t.advance_server_time(ServerPosition::new(500, 0).unwrap(), 100)
        .unwrap();
    assert!(
        t.advance_server_time(ServerPosition::new(499, 0).unwrap(), 100)
            .is_err()
    );
}

#[test]
fn independent_descriptor_and_sample_limits_refuse_without_consuming_other_credits() {
    let mut t = timeline(TimelineLimits::new(1000, 10, 1).unwrap());
    start(&mut t);
    audio(&mut t, 1000, 1).unwrap();
    let usage = t.usage();
    assert_eq!(
        audio(&mut t, 2000, 1).unwrap_err(),
        PlayerError::LocalBudget
    );
    assert_eq!(t.usage(), usage);
    let mut t = timeline(TimelineLimits::new(1000, 1, 10).unwrap());
    start(&mut t);
    audio(&mut t, 1000, 1).unwrap();
    let usage = t.usage();
    assert_eq!(
        audio(&mut t, 2000, 1).unwrap_err(),
        PlayerError::LocalBudget
    );
    assert_eq!(t.usage(), usage);
    let chunk = t.take_chunk().unwrap();
    t.release_chunk(chunk).unwrap();
    audio(&mut t, 2000, 1).unwrap();
    assert_eq!(t.usage().wire_bytes, 34);
    assert_eq!(
        ServerPosition::new(MAX_TIME_US, 1).unwrap_err(),
        PlayerError::NumericRange
    );
}

#[test]
fn partial_clear_revokes_selected_transferred_chunks_but_keeps_prior_chunks_current() {
    let mut t = timeline(TimelineLimits::default());
    start(&mut t);
    audio(&mut t, 1000, 10).unwrap();
    let prior = t.take_chunk().unwrap();
    audio(&mut t, 2000, 10).unwrap();
    let removed = t.take_chunk().unwrap();
    deliver(
        &mut t,
        "stream/clear",
        json!({"server_transmitted":1,"from_timestamp":2000}),
    )
    .unwrap();
    assert!(t.is_current(&prior));
    assert!(!t.is_current(&removed));
    assert_eq!(t.usage().allocated_frames, 20);
    assert_eq!(t.usage().wire_bytes, 53);
    t.release_chunk(removed).unwrap();
    t.release_chunk(prior).unwrap();
    assert_eq!(t.usage().allocated_frames, 0);
    assert_eq!(t.usage().wire_bytes, 53);
    t.advance_server_time(ServerPosition::new(2000, 0).unwrap(), 0)
        .unwrap();
    assert_eq!(t.usage(), TimelineUsage::default());
}

#[test]
fn descriptor_storage_is_bounded_and_observable_for_downstream_preparation() {
    let mut t = timeline(TimelineLimits::new(1000, 10, 2).unwrap());
    let metadata = t.metadata_capacity_bytes();
    assert!(metadata > 0);
    start(&mut t);
    audio(&mut t, 1000, 1).unwrap();
    audio(&mut t, 2000, 1).unwrap();
    assert_eq!(t.metadata_capacity_bytes(), metadata);
    t.cancel().unwrap();
    assert_eq!(t.metadata_capacity_bytes(), metadata);
}

#[test]
fn zero_frames_beyond_clear_floor_cannot_erase_irretractable_audio() {
    let mut t = timeline(TimelineLimits::new(1000, 10, 3).unwrap());
    start(&mut t);
    audio(&mut t, 1000, 1).unwrap();
    let clear = PlayerEvent::Clear {
        server_transmitted_us: 1,
        from_timestamp_us: Some(1000),
        arrival: ReceiveStamp::new(100).unwrap(),
    };
    t.apply(
        clear,
        binding(),
        true,
        Some(ServerPosition::new(2000, 0).unwrap()),
    )
    .unwrap();
    assert_eq!(audio(&mut t, 3000, 0).unwrap(), TimelineAction::NoAudio);
    assert_eq!(t.usage().wire_bytes, 13);
    assert_eq!(t.usage().allocated_frames, 0);
    assert_eq!(audio(&mut t, 1000, 1).unwrap(), TimelineAction::NoAudio);
    assert!(t.take_chunk().is_none());
    assert_eq!(t.usage().wire_bytes, 30);
    audio(&mut t, 3000, 0).unwrap();
    assert_eq!(
        audio(&mut t, 3000, 0).unwrap_err(),
        PlayerError::LocalBudget
    );
}

#[test]
fn owned_samples_move_once_while_the_chunk_keeps_metadata_and_release_credits() {
    let mut t = timeline(TimelineLimits::new(1000, 10, 3).unwrap());
    start(&mut t);
    audio(&mut t, 1000, 2).unwrap();
    let mut chunk = t.take_chunk().unwrap();
    let pointer = chunk.samples().as_ptr();
    let usage = t.usage();
    let position = chunk.position();
    let end = chunk.end_position();
    let revision = chunk.format_revision();
    let storage = chunk.take_samples().unwrap();
    assert_eq!(storage.as_ptr(), pointer);
    assert_eq!(storage, &[i16::MIN, i16::MAX, i16::MIN, i16::MAX]);
    assert!(chunk.take_samples().is_none());
    assert!(chunk.samples().is_empty());
    assert_eq!(chunk.frames(), 2);
    assert_eq!(chunk.original_frames(), 2);
    assert_eq!(chunk.trimmed_frames(), 0);
    assert_eq!(chunk.position(), position);
    assert_eq!(chunk.end_position(), end);
    assert_eq!(chunk.format_revision(), revision);
    assert_eq!(chunk.server_timestamp_us(), 1000);
    assert_eq!(chunk.wire_bytes(), 21);
    assert_eq!(chunk.arrival().microseconds(), 100);
    assert_eq!(t.usage(), usage);
    assert!(t.is_current(&chunk));
    let foreign = PlayerTimeline::new(
        PlayerBinding::new(binding().clock(), 7, 9),
        TimelineLimits::default(),
    );
    assert!(!foreign.is_current(&chunk));
    t.advance_server_time(ServerPosition::new(2000, 0).unwrap(), 0)
        .unwrap();
    assert_eq!(t.usage().wire_bytes, 0);
    assert_eq!(t.usage().allocated_frames, 2);
    drop(storage);
    t.release_chunk(chunk).unwrap();
    assert_eq!(t.usage(), TimelineUsage::default());
}

#[test]
fn moved_storage_keeps_credits_and_identity_through_partial_full_clear_and_cancel() {
    for operation in ["partial", "full", "cancel"] {
        let mut t = timeline(TimelineLimits::new(1000, 2, 3).unwrap());
        start(&mut t);
        audio(&mut t, 1000, 2).unwrap();
        let mut chunk = t.take_chunk().unwrap();
        let storage = chunk.take_samples().unwrap();
        let old_binding = chunk.binding();
        let old_generation = chunk.generation();
        match operation {
            "partial" => {
                deliver(
                    &mut t,
                    "stream/clear",
                    json!({"server_transmitted":1,"from_timestamp":1000}),
                )
                .unwrap();
            }
            "full" => {
                deliver(&mut t, "stream/clear", json!({"server_transmitted":1})).unwrap();
            }
            _ => {
                t.cancel().unwrap();
            }
        }
        assert!(!t.is_current(&chunk));
        assert_eq!(chunk.binding(), old_binding);
        assert_eq!(chunk.generation(), old_generation);
        assert_ne!(t.generation(), old_generation);
        assert_eq!(t.usage().allocated_frames, 2);
        assert_eq!(t.usage().allocated_bytes, storage.capacity() * 2);
        assert_eq!(t.usage().wire_bytes, 0);
        assert_eq!(t.usage().descriptors, 1);
        if operation == "cancel" {
            t.set_participation(true, true).unwrap();
            start(&mut t);
        }
        assert_eq!(
            audio(&mut t, 3000, 1).unwrap_err(),
            PlayerError::LocalBudget
        );
        drop(storage);
        t.release_chunk(chunk).unwrap();
        assert_eq!(t.usage(), TimelineUsage::default());
        audio(&mut t, 3000, 1).unwrap();
    }
}
