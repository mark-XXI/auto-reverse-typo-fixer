//! Low-level keyboard/mouse hooks. Callbacks only enqueue and return immediately:
//! Windows silently drops hooks whose callbacks take too long (LowLevelHooksTimeout).

use crate::sys::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::OnceLock;

/// Marks events injected by this program ("ARFT").
pub const SIGNATURE: usize = 0x4152_4654;

/// Hard cap on pending raw keyboard/mouse events. Never block a hook callback.
pub const INPUT_QUEUE_CAPACITY: usize = 2048;

/// Once an event is lost, the worker's view of the typed text cannot be trusted.
/// Disable all correction until the process restarts, even if the queue later drains.
#[derive(Default)]
struct DeliveryState {
    failed: AtomicBool,
}

impl DeliveryState {
    fn healthy(&self) -> bool {
        !self.failed.load(Ordering::Acquire)
    }

    fn try_deliver(&self, tx: &SyncSender<RawEvent>, event: RawEvent) -> bool {
        if !self.healthy() {
            return false;
        }
        if tx.try_send(event).is_err() {
            self.failed.store(true, Ordering::Release);
            return false;
        }
        true
    }
}

static EVENT_DELIVERY: DeliveryState = DeliveryState {
    failed: AtomicBool::new(false),
};

/// The worker must never rewrite text after even one dropped hook event.
pub fn event_stream_healthy() -> bool {
    EVENT_DELIVERY.healthy()
}

/// No real hooks or input are involved. This exercises the bounded queue and
/// its permanent fail-closed transition using synthetic events only.
pub fn run_input_queue_stress() -> i32 {
    let (tx, rx) = mpsc::sync_channel(INPUT_QUEUE_CAPACITY);
    let delivery = DeliveryState::default();
    let started = std::time::Instant::now();
    let mut accepted = 0usize;
    for _ in 0..100_000 {
        if delivery.try_deliver(&tx, RawEvent::Mouse) {
            accepted += 1;
        }
    }
    let elapsed = started.elapsed();
    let queued = rx.try_iter().count();
    let passed =
        accepted == INPUT_QUEUE_CAPACITY && queued == INPUT_QUEUE_CAPACITY && !delivery.healthy();
    println!(
        "Synthetic input queue: capacity={} accepted={} buffered={} in {:?}",
        INPUT_QUEUE_CAPACITY, accepted, queued, elapsed
    );
    println!(
        "Overflow permanently disables correction: {}",
        !delivery.healthy()
    );
    println!("No keyboard hooks, COM, user text, or injection were accessed.");
    if passed {
        0
    } else {
        1
    }
}

/// Monotonic generation for physical key presses, mouse clicks, and foreign input.
/// It is updated in the low-level hook BEFORE the event is queued for the worker.
#[derive(Default)]
struct ActivityTracker {
    generation: AtomicU64,
}

impl ActivityTracker {
    fn note_input(&self) -> u64 {
        self.generation
            .fetch_add(1, Ordering::SeqCst)
            .wrapping_add(1)
    }

