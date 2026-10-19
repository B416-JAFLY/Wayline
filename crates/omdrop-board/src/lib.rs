//! Optional display feedback. Never opens the radio COM port or changes transfer outcomes.
pub use omdrop_proto::transfer::{Phase, Reason};
use omdrop_proto::transfer::{Transfer, FRAME_LEN, HEADER_LEN, LABEL_LEN};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{query, raster_label};

#[derive(Clone)]
struct Slot {
    frame: [u8; FRAME_LEN],
    revision: u64,
    delivered: u64,
    changed: Instant,
    sent: Option<Instant>,
}
type Slots = [Option<Slot>; 2];
struct Shared {
    slots: Mutex<Slots>,
    delivered: Condvar,
}
static SHARED: OnceLock<Arc<Shared>> = OnceLock::new();

pub struct Tracker {
    shared: Option<Arc<Shared>>,
    state: Transfer,
}
impl Tracker {
    pub fn begin(enabled: bool, direction: u8, peer: &str, files: usize) -> Self {
        let mut state = Transfer::idle(direction.min(1));
        let shared = if enabled && cfg!(windows) {
            Some(
                SHARED
                    .get_or_init(|| {
                        let shared = Arc::new(Shared {
                            slots: Mutex::new([None, None]),
                            delivered: Condvar::new(),
                        });
                        let worker = shared.clone();
                        std::thread::spawn(move || run(worker));
                        shared
                    })
                    .clone(),
            )
        } else {
            None
        };
        if let Some(shared) = &shared {
            let mut random = [0; 8];
            if getrandom::fill(&mut random).is_err() {
                return Self {
                    shared: None,
                    state,
                };
            }
            state.id = u64::from_le_bytes(random).max(1);
            state.files = files.min(u16::MAX as usize) as u16;
            state.phase = Phase::Preparing;
            let mut frame = [0; FRAME_LEN];
            frame[..HEADER_LEN].copy_from_slice(&state.encode());
            frame[HEADER_LEN..].copy_from_slice(&label(peer));
            let mut slots = shared.slots.lock().unwrap_or_else(|e| e.into_inner());
            let slot = &mut slots[usize::from(state.direction)];
            // One visible task per direction. Concurrent tasks never steal another task's label.
            if slot.as_ref().is_some_and(|s| {
                Transfer::decode(&s.frame[..HEADER_LEN])
                    .unwrap()
                    .phase
                    .active()
            }) {
                return Self {
                    shared: None,
                    state,
                };
            }
            *slot = Some(Slot {
                frame,
                revision: 1,
                delivered: 0,
                changed: Instant::now(),
                sent: None,
            });
        }
        Self { shared, state }
    }
    pub fn update(&mut self, phase: Phase, done: u64, total: Option<u64>, reason: Reason) {
        self.state.phase = phase;
        self.state.done = done;
        self.state.total = total.unwrap_or(0);
        self.state.known_total = total.is_some_and(|n| n > 0 && done <= n);
        self.state.reason = reason;
        let Some(shared) = &self.shared else {
            return;
        };
        let mut slots = shared.slots.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = &mut slots[usize::from(self.state.direction)] else {
            return;
        };
        if slot.frame[8..16] != self.state.id.to_le_bytes() {
            return;
        }
        slot.frame[..HEADER_LEN].copy_from_slice(&self.state.encode());
        slot.revision += 1;
        slot.changed = Instant::now();
        // Terminal delivery gets a bounded opportunity before a short-lived CLI exits.
        if !phase.active() {
            let revision = slot.revision;
            let _ =
                shared
                    .delivered
                    .wait_timeout_while(slots, Duration::from_millis(500), |slots| {
                        slots[usize::from(self.state.direction)]
                            .as_ref()
                            .is_some_and(|s| s.delivered < revision)
                    });
        }
    }
    pub fn phase(&mut self, phase: Phase) {
        self.update(
            phase,
            self.state.done,
            self.state.known_total.then_some(self.state.total),
            Reason::None,
        );
    }
    pub fn fail(&mut self, reason: Reason) {
        self.update(
            Phase::Failed,
            self.state.done,
            self.state.known_total.then_some(self.state.total),
            reason,
        );
    }
}
impl Drop for Tracker {
    fn drop(&mut self) {
        if self.state.phase.active() {
            self.fail(Reason::Interrupted);
        }
    }
}

fn label(peer: &str) -> [u8; LABEL_LEN] {
    #[cfg(windows)]
    {
        raster_label(peer).unwrap_or([0; LABEL_LEN])
    }
    #[cfg(not(windows))]
    {
        let _ = peer;
        [0; LABEL_LEN]
    }
}

fn run(shared: Arc<Shared>) {
    #[cfg(windows)]
    {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return;
        };
        loop {
            let snapshot = shared
                .slots
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            for (direction, slot) in snapshot.iter().enumerate() {
                let Some(slot) = slot else {
                    continue;
                };
                let mut frame = slot.frame;
                let state = Transfer::decode(&frame[..HEADER_LEN]).unwrap();
                let terminal = !state.phase.active();
                let expiry = if matches!(state.phase, Phase::Failed | Phase::Cancelled) {
                    30
                } else {
                    10
                };
                let expired = terminal && slot.changed.elapsed() >= Duration::from_secs(expiry);
                if expired {
                    frame[2] = Phase::Idle as u8;
                }
                if !expired
                    && slot.revision == slot.delivered
                    && slot
                        .sent
                        .is_some_and(|at| at.elapsed() < Duration::from_secs(1))
                {
                    continue;
                }
                if runtime.block_on(windows::publish(&frame)).is_ok() {
                    let mut slots = shared.slots.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(current) = &mut slots[direction] {
                        if current.frame[8..16] == frame[8..16] {
                            current.delivered = slot.revision;
                            current.sent = Some(Instant::now());
                            if expired && current.revision == slot.revision {
                                slots[direction] = None;
                            }
                        }
                    }
                    shared.delivered.notify_all();
                }
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = shared;
    }
}
