// ABOUTME: Tests bounded local queue edits through the public domain API.
// ABOUTME: Covers stable identities, atomic failures, selection, and preparation results.
use earph_core::queue::{LocalQueue, PreparationStatus};

#[test]
fn append_preserves_order_and_unicode_paths_without_selecting() {
    let mut queue = LocalQueue::default();
    let first = queue.append("/music/雪.flac").unwrap();
    let second = queue.append("/music/live.wav").unwrap();
    assert_ne!(first, second);
    assert_eq!(queue.entries()[0].id, first);
    assert_eq!(queue.entries()[0].path, "/music/雪.flac");
    assert_eq!(queue.entries()[0].status, PreparationStatus::Unprepared);
    assert_eq!(queue.entries()[1].id, second);
    assert_eq!(queue.entries()[1].path, "/music/live.wav");
    assert_eq!(queue.selected(), None);
}

#[test]
fn bad_paths_leave_existing_entries_unchanged() {
    let mut queue = LocalQueue::default();
    queue.append("/music/kept.wav").unwrap();
    let before = queue.entries().to_vec();
    for path in ["".to_owned(), "bad\0path".to_owned(), "é".repeat(513)] {
        assert_eq!(
            queue.append(path).unwrap_err().code,
            earph_core::ErrorCode::AssetInvalid
        );
        assert_eq!(queue.entries(), before);
    }
    queue.append("é".repeat(512)).unwrap();
    assert_eq!(queue.entries()[1].path.len(), 1024);
}

#[test]
fn capacity_rejects_the_next_entry_without_mutation() {
    let mut queue = LocalQueue::default();
    for index in 0..256 {
        queue.append(format!("/music/{index}.flac")).unwrap();
    }
    let before = queue.entries().to_vec();
    assert_eq!(
        queue.append("/music/overflow.flac").unwrap_err().code,
        earph_core::ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(queue.entries(), before);
}

#[test]
fn reorder_keeps_selection_attached_to_identity() {
    let mut queue = LocalQueue::default();
    let first = queue.append("first.wav").unwrap();
    let second = queue.append("second.wav").unwrap();
    let third = queue.append("third.wav").unwrap();
    queue.select(Some(second.clone())).unwrap();
    queue.move_entry(&second, 0).unwrap();
    assert_eq!(
        queue
            .entries()
            .iter()
            .map(|entry| &entry.id)
            .collect::<Vec<_>>(),
        [&second, &first, &third]
    );
    assert_eq!(queue.selected(), Some(&second));
    queue.move_entry(&second, 2).unwrap();
    assert_eq!(
        queue
            .entries()
            .iter()
            .map(|entry| &entry.id)
            .collect::<Vec<_>>(),
        [&first, &third, &second]
    );
    assert_eq!(queue.selected(), Some(&second));
    queue.move_entry(&second, 2).unwrap();
    queue.select(None).unwrap();
    assert_eq!(queue.selected(), None);
}

#[test]
fn invalid_edits_leave_entries_and_selection_unchanged() {
    use earph_core::{ErrorCode, ids::QueueEntryId};
    let mut queue = LocalQueue::default();
    let first = queue.append("first.wav").unwrap();
    queue.append("second.wav").unwrap();
    queue.select(Some(first.clone())).unwrap();
    let missing = QueueEntryId::new("missing").unwrap();
    let before = queue.entries().to_vec();
    assert_eq!(
        queue.move_entry(&first, 2).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(
        queue.move_entry(&first, usize::MAX).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(
        queue.move_entry(&missing, 0).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(
        queue.remove(&missing).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(
        queue.select(Some(missing)).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(queue.entries(), before);
    assert_eq!(queue.selected(), Some(&first));
}

#[test]
fn remove_clears_only_selected_entry_and_never_reuses_ids() {
    let mut queue = LocalQueue::default();
    let first = queue.append("first.wav").unwrap();
    let selected = queue.append("selected.wav").unwrap();
    queue.select(Some(selected.clone())).unwrap();
    queue.remove(&first).unwrap();
    assert_eq!(queue.selected(), Some(&selected));
    queue.remove(&selected).unwrap();
    assert!(queue.entries().is_empty());
    assert_eq!(queue.selected(), None);
    let replacement = queue.append("selected.wav").unwrap();
    assert_ne!(replacement, selected);
    assert_ne!(replacement, first);
    assert!(queue.select(Some(selected)).is_err());
    assert_eq!(queue.selected(), None);
}

#[test]
fn preparation_results_stay_with_entries_and_removed_work_is_rejected() {
    use earph_core::{
        ErrorCode,
        audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    };
    let mut queue = LocalQueue::default();
    let ready = queue.append("ready.wav").unwrap();
    let failed = queue.append("failed.wav").unwrap();
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed24).unwrap();
    let status = PreparationStatus::Ready {
        format,
        total_frames: 64,
    };
    queue.update_status(&ready, status).unwrap();
    queue
        .update_status(
            &failed,
            PreparationStatus::Failed {
                code: ErrorCode::FormatUnsupported,
            },
        )
        .unwrap();
    queue.move_entry(&ready, 1).unwrap();
    assert_eq!(
        queue.entries()[0].status,
        PreparationStatus::Failed {
            code: ErrorCode::FormatUnsupported
        }
    );
    assert_eq!(queue.entries()[1].status, status);
    queue.remove(&ready).unwrap();
    let replacement = queue.append("ready.wav").unwrap();
    let before = queue.entries().to_vec();
    assert_eq!(
        queue.update_status(&ready, status).unwrap_err().code,
        ErrorCode::AssetInvalid
    );
    assert_eq!(queue.entries(), before);
    assert_eq!(queue.entries()[1].id, replacement);
    assert_eq!(queue.entries()[1].status, PreparationStatus::Unprepared);
}