    fn current(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn matches(&self, event_generation: u64) -> bool {
        event_generation != 0 && self.current() == event_generation
    }
}

static INPUT_ACTIVITY: ActivityTracker = ActivityTracker {
    generation: AtomicU64::new(0),
};

/// Used by the worker to reject a rewrite if any newer real input arrived.
pub fn activity_is_current(event_generation: u64) -> bool {
    INPUT_ACTIVITY.matches(event_generation)
}

#[derive(Clone, Copy, Debug)]
pub enum RawEvent {
    Key {
        vk: u32,
        scan: u16,
        extended: bool,
        down: bool,
        activity: u64,
    },
    /// Input injected by another program: positions in the target app are no longer trustworthy.
    Foreign,
    Mouse,
}

impl RawEvent {
    /// A key press or click (something that changes the text or the caret).
    pub fn is_press(&self) -> bool {
        match self {
            RawEvent::Key { down, .. } => *down,
            _ => true,
        }
    }
}

static TX: OnceLock<SyncSender<RawEvent>> = OnceLock::new();

fn emit(ev: RawEvent) {
    if let Some(tx) = TX.get() {
        if !EVENT_DELIVERY.try_deliver(tx, ev) {
            // Also invalidate any in-flight correction token. A lost event is
            // irrecoverable: the worker will stop correcting for this session.
            INPUT_ACTIVITY.note_input();
        }
    }
}

unsafe extern "system" fn keyboard_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION && lparam != 0 {
        let k = &*(lparam as *const KBDLLHOOKSTRUCT);
        let down = matches!(wparam as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        if k.dwExtraInfo == SIGNATURE {
            // our own injection: ignore (prevents feedback loops)
        } else if k.flags & (LLKHF_INJECTED | LLKHF_LOWER_IL_INJECTED) != 0 {
            if down {
                INPUT_ACTIVITY.note_input();
                emit(RawEvent::Foreign);
            }
        } else {
            let activity = if down {
                INPUT_ACTIVITY.note_input()
            } else {
                INPUT_ACTIVITY.current()
            };
            emit(RawEvent::Key {
                vk: k.vkCode,
                scan: k.scanCode as u16,
                extended: k.flags & LLKHF_EXTENDED != 0,
                down,
                activity,
            });
        }
    }
    CallNextHookEx(0, code, wparam, lparam)
}

unsafe extern "system" fn mouse_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if code == HC_ACTION
        && matches!(
            wparam as u32,
            WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
        )
    {
        INPUT_ACTIVITY.note_input();
        emit(RawEvent::Mouse);
    }
    CallNextHookEx(0, code, wparam, lparam)
}

pub struct Hooks {
    kb: HHOOK,
    mouse: HHOOK,
}

impl Hooks {
    pub fn install(tx: SyncSender<RawEvent>) -> Result<Hooks, String> {
        TX.set(tx)
            .map_err(|_| "hooks already installed".to_string())?;
        unsafe {
            let module = GetModuleHandleW(std::ptr::null());
            let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(keyboard_proc), module, 0);
            if kb == 0 {
                return Err("SetWindowsHookExW(WH_KEYBOARD_LL) failed".into());
            }
            let mouse = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), module, 0);
            if mouse == 0 {
                UnhookWindowsHookEx(kb);
                return Err("SetWindowsHookExW(WH_MOUSE_LL) failed".into());
            }
            Ok(Hooks { kb, mouse })
        }
    }

    /// Runs the message loop that low-level hooks require. Returns when `WM_QUIT` arrives.
    pub fn run_message_loop(&self) {
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, 0, 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        unsafe {
            UnhookWindowsHookEx(self.kb);
            UnhookWindowsHookEx(self.mouse);
        }
    }
}

#[cfg(test)]
mod activity_tests {
    use super::*;

    #[test]
    fn newer_physical_input_invalidates_replacement_token() {
        let clock = ActivityTracker::default();
        assert!(!clock.matches(0));
        let space = clock.note_input();
        assert!(clock.matches(space));
        clock.note_input(); // another key or click while a rewrite is planned
        assert!(!clock.matches(space));
    }

    #[test]
    fn key_up_does_not_invalidate_press_token() {
        let clock = ActivityTracker::default();
        let press = clock.note_input();
        // A key-up event reads current(), without advancing the generation.
        assert_eq!(clock.current(), press);
        assert!(clock.matches(press));
    }

    #[test]
    fn overflow_disables_delivery_permanently_even_after_drain() {
        let (tx, rx) = mpsc::sync_channel(2);
        let state = DeliveryState::default();
        assert!(state.try_deliver(&tx, RawEvent::Mouse));
        assert!(state.try_deliver(&tx, RawEvent::Foreign));
        assert!(!state.try_deliver(&tx, RawEvent::Mouse));
        assert!(!state.healthy());
        assert_eq!(rx.try_iter().count(), 2);
        // Once events have been dropped, do not resume with an incomplete run.
        assert!(!state.try_deliver(&tx, RawEvent::Mouse));
        assert!(!state.healthy());
    }

    #[test]
    fn disconnected_consumer_disables_delivery() {
        let (tx, rx) = mpsc::sync_channel(1);
        drop(rx);
        let state = DeliveryState::default();
        assert!(!state.try_deliver(&tx, RawEvent::Mouse));
        assert!(!state.healthy());
    }
}
