// ABOUTME: Measures thread-local allocations and frees in prepared processing tests.
// ABOUTME: Delegates to the system allocator and restores tracking after each check.
use std::{
    alloc::{GlobalAlloc, Layout, System},
    cell::Cell,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct AllocationCounts {
    pub allocations: usize,
    pub frees: usize,
}
thread_local! {
    static TRACKING: Cell<bool> = const { Cell::new(false) };
    static COUNTS: Cell<AllocationCounts> = const { Cell::new(AllocationCounts { allocations: 0, frees: 0 }) };
}
struct TrackingAllocator;
// SAFETY: Each operation delegates to System with the caller's original pointer and layout.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count_allocation(false);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        count_allocation(true);
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        count_allocation(false);
        count_allocation(true);
        unsafe { System.realloc(pointer, layout, size) }
    }
}
fn count_allocation(free: bool) {
    if TRACKING.try_with(Cell::get).unwrap_or(false) {
        let _ = COUNTS.try_with(|cell| {
            let mut counts = cell.get();
            if free {
                counts.frees += 1;
            } else {
                counts.allocations += 1;
            }
            cell.set(counts);
        });
    }
}
#[global_allocator]
static ALLOCATOR: TrackingAllocator = TrackingAllocator;
struct TrackingGuard;
impl Drop for TrackingGuard {
    fn drop(&mut self) {
        TRACKING.with(|active| active.set(false));
    }
}
pub fn tracked<T>(operation: impl FnOnce() -> T) -> (T, AllocationCounts) {
    COUNTS.with(|counts| counts.set(AllocationCounts::default()));
    TRACKING.with(|active| active.set(true));
    let guard = TrackingGuard;
    let result = operation();
    drop(guard);
    (result, COUNTS.with(Cell::get))
}
