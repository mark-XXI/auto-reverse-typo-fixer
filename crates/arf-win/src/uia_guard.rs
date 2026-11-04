//! Conservative, background-only UIA focus gate.
//! No key events or user text enter this module. UIA cannot authorize browser
//! correction yet: the separate browser denylist remains mandatory.

use crate::uia::{FocusSafety, FocusSample};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const MAX_SAMPLE_AGE: Duration = Duration::from_millis(600);
const REFRESH_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WinFocus {
    pub hwnd: isize,
    pub focus: isize,
    pub pid: u32,
}

#[derive(Clone, Copy)]
struct Cached {
    sample: FocusSample,
    win: WinFocus,
    epoch: u64,
    checked_at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiaVerdict {
    pub epoch: u64,
    pub safety: FocusSafety,
}

impl UiaVerdict {
    pub fn allows_text(self) -> bool {
        self.safety == FocusSafety::NonPassword
    }
}

pub struct UiaGuard {
    epoch: Arc<AtomicU64>,
    latest: Arc<Mutex<Option<Cached>>>,
    stop: Arc<AtomicBool>,
    // Terminal monitoring failure must revoke cached permissions immediately.
    monitor_healthy: Arc<AtomicBool>,
}

impl UiaGuard {
    fn empty() -> Self {
        Self {
            epoch: Arc::new(AtomicU64::new(0)),
            latest: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            monitor_healthy: Arc::new(AtomicBool::new(true)),
        }
    }

    /// Starts a dedicated MTA worker. Startup errors leave the gate Unknown.
    pub fn start() -> Self {
        let guard = Self::empty();
        #[cfg(windows)]
        {
            let epoch = Arc::clone(&guard.epoch);
            let latest = Arc::clone(&guard.latest);
            let stop = Arc::clone(&guard.stop);
            let monitor_healthy = Arc::clone(&guard.monitor_healthy);
            let spawned = std::thread::Builder::new()
                .name("arf-uia-safety".to_owned())
                .spawn(move || {
                    // A provider may error or panic. Neither can leave a previously
                    // approved cached focus usable after the monitor exits.
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_monitor(Arc::clone(&epoch), Arc::clone(&latest), stop)
                    }));
                    mark_monitor_stopped(&monitor_healthy, &epoch, &latest);
                    match result {
                        Ok(Err(error)) => eprintln!(
                            "warning: UIA safety monitor unavailable: {error}; correction remains blocked"
                        ),
                        Err(_) => eprintln!(
                            "warning: UIA safety monitor panicked; correction remains blocked"
                        ),
                        Ok(Ok(())) => {}
                    }
                });
            if let Err(error) = spawned {
                mark_monitor_stopped(&guard.monitor_healthy, &guard.epoch, &guard.latest);
                eprintln!(
                    "warning: cannot spawn UIA safety monitor: {error}; correction remains blocked"
                );
            }
        }
        guard
    }

    /// Cheap read on the keyboard worker. Fail closed for missing/stale data,
    /// process/window mismatch, or an event racing with this read.
    pub fn verdict(&self, current: WinFocus) -> UiaVerdict {
        let monitor_before = self.monitor_healthy.load(Ordering::Acquire);
        let before = self.epoch.load(Ordering::Acquire);
        let cached = self.latest.lock().ok().and_then(|value| *value);
        let after = self.epoch.load(Ordering::Acquire);
        let monitor_after = self.monitor_healthy.load(Ordering::Acquire);
        let safety = match cached {
            Some(entry)
                if monitor_before
                    && monitor_after
                    && current.pid > 0
                    && current.hwnd != 0
                    && before == after
                    && entry.epoch == after
                    && entry.win == current
                    && entry.sample.pid > 0
                    && entry.sample.pid as u32 == current.pid
                    && entry.checked_at.elapsed() <= MAX_SAMPLE_AGE =>
            {
                entry.sample.safety
            }
            _ => FocusSafety::Unknown,
        };
        UiaVerdict {
            epoch: after,
            safety,
        }
    }
}

/// Revoke every authorization as soon as the monitor stops. A stalled monitor
/// is additionally guarded by MAX_SAMPLE_AGE even if no exit is observable.
fn mark_monitor_stopped(healthy: &AtomicBool, epoch: &AtomicU64, latest: &Mutex<Option<Cached>>) {
    healthy.store(false, Ordering::Release);
    epoch.fetch_add(1, Ordering::AcqRel);
    if let Ok(mut value) = latest.lock() {
        *value = None;
    }
}

