// ABOUTME: Shares HTTP progress credits and fixed event storage inside one adapter ledger.
// ABOUTME: Keeps concrete slot and sender ownership attached to Core subscription leases.
use crate::PolicyError;
use earph_core::renderer_events::{RendererService, RendererSubscriptionLease};
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
pub const ADAPTER_MEMORY_BYTES: usize = 8 * 1024 * 1024;
pub(super) const BASE_RESERVATION: usize = 256 * 1024;
pub(super) const REQUEST_RESERVATION: usize = (ADAPTER_MEMORY_BYTES - BASE_RESERVATION) / 4;
pub(super) const SENDER_RESERVATION: usize = 128 * 1024;
pub(super) const SENDER_BUFFER: usize = 80 * 1024;
static NEXT_LEASE: AtomicU64 = AtomicU64::new(1);
pub(super) struct Budget {
    state: Mutex<State>,
    pub active: AtomicUsize,
    pub queued: AtomicUsize,
}
struct State {
    requests: usize,
    ordinary: usize,
    event_bytes: usize,
    events: usize,
    leases: Vec<(u64, Weak<EventLease>)>,
    pool: Option<Arc<ScratchPool>>,
}
impl State {
    fn reserved(&self) -> usize {
        BASE_RESERVATION
            + self.event_bytes
            + if self.events > 0 {
                4 * SENDER_RESERVATION
                    + self.requests.max(self.ordinary + 1).max(2) * REQUEST_RESERVATION
            } else {
                self.requests * REQUEST_RESERVATION
            }
    }
}
impl Budget {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State {
                requests: 0,
                ordinary: 0,
                event_bytes: 0,
                events: 0,
                leases: Vec::with_capacity(16),
                pool: None,
            }),
            active: AtomicUsize::new(0),
            queued: AtomicUsize::new(0),
        })
    }
    pub fn reserved(&self) -> usize {
        self.state.lock().unwrap().reserved()
    }
    pub fn request(self: &Arc<Self>) -> Option<Arc<Lease>> {
        let mut s = self.state.lock().unwrap();
        s.requests += 1;
        if s.requests > 4 || s.reserved() > ADAPTER_MEMORY_BYTES {
            s.requests -= 1;
            return None;
        }
        Some(Arc::new(Lease {
            budget: self.clone(),
            ordinary: AtomicBool::new(false),
        }))
    }
    pub fn reserve_event(
        self: &Arc<Self>,
        service: RendererService,
    ) -> Result<Arc<EventLease>, PolicyError> {
        let mut s = self.state.lock().unwrap();
        let bytes = 2 * service.event_slot_bytes();
        let candidate = BASE_RESERVATION
            + s.event_bytes
            + bytes
            + 4 * SENDER_RESERVATION
            + s.requests.max(s.ordinary + 1).max(2) * REQUEST_RESERVATION;
        if s.events >= 16 || candidate > ADAPTER_MEMORY_BYTES {
            return Err(PolicyError::Budget);
        }
        let id = NEXT_LEASE
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| n.checked_add(1))
            .map_err(|_| PolicyError::Budget)?;
        let mut slots = Vec::with_capacity(2);
        for _ in 0..2 {
            let mut slot = String::new();
            slot.try_reserve_exact(service.event_slot_bytes())
                .map_err(|_| PolicyError::Budget)?;
            slots.push(slot);
        }
        let capacities = (slots[0].capacity(), slots[1].capacity());
        if capacities != (service.event_slot_bytes(), service.event_slot_bytes()) {
            return Err(PolicyError::Budget);
        }
        let pool = if let Some(pool) = &s.pool {
            pool.clone()
        } else {
            let make = || -> Result<_, PolicyError> {
                let mut bytes = Vec::new();
                bytes
                    .try_reserve_exact(SENDER_BUFFER)
                    .map_err(|_| PolicyError::Budget)?;
                if bytes.capacity() != SENDER_BUFFER {
                    return Err(PolicyError::Budget);
                }
                Ok(Mutex::new(Some(bytes)))
            };
            Arc::new(ScratchPool {
                slots: [make()?, make()?, make()?, make()?],
            })
        };
        let lease = Arc::new(EventLease {
            id,
            service,
            capacities,
            slots: Mutex::new(slots),
            budget: self.clone(),
            pool: pool.clone(),
        });
        s.leases.retain(|(_, w)| w.strong_count() > 0);
        s.leases.push((id, Arc::downgrade(&lease)));
        s.pool = Some(pool);
        s.events += 1;
        s.event_bytes += bytes;
        Ok(lease)
    }
    pub fn event(&self, id: u64) -> Option<Arc<EventLease>> {
        let weak = self
            .state
            .lock()
            .unwrap()
            .leases
            .iter()
            .find(|(key, _)| *key == id)
            .map(|(_, weak)| weak.clone());
        weak.and_then(|w| w.upgrade())
    }
}
pub(super) struct Lease {
    pub budget: Arc<Budget>,
    ordinary: AtomicBool,
}
impl Lease {
    pub fn admit_ordinary(&self) -> bool {
        let mut s = self.budget.state.lock().unwrap();
        if self.ordinary.load(Ordering::Acquire) {
            return true;
        }
        let limit = if s.events == 0 {
            3
        } else {
            ((ADAPTER_MEMORY_BYTES - BASE_RESERVATION - s.event_bytes - 4 * SENDER_RESERVATION)
                / REQUEST_RESERVATION)
                .saturating_sub(1)
        };
        if s.ordinary >= limit {
            return false;
        }
        s.ordinary += 1;
        self.ordinary.store(true, Ordering::Release);
        true
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        let mut s = self.budget.state.lock().unwrap();
        s.requests -= 1;
        if self.ordinary.load(Ordering::Acquire) {
            s.ordinary -= 1;
        }
    }
}
pub struct EventLease {
    id: u64,
    service: RendererService,
    capacities: (usize, usize),
    slots: Mutex<Vec<String>>,
    budget: Arc<Budget>,
    pub(super) pool: Arc<ScratchPool>,
}
impl RendererSubscriptionLease for EventLease {
    fn lease_id(&self) -> u64 {
        self.id
    }
    fn service(&self) -> RendererService {
        self.service
    }
    fn event_slot_bytes(&self) -> (usize, usize) {
        self.capacities
    }
}
impl EventLease {
    pub(super) fn take(self: &Arc<Self>) -> Result<EventBody, PolicyError> {
        let value = self
            .slots
            .lock()
            .unwrap()
            .pop()
            .ok_or(PolicyError::Budget)?;
        Ok(EventBody {
            value,
            lease: self.clone(),
        })
    }
}
impl Drop for EventLease {
    fn drop(&mut self) {
        let mut s = self.budget.state.lock().unwrap();
        s.events -= 1;
        s.event_bytes -= 2 * self.service.event_slot_bytes();
        if s.events == 0 {
            s.pool = None;
        }
    }
}
pub(super) struct EventBody {
    pub value: String,
    lease: Arc<EventLease>,
}
impl Drop for EventBody {
    fn drop(&mut self) {
        self.value.clear();
        self.lease
            .slots
            .lock()
            .unwrap()
            .push(std::mem::take(&mut self.value));
    }
}
pub(super) struct ScratchPool {
    slots: [Mutex<Option<Vec<u8>>>; 4],
}
impl ScratchPool {
    pub fn take(self: &Arc<Self>, position: u8) -> Result<Scratch, PolicyError> {
        let index = usize::from(position);
        let value = self
            .slots
            .get(index)
            .ok_or(PolicyError::Budget)?
            .lock()
            .unwrap()
            .take()
            .ok_or(PolicyError::Budget)?;
        Ok(Scratch {
            value,
            index,
            pool: self.clone(),
        })
    }
}
pub(super) struct Scratch {
    pub value: Vec<u8>,
    index: usize,
    pool: Arc<ScratchPool>,
}
impl Drop for Scratch {
    fn drop(&mut self) {
        self.value.clear();
        *self.pool.slots[self.index].lock().unwrap() = Some(std::mem::take(&mut self.value));
    }
}
pub(super) struct Active(pub Arc<Budget>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shared_storage_reserves_progress_and_refuses_seventh_full_sid() {
        let budget = Budget::new();
        let mut leases = Vec::new();
        for _ in 0..6 {
            leases.push(budget.reserve_event(RendererService::AvTransport).unwrap());
        }
        assert_eq!(budget.reserved(), 7_995_392);
        assert!(matches!(
            budget.reserve_event(RendererService::AvTransport),
            Err(PolicyError::Budget)
        ));
        let one = budget.request().unwrap();
        let two = budget.request().unwrap();
        assert!(budget.request().is_none());
        assert_eq!(budget.reserved(), 7_995_392);
        assert!(one.admit_ordinary());
        assert!(!two.admit_ordinary());
        let slot_capacities: Vec<_> = leases
            .iter()
            .flat_map(|l| {
                l.slots
                    .lock()
                    .unwrap()
                    .iter()
                    .map(String::capacity)
                    .collect::<Vec<_>>()
            })
            .collect();
        let sender_capacities: Vec<_> = leases[0]
            .pool
            .slots
            .iter()
            .map(|s| s.lock().unwrap().as_ref().unwrap().capacity())
            .collect();
        assert_eq!(slot_capacities.iter().sum::<usize>(), 3_145_728);
        assert_eq!(sender_capacities, vec![81920; 4]);
        println!(
            "RESOURCE_ACCOUNTING {{\"reserved\":{},\"event_slots\":{:?},\"sender_buffers\":{:?},\"sender_transient_allowance\":{},\"request_credits\":2,\"resident\":{}}}",
            budget.reserved(),
            slot_capacities,
            sender_capacities,
            4 * (SENDER_RESERVATION - SENDER_BUFFER),
            BASE_RESERVATION
        );
        drop(leases);
        assert_eq!(
            budget.reserved(),
            BASE_RESERVATION + 2 * REQUEST_RESERVATION
        );
        drop(one);
        drop(two);
        assert_eq!(budget.reserved(), BASE_RESERVATION);
    }
    #[test]
    fn small_service_slots_admit_sixteen_and_release_shared_sender_pool_once() {
        let budget = Budget::new();
        let leases = (0..16)
            .map(|_| {
                budget
                    .reserve_event(RendererService::RenderingControl)
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(
            budget
                .reserve_event(RendererService::ConnectionManager)
                .is_err()
        );
        assert_eq!(budget.reserved(), 4_915_200);
        drop(leases);
        assert_eq!(budget.reserved(), BASE_RESERVATION);
    }
}
