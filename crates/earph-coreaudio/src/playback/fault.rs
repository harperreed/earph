// ABOUTME: Publishes the first native fault and bounded frame facts in one atomic word.
// ABOUTME: Keeps callback diagnostics allocation-free and immune to later cleanup or listener faults.
use serde::Serialize;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum OutputFaultReason {
    Unknown = 1,
    DevicePropertyChanged = 2,
    DeviceIdentityChanged = 3,
    DeviceConfigurationChanged = 4,
    CallbackBufferCount = 5,
    CallbackBufferSize = 6,
    CallbackBufferChannels = 7,
    CallbackBufferPointer = 8,
    CallbackReentry = 9,
    RingReentry = 10,
    RingCursorInvalid = 11,
    RingUnderrun = 12,
    NativeListenerRegistration = 13,
    NativeStart = 14,
    NativeStop = 15,
    NativeListenerRemoval = 16,
    NativeDestruction = 17,
    NativePresentation = 18,
    NativeClose = 19,
}
impl OutputFaultReason {
    fn decode(code: u8) -> Self {
        match code {
            2 => Self::DevicePropertyChanged,
            3 => Self::DeviceIdentityChanged,
            4 => Self::DeviceConfigurationChanged,
            5 => Self::CallbackBufferCount,
            6 => Self::CallbackBufferSize,
            7 => Self::CallbackBufferChannels,
            8 => Self::CallbackBufferPointer,
            9 => Self::CallbackReentry,
            10 => Self::RingReentry,
            11 => Self::RingCursorInvalid,
            12 => Self::RingUnderrun,
            13 => Self::NativeListenerRegistration,
            14 => Self::NativeStart,
            15 => Self::NativeStop,
            16 => Self::NativeListenerRemoval,
            17 => Self::NativeDestruction,
            18 => Self::NativePresentation,
            19 => Self::NativeClose,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OutputFaultDetail {
    pub reason: OutputFaultReason,
    pub requested_frames: Option<u32>,
    pub queued_frames: Option<u32>,
}

const REQUEST_SHIFT: u32 = 8;
const REQUEST_BITS: u32 = 14;
const QUEUE_SHIFT: u32 = REQUEST_SHIFT + REQUEST_BITS;
const QUEUE_BITS: u32 = 17;
const RESERVED_SHIFT: u32 = QUEUE_SHIFT + QUEUE_BITS;
const REQUEST_LIMIT: u64 = 8192;
const QUEUE_LIMIT: u64 = 65536;

#[derive(Debug)]
pub(super) struct FaultSignal(pub(super) AtomicU64);
impl FaultSignal {
    #[cfg(any(target_os = "macos", test))]
    pub(super) const fn new() -> Self {
        Self(AtomicU64::new(0))
    }
    pub(super) fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire) != 0
    }
    #[cfg(any(target_os = "macos", test))]
    pub(super) fn latch(
        &self,
        reason: OutputFaultReason,
        requested_frames: Option<u64>,
        queued_frames: Option<u64>,
    ) {
        let encode = |value: Option<u64>, limit| {
            value
                .filter(|value| *value <= limit)
                .map_or(0, |value| value + 1)
        };
        let word = reason as u64
            | (encode(requested_frames, REQUEST_LIMIT) << REQUEST_SHIFT)
            | (encode(queued_frames, QUEUE_LIMIT) << QUEUE_SHIFT);
        let _ = self
            .0
            .compare_exchange(0, word, Ordering::Release, Ordering::Relaxed);
    }
    pub(super) fn detail(&self) -> Option<OutputFaultDetail> {
        let word = self.0.load(Ordering::Acquire);
        if word == 0 {
            return None;
        }
        let decode = |shift: u32, bits: u32, limit: u64| {
            let count = (word >> shift) & ((1_u64 << bits) - 1);
            count
                .checked_sub(1)
                .filter(|count| *count <= limit)
                .and_then(|count| u32::try_from(count).ok())
        };
        Some(OutputFaultDetail {
            reason: if word >> RESERVED_SHIFT != 0 {
                OutputFaultReason::Unknown
            } else {
                OutputFaultReason::decode(word as u8)
            },
            requested_frames: decode(REQUEST_SHIFT, REQUEST_BITS, REQUEST_LIMIT),
            queued_frames: decode(QUEUE_SHIFT, QUEUE_BITS, QUEUE_LIMIT),
        })
    }
}
