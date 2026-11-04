//! Standalone, read-only UIA focus-event diagnostic.
//! The callback only queues a signal. All COM queries happen off the callback.
//! This is NOT yet an authorization source for real-time correction.

use crate::uia::{self, FocusSample};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::{implement, Ref, Result as WinResult};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationFocusChangedEventHandler,
    IUIAutomationFocusChangedEventHandler_Impl,
};

#[implement(IUIAutomationFocusChangedEventHandler)]
struct FocusEventRelay {
    sender: SyncSender<()>,
    epoch: Option<Arc<AtomicU64>>,
}

#[allow(non_snake_case)]
impl IUIAutomationFocusChangedEventHandler_Impl for FocusEventRelay_Impl {
    fn HandleFocusChangedEvent(&self, _sender: Ref<'_, IUIAutomationElement>) -> WinResult<()> {
        // Invalidate stale safety state before the worker can process the event.
        if let Some(epoch) = &self.epoch {
            epoch.fetch_add(1, Ordering::AcqRel);
        }
        // Coalesce notifications without blocking a COM callback. Epoch still advances for each event.
        // Never read text or make COM requests inside an OS callback.
        let _ = self.sender.try_send(());
        Ok(())
    }
}

pub(crate) fn make_guard_handler(
    sender: SyncSender<()>,
    epoch: Arc<AtomicU64>,
) -> IUIAutomationFocusChangedEventHandler {
    FocusEventRelay {
        sender,
        epoch: Some(epoch),
    }
    .into()
}

fn run_on_mta() -> Result<(), String> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
        .ok()
        .map_err(|e| format!("COM init failed: {e}"))?;
    struct ComGuard;
    impl Drop for ComGuard {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }
    let _com_guard = ComGuard;

    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }
            .map_err(|e| format!("Cannot start UI Automation: {e}"))?;

    let (tx, rx) = mpsc::sync_channel(1);
    let handler: IUIAutomationFocusChangedEventHandler = FocusEventRelay {
        sender: tx,
        epoch: None,
    }
    .into();
    unsafe { automation.AddFocusChangedEventHandler(None, &handler) }
        .map_err(|e| format!("Register focus handler failed: {e}"))?;

    println!("UIA event watch active: change fields for 20 seconds.");
    println!("No keyboard hooks, typing, or content access. Browser correction remains disabled.");
    let mut signals = 0usize;
    let mut transitions = 0usize;
    let started = Instant::now();
    let until = started + Duration::from_secs(20);

    // Capture the initial state to distinguish lack of events from no movement.
    let initial = uia::sample_from_query(uia::query_focused());
    println!(
        "initial: pid={} classification={:?}",
        initial.pid, initial.safety
    );
    let mut previous: Option<FocusSample> = Some(initial);

    while Instant::now() < until {
        let remaining = until.saturating_duration_since(Instant::now());
        let wait = remaining.min(Duration::from_millis(250));
        match rx.recv_timeout(wait) {
            Ok(()) => {
                signals += 1;
                // Sampling the CURRENT focus is intentionally diagnostic-only: a rapid
                // focus change might occur between the event and this query.
                let current = uia::sample_from_query(uia::query_focused());
                if previous != Some(current) {
                    println!(
                        "event: pid={} classification={:?}",
                        current.pid, current.safety
                    );
                    previous = Some(current);
                    transitions += 1;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    let remove = unsafe { automation.RemoveFocusChangedEventHandler(&handler) };
    println!("UIA event watch finished: {signals} processed signals (coalesced), {transitions} observed classification changes.");
    println!("No text was read. Do NOT use these observations to authorize browser correction.");
    remove.map_err(|e| format!("Could not unregister focus handler: {e}"))?;
    Ok(())
}

pub fn run_event_watch() -> i32 {
    match std::thread::spawn(run_on_mta).join() {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            eprintln!("UIA event watch: {e}");
            1
        }
        Err(_) => {
            eprintln!("UIA event watch thread panicked");
            1
        }
    }
}

#[cfg(test)]
mod queue_tests {
    use std::sync::mpsc::{sync_channel, TrySendError};

    #[test]
    fn focus_signals_coalesce_without_blocking() {
        let (tx, rx) = sync_channel(1);
        assert!(tx.try_send(()).is_ok());
        assert!(matches!(tx.try_send(()), Err(TrySendError::Full(()))));
        assert!(rx.try_recv().is_ok());
    }
}