impl Drop for UiaGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

#[cfg(windows)]
fn read_win_focus() -> Option<WinFocus> {
    use crate::sys::{
        GetForegroundWindow, GetGUIThreadInfo, GetWindowThreadProcessId, GUITHREADINFO,
    };
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd == 0 {
            return None;
        }
        let mut pid = 0u32;
        let tid = GetWindowThreadProcessId(hwnd, &mut pid);
        if tid == 0 || pid == 0 {
            return None;
        }
        let mut info: GUITHREADINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
        if GetGUIThreadInfo(tid, &mut info) == 0 || info.hwndFocus == 0 {
            return None; // The active edit control is unknown; fail closed.
        }
        Some(WinFocus {
            hwnd,
            focus: info.hwndFocus,
            pid,
        })
    }
}

#[cfg(windows)]
fn run_monitor(
    epoch: Arc<AtomicU64>,
    latest: Arc<Mutex<Option<Cached>>>,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    use crate::uia::{query_focused, sample_from_query};
    use std::sync::mpsc::{self, RecvTimeoutError};
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER,
        COINIT_MULTITHREADED,
    };
    use windows::Win32::UI::Accessibility::{CUIAutomation, IUIAutomation};

    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|e| format!("COM initialization failed: {e}"))?;
    struct ComGuard;
    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let _com_guard = ComGuard;

    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("UI Automation creation failed: {e}"))?;

    let (sender, receiver) = mpsc::sync_channel(1);
    // The COM callback increments epoch BEFORE queuing an event, immediately
    // invalidating any previous NonPassword classification.
    let handler = crate::uia_events::make_guard_handler(sender, Arc::clone(&epoch));
    unsafe { automation.AddFocusChangedEventHandler(None, &handler) }
        .map_err(|e| format!("Cannot register focus handler: {e}"))?;

    let result = loop {
        if stop.load(Ordering::Acquire) {
            break Ok(());
        }
        let before = epoch.load(Ordering::Acquire);
        let before_win = read_win_focus();
        let sample = sample_from_query(query_focused());
        let after_win = read_win_focus();
        let after = epoch.load(Ordering::Acquire);
        // If UIA and Win32 focus disagree, or focus moved during the
        // query, never publish an authorizing result.
        let verified = match (before_win, after_win) {
            (Some(a), Some(b))
                if before == after && a == b && sample.pid > 0 && sample.pid as u32 == b.pid =>
            {
                Some(b)
            }
            _ => None,
        };
        match latest.lock() {
            Ok(mut state) => {
                *state = verified.map(|win| Cached {
                    sample,
                    win,
                    epoch: after,
                    checked_at: Instant::now(),
                });
            }
            Err(_) => break Err("UIA safety state lock failed".to_owned()),
        }
        match receiver.recv_timeout(REFRESH_INTERVAL) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                break Err("UIA focus event channel closed".to_owned());
            }
        }
    };
    let cleanup = unsafe { automation.RemoveFocusChangedEventHandler(&handler) }
        .map_err(|e| format!("Cannot unregister focus handler: {e}"));
    result.and(cleanup)
}

