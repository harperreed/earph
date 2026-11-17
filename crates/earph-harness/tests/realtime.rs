// ABOUTME: Counts allocations and frees only inside prepared processing.
// ABOUTME: Exercises real virtual-output paths without mocking the allocator.
use earph_core::{audio::MemoryBudget, ids::Epochs};
use earph_harness::{fixture::qualification_specs, render::*};
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
    sync::atomic::{AtomicUsize, Ordering},
};

thread_local! {static ACTIVE:Cell<bool>=const {Cell::new(false)};}
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static FREES: AtomicUsize = AtomicUsize::new(0);
struct TrackingAllocator;
// SAFETY: Every operation delegates to System with the caller's original layout.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        if ACTIVE.try_with(Cell::get).unwrap_or(false) {
            FREES.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if ACTIVE.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            FREES.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.realloc(pointer, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;

#[test]
fn prepared_processing_allocates_and_frees_nothing_even_on_faults() {
    for fault in [
        None,
        Some(FaultKind::ShortWrite { max_frames: 1 }),
        Some(FaultKind::StaleEpoch),
        Some(FaultKind::MalformedChunk),
        Some(FaultKind::BackendFailure),
    ] {
        let mut schedule = FaultSchedule::default();
        if let Some(fault) = fault {
            schedule.faults.push(ScheduledFault {
                at_frame: 64,
                fault,
            });
        }
        let input = RenderRequest {
            fixture: qualification_specs().pop().unwrap(),
            block_frames: 256,
            budget: MemoryBudget::default(),
            epochs: Epochs::default(),
            faults: schedule,
        };
        let mut prepared = PreparedRender::prepare(input).unwrap();
        let allocations = ALLOCATIONS.load(Ordering::Relaxed);
        let frees = FREES.load(Ordering::Relaxed);
        ACTIVE.with(|active| active.set(true));
        let result = prepared.process();
        ACTIVE.with(|active| active.set(false));
        assert_eq!(ALLOCATIONS.load(Ordering::Relaxed) - allocations, 0);
        assert_eq!(FREES.load(Ordering::Relaxed) - frees, 0);
        if matches!(fault, None | Some(FaultKind::ShortWrite { .. })) {
            assert!(result.is_ok());
        } else {
            assert!(result.is_err());
        }
    }
}
