// ABOUTME: Invokes the real SDK callback ABI against owned AudioBufferList memory.
// ABOUTME: Proves delivery and safe silence without registering or starting hardware.
use super::*;
use objc2_core_audio_types::AudioBuffer;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    static HEAP: Cell<(bool, usize, usize)> = const { Cell::new((false,0,0)) };
}
struct TrackedAllocator;
// SAFETY: The allocator delegates every pointer and layout unchanged to System.
unsafe impl GlobalAlloc for TrackedAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = HEAP.try_with(|heap| {
            let (enabled, allocations, frees) = heap.get();
            heap.set((enabled, allocations + usize::from(enabled), frees));
        });
        // SAFETY: Preserve the caller's valid allocation layout.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        let _ = HEAP.try_with(|heap| {
            let (enabled, allocations, frees) = heap.get();
            heap.set((enabled, allocations, frees + usize::from(enabled)));
        });
        // SAFETY: Forward the original pointer and layout to its owning allocator.
        unsafe { System.dealloc(pointer, layout) }
    }
}
#[global_allocator]
static ALLOCATOR: TrackedAllocator = TrackedAllocator;

fn without_heap(operation: impl FnOnce()) {
    HEAP.with(|heap| heap.set((true, 0, 0)));
    operation();
    let (_, allocations, frees) = HEAP.with(|heap| heap.replace((false, 0, 0)));
    assert_eq!(
        (allocations, frees),
        (0, 0),
        "callback allocated or freed storage"
    );
}

fn call(context: &mut Context, output: &mut AudioBufferList) {
    let mut stamp: AudioTimeStamp = unsafe { std::mem::zeroed() };
    let mut input = AudioBufferList {
        mNumberBuffers: 0,
        mBuffers: [AudioBuffer {
            mNumberChannels: 0,
            mDataByteSize: 0,
            mData: std::ptr::null_mut(),
        }],
    };
    // SAFETY: Each pointer addresses owned SDK storage for this synchronous callback invocation.
    without_heap(|| {
        // SAFETY: Every pointer targets owned SDK memory until this callback returns.
        unsafe {
            callback(
                1,
                NonNull::from(&mut stamp),
                NonNull::from(&mut input),
                NonNull::from(&mut stamp),
                NonNull::from(output),
                NonNull::from(&mut stamp),
                std::ptr::from_mut(context).cast(),
            );
        }
    });
}
#[test]
fn sdk_callback_delivers_owned_frames_and_silences_after_underflow() {
    let mut producer = super::super::tests::producer(PlaybackRoute::StrictInteger);
    producer
        .write(&super::super::tests::block(0, &[1, -1, 2, -2]), 0, 2)
        .unwrap();
    let mut context = Context {
        ring: producer.ring.clone(),
        channels: 2,
    };
    context.ring.enabled.store(true, Ordering::Release);
    let mut bytes = [255_u8; 12];
    let mut output = Box::new(AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: 2,
            mDataByteSize: 12,
            mData: bytes.as_mut_ptr().cast(),
        }],
    });
    call(&mut context, output.as_mut());
    assert_eq!(bytes, [1, 0, 0, 255, 255, 255, 2, 0, 0, 254, 255, 255]);
    call(&mut context, output.as_mut());
    assert_eq!(bytes, [0; 12]);
    assert_eq!(producer.ring.consumed.load(Ordering::Acquire), 2);
    assert_ne!(producer.ring.fault.load(Ordering::Acquire), 0);
}
#[test]
fn sdk_callback_rejects_planar_or_changed_channels_with_silence() {
    let producer = super::super::tests::producer(PlaybackRoute::StrictInteger);
    let mut context = Context {
        ring: producer.ring.clone(),
        channels: 2,
    };
    context.ring.enabled.store(true, Ordering::Release);
    let mut bytes = [255_u8; 12];
    let mut output = Box::new(AudioBufferList {
        mNumberBuffers: 1,
        mBuffers: [AudioBuffer {
            mNumberChannels: 1,
            mDataByteSize: 12,
            mData: bytes.as_mut_ptr().cast(),
        }],
    });
    call(&mut context, output.as_mut());
    assert_eq!(bytes, [0; 12]);
    assert_ne!(producer.ring.fault.load(Ordering::Acquire), 0);
}

#[test]
fn observed_device_changes_disable_callbacks_and_reject_future_submission() {
    let mut producer = super::super::tests::producer(PlaybackRoute::StrictInteger);
    let context = Context {
        ring: producer.ring.clone(),
        channels: 2,
    };
    context.ring.enabled.store(true, Ordering::Release);
    let mut address = ca::AudioObjectPropertyAddress {
        mSelector: ca::kAudioDevicePropertyNominalSampleRate,
        mScope: ca::kAudioObjectPropertyScopeGlobal,
        mElement: ca::kAudioObjectPropertyElementMain,
    };
    // SAFETY: Owned notification address and context remain live during the synchronous invocation.
    unsafe {
        changed(
            1,
            1,
            NonNull::from(&mut address),
            std::ptr::from_ref(context.ring.fault).cast_mut().cast(),
        );
    }
    assert_ne!(context.ring.fault.load(Ordering::Acquire), 0);
    assert_eq!(
        producer
            .write(&super::super::tests::block(0, &[1, 2]), 0, 1)
            .unwrap_err()
            .code,
        ErrorCode::OutputFault
    );
}

#[test]
fn late_notification_outlives_freed_ring_and_cannot_fault_a_new_generation() {
    let producer = super::super::tests::producer(PlaybackRoute::StrictInteger);
    let weak_ring = Arc::downgrade(&producer.ring);
    let retired = producer.ring.fault;
    let pointer = std::ptr::from_ref(retired).cast_mut().cast();
    drop(producer);
    assert!(
        weak_ring.upgrade().is_none(),
        "retired listener retained audio storage"
    );
    let mut current = super::super::tests::producer(PlaybackRoute::StrictInteger);
    let mut address = ca::AudioObjectPropertyAddress {
        mSelector: ca::kAudioDevicePropertyNominalSampleRate,
        mScope: ca::kAudioObjectPropertyScopeGlobal,
        mElement: ca::kAudioObjectPropertyElementMain,
    };
    without_heap(|| {
        // SAFETY: The retired listener references a static slot after all owned audio storage was freed.
        unsafe {
            changed(1, 1, NonNull::from(&mut address), pointer);
        }
    });
    assert_eq!(retired.load(Ordering::Acquire), 1);
    assert_eq!(current.ring.fault.load(Ordering::Acquire), 0);
    current
        .write(&super::super::tests::block(0, &[1, 2]), 0, 1)
        .unwrap();
}
