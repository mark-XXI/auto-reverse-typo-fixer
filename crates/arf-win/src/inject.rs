//! Text injection and input-language switching.

use crate::hook::SIGNATURE;
use crate::sys::*;

fn key_input(vk: u16, scan: u16, flags: u32) -> INPUT {
    INPUT {
        type_: INPUT_KEYBOARD,
        u: INPUT_UNION {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: SIGNATURE,
            },
        },
    }
}

const SCAN_BACKSPACE: u16 = 0x0E;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// All events in one SendInput call; this is NOT an atomic editor transaction.
    Batch,
    /// Legacy multi-call mode. Disabled for automatic replacements because an
    /// interruption after Backspace can leave the original text partly erased.
    Paced,
}

fn send(inputs: &[INPUT]) -> Result<(), String> {
    if inputs.is_empty() {
        return Ok(());
    }
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err(format!(
            "SendInput accepted {sent} of {} events (blocked by UIPI or another hook?)",
            inputs.len()
        ))
    }
}

fn backspaces_inputs(n: usize) -> Vec<INPUT> {
    let mut v = Vec::with_capacity(n * 2);
    for _ in 0..n {
        v.push(key_input(VK_BACK, SCAN_BACKSPACE, 0));
        v.push(key_input(VK_BACK, SCAN_BACKSPACE, KEYEVENTF_KEYUP));
    }
    v
}

fn char_inputs(unit: u16) -> [INPUT; 2] {
    [
        key_input(0, unit, KEYEVENTF_UNICODE),
        key_input(0, unit, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP),
    ]
}

/// Inject a replacement with a last-moment caller-supplied focus/activity gate.
/// Automatic rewriting is restricted to ONE SendInput call. Even that call is
/// not atomic with the target app's editing state and may fail partially.
/// The adapter makes failure paths testable without generating actual input.
fn send_replacement_via(
    backspaces: usize,
    text: &str,
    mode: Mode,
    mut target_valid: impl FnMut() -> bool,
    mut send_once: impl FnMut(&[INPUT]) -> Result<(), String>,
) -> Result<(), String> {
    if mode == Mode::Paced {
        return Err("Paced automatic replacement is disabled to avoid partial deletion".to_owned());
    }
    let mut inputs = backspaces_inputs(backspaces);
    for unit in text.encode_utf16() {
        inputs.extend(char_inputs(unit));
    }
    if !target_valid() {
        return Err("Focus or input activity changed before injection".to_owned());
    }
    send_once(&inputs)?;
    if !target_valid() {
        return Err("Focus or input activity changed immediately after injection".to_owned());
    }
    Ok(())
}

/// Checked path used by the automatic correction and Undo workers.
pub fn send_replacement_checked(
    backspaces: usize,
    text: &str,
    mode: Mode,
    target_valid: impl FnMut() -> bool,
) -> Result<(), String> {
    send_replacement_via(backspaces, text, mode, target_valid, send)
}

/// Asks the foreground application to switch its input language.
/// (`ActivateKeyboardLayout` would only affect this process's own thread.)
/// Do not post a layout change to another foreground window after a correction delay.
/// This narrows, but cannot eliminate, the focus-change race at the Win32 boundary.
pub fn request_layout_if_foreground(expected_hwnd: HWND, hkl: HKL) -> bool {
    unsafe {
        expected_hwnd != 0
            && GetForegroundWindow() == expected_hwnd
            && PostMessageW(expected_hwnd, WM_INPUTLANGCHANGEREQUEST, 0, hkl) != 0
    }
}
pub fn request_layout(hkl: HKL) {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd != 0 {
            PostMessageW(hwnd, WM_INPUTLANGCHANGEREQUEST, 0, hkl);
        }
    }
}

/// Waits (bounded) until Shift/Ctrl/Alt/Win are physically released, so injected text is
/// not interpreted as a shortcut (e.g. Ctrl+Alt+Backspace).
pub fn wait_modifiers_released(timeout_ms: u64) {
    let start = std::time::Instant::now();
    let down = |vk: i32| unsafe { GetAsyncKeyState(vk) as u16 & 0x8000 != 0 };
    while start.elapsed().as_millis() < timeout_ms as u128 {
        if !(down(VK_SHIFT) || down(VK_CONTROL) || down(VK_MENU) || down(VK_LWIN) || down(VK_RWIN))
        {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

pub fn caps_lock_on() -> bool {
    unsafe { GetKeyState(VK_CAPITAL) & 1 != 0 }
}

#[cfg(test)]
mod safety_tests {
    use super::*;

    #[test]
    fn unsafe_target_blocks_both_injection_modes_before_any_send() {
        for mode in [Mode::Batch, Mode::Paced] {
            assert!(send_replacement_checked(3, "abc", mode, || false).is_err());
        }
    }

    #[test]
    fn paced_mode_is_rejected_before_any_send_even_if_focus_is_safe() {
        let mut calls = 0;
        let result = send_replacement_via(
            4,
            "text",
            Mode::Paced,
            || true,
            |_| {
                calls += 1;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(calls, 0);
    }

    #[test]
    fn changed_focus_cancels_batch_without_any_send() {
        let mut calls = 0;
        let result = send_replacement_via(
            4,
            "text",
            Mode::Batch,
            || false,
            |_| {
                calls += 1;
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(calls, 0);
    }

    #[test]
    fn batch_uses_only_one_send_and_reports_simulated_short_send() {
        let mut calls = 0;
        let result = send_replacement_via(
            3,
            "abc",
            Mode::Batch,
            || true,
            |inputs| {
                calls += 1;
                assert_eq!(inputs.len(), 12); // three backspaces + three ASCII letters
                Err("simulated partial SendInput".to_owned())
            },
        );
        assert!(result.is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn activity_during_batch_is_reported_as_failure() {
        use std::cell::Cell;
        let safe = Cell::new(true);
        let result = send_replacement_via(
            2,
            "abc",
            Mode::Batch,
            || safe.get(),
            |_| {
                safe.set(false); // simulated key press during OS injection
                Ok(())
            },
        );
        assert!(result.is_err());
    }
}