/// Read-only probe of real UIA query latency and classification. It runs in
/// its own thread and uses a bounded wait so a stuck provider does not hang
/// the caller. It never installs keyboard hooks or reads control contents.
#[cfg(windows)]
pub fn run_guard_probe() -> i32 {
    const COUNT: usize = 15;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("arf-uia-probe".to_string())
        .spawn(move || {
            println!("Focus a dummy normal/password field within 3 seconds.");
            std::thread::sleep(Duration::from_secs(3));
            let mut latencies = Vec::with_capacity(COUNT);
            let mut good = 0usize;
            let mut errors = 0usize;
            for index in 0..COUNT {
                let started = Instant::now();
                let result = crate::uia::query_focused();
                let elapsed = started.elapsed();
                latencies.push(elapsed);
                let class = match result {
                    Ok((pid, safety)) if pid > 0 => {
                        good += 1;
                        safety
                    }
                    _ => {
                        errors += 1;
                        FocusSafety::Unknown
                    }
                };
                println!(
                    "UIA probe {}/{}: {:?}, {:.3} ms",
                    index + 1,
                    COUNT,
                    class,
                    elapsed.as_secs_f64() * 1000.0
                );
                std::thread::sleep(Duration::from_millis(80));
            }
            latencies.sort_unstable();
            let p50 = latencies[COUNT / 2].as_secs_f64() * 1000.0;
            let p95_index = ((COUNT * 95).div_ceil(100) - 1).min(COUNT - 1);
            let p95 = latencies[p95_index].as_secs_f64() * 1000.0;
            let maximum = latencies[COUNT - 1].as_secs_f64() * 1000.0;
            println!(
                "UIA probe summary: success={} errors={} p50={:.3}ms p95={:.3}ms max={:.3}ms",
                good, errors, p50, p95, maximum
            );
            println!("No text, keyboard hooks, or injected input were accessed.");
            let _ = sender.try_send(good > 0);
        });
    if let Err(error) = spawned {
        eprintln!("Cannot spawn UIA probe: {error}");
        return 1;
    }
    match receiver.recv_timeout(Duration::from_secs(15)) {
        Ok(true) => 0,
        Ok(false) => {
            eprintln!("All UIA probe requests failed.");
            1
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            eprintln!("UIA probe exceeded 15 seconds; possible blocked provider.");
            1
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            eprintln!("UIA probe worker stopped unexpectedly.");
            1
        }
    }
}

/// Synthetic fail-closed check: no UIA/Win32 calls or keyboard hooks.
#[cfg(windows)]
pub fn run_guard_fault_test() -> i32 {
    let guard = UiaGuard::empty();
    let focus = WinFocus {
        hwnd: 100,
        focus: 101,
        pid: 42,
    };
    *guard.latest.lock().unwrap() = Some(Cached {
        sample: FocusSample {
            pid: 42,
            safety: FocusSafety::NonPassword,
        },
        win: focus,
        epoch: 0,
        checked_at: Instant::now(),
    });
    let before = guard.verdict(focus).allows_text();
    mark_monitor_stopped(&guard.monitor_healthy, &guard.epoch, &guard.latest);
    let after = guard.verdict(focus);
    let cleared = guard.latest.lock().unwrap().is_none();
    println!(
        "Synthetic monitor failure: previously_allowed={} after={:?} cache_cleared={}",
        before, after.safety, cleared
    );
    println!("No UIA, Win32 hooks, input content, or injection were accessed.");
    if before && !after.allows_text() && cleared {
        0
    } else {
        1
    }
}

/// Diagnostic only. No hooks, rewriting or input-content access.
#[cfg(windows)]
pub fn run_guard_watch() -> i32 {
    let guard = UiaGuard::start();
    println!("UIA safety gate watch: 20 seconds. Switch between ordinary/password fields.");
    println!("Gate=NonPassword is NOT permission to correct text inside browsers.");
    let until = Instant::now() + Duration::from_secs(20);
    let mut previous: Option<(u32, UiaVerdict)> = None;
    while Instant::now() < until {
        let current = read_win_focus();
        let state = guard.verdict(current.unwrap_or(WinFocus {
            hwnd: 0,
            focus: 0,
            pid: 0,
        }));
        let pid = current.map_or(0, |c| c.pid);
        if previous != Some((pid, state)) {
            println!(
                "gate: pid={pid} epoch={} classification={:?}",
                state.epoch, state.safety
            );
            previous = Some((pid, state));
        }
        std::thread::sleep(Duration::from_millis(80));
    }
    println!("UIA gate watch complete. No input content was accessed.");
    0
}

/// Read-only microbenchmark for the cached gate. Does not start UIA, hooks or injection.
/// This measures lookup cost, NOT full end-to-end typing latency.
#[cfg(windows)]
pub fn run_guard_bench() -> i32 {
    let guard = UiaGuard::empty();
    let focus = WinFocus {
        hwnd: 100,
        focus: 101,
        pid: 42,
    };
    *guard.latest.lock().unwrap() = Some(Cached {
        sample: FocusSample {
            pid: 42,
            safety: FocusSafety::NonPassword,
        },
        win: focus,
        epoch: 0,
        checked_at: Instant::now(),
    });
    const N: u32 = 10_000;
    let started = Instant::now();
    let mut allowed = 0u32;
    for _ in 0..N {
        if guard.verdict(focus).allows_text() {
            allowed += 1;
        }
    }
    let elapsed = started.elapsed();
    guard.epoch.fetch_add(1, Ordering::AcqRel);
    let denied = (0..N)
        .filter(|_| !guard.verdict(focus).allows_text())
        .count();
    println!(
        "Cached gate: {N} checks in {:?}; allowed={allowed}",
        elapsed
    );
    println!("After invalidation: {denied}/{N} checks denied");
    println!("No input content, Win32 hooks, UIA services, or injection were accessed.");
    if denied != N as usize {
        1
    } else {
        0
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> WinFocus {
        WinFocus {
            hwnd: 100,
            focus: 101,
            pid: 42,
        }
    }

    fn record(guard: &UiaGuard, safety: FocusSafety, age: Duration) {
        *guard.latest.lock().unwrap() = Some(Cached {
            sample: FocusSample { pid: 42, safety },
            win: context(),
            epoch: guard.epoch.load(Ordering::Acquire),
            checked_at: Instant::now() - age,
        });
    }

    #[test]
    fn new_epoch_requires_new_matching_sample() {
        let guard = UiaGuard::empty();
        record(&guard, FocusSafety::NonPassword, Duration::ZERO);
        assert!(guard.verdict(context()).allows_text());
        guard.epoch.fetch_add(1, Ordering::AcqRel);
        assert!(!guard.verdict(context()).allows_text());
        record(&guard, FocusSafety::NonPassword, Duration::ZERO);
        assert!(guard.verdict(context()).allows_text());
    }

    #[test]
    fn unknown_process_or_missing_focus_never_authorizes() {
        let guard = UiaGuard::empty();
        record(&guard, FocusSafety::NonPassword, Duration::ZERO);
        assert!(!guard
            .verdict(WinFocus {
                pid: 0,
                ..context()
            })
            .allows_text());
        assert!(!guard
            .verdict(WinFocus {
                focus: 0,
                ..context()
            })
            .allows_text());
        assert!(!guard
            .verdict(WinFocus {
                hwnd: 0,
                ..context()
            })
            .allows_text());
    }
    #[test]
    fn fails_closed_until_matching_fresh_non_password_sample() {
        let guard = UiaGuard::empty();
        assert!(!guard.verdict(context()).allows_text());
        record(&guard, FocusSafety::NonPassword, Duration::ZERO);
        assert!(guard.verdict(context()).allows_text());
        assert!(!guard
            .verdict(WinFocus {
                pid: 41,
                ..context()
            })
            .allows_text());
        assert!(!guard
            .verdict(WinFocus {
                focus: 102,
                ..context()
            })
            .allows_text());
        assert!(!guard
            .verdict(WinFocus {
                hwnd: 102,
                ..context()
            })
            .allows_text());
        record(&guard, FocusSafety::Password, Duration::ZERO);
        assert!(!guard.verdict(context()).allows_text());
        record(&guard, FocusSafety::Unknown, Duration::ZERO);
        assert!(!guard.verdict(context()).allows_text());
    }

    #[test]
    fn focus_event_invalidates_old_authorization() {
        let guard = UiaGuard::empty();
        record(&guard, FocusSafety::NonPassword, Duration::ZERO);
        assert!(guard.verdict(context()).allows_text());
        guard.epoch.fetch_add(1, Ordering::AcqRel);
        assert_eq!(guard.verdict(context()).safety, FocusSafety::Unknown);
    }

    #[test]
    fn stale_samples_cannot_authorize_input() {
        let guard = UiaGuard::empty();
        record(&guard, FocusSafety::NonPassword, Duration::from_secs(2));
        assert_eq!(guard.verdict(context()).safety, FocusSafety::Unknown);
    }

    #[test]
    fn terminated_monitor_revokes_fresh_sample_and_clears_cache() {
        let guard = UiaGuard::empty();
        record(&guard, FocusSafety::NonPassword, Duration::ZERO);
        assert!(guard.verdict(context()).allows_text());
        mark_monitor_stopped(&guard.monitor_healthy, &guard.epoch, &guard.latest);
        assert_eq!(guard.verdict(context()).safety, FocusSafety::Unknown);
        assert!(guard.latest.lock().unwrap().is_none());
    }

    #[test]
    fn terminated_monitor_cannot_reauthorize_even_with_new_cache() {
        let guard = UiaGuard::empty();
        mark_monitor_stopped(&guard.monitor_healthy, &guard.epoch, &guard.latest);
        record(&guard, FocusSafety::NonPassword, Duration::ZERO);
        assert_eq!(guard.verdict(context()).safety, FocusSafety::Unknown);
    }
}
