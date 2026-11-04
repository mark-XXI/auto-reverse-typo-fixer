//! Foreground/focus information: layout, process, password-field detection.

use crate::sys::*;

/// Until UI Automation focus tracking has been integrated and validated, do not
/// buffer or transform text typed in browsers. Unknown processes fail closed.
/// This protection is intentionally independent of user-configurable blacklist.
pub fn is_sensitive_host(process: &str) -> bool {
    matches!(
        process,
        "" | "brave.exe"
            | "chrome.exe"
            | "msedge.exe"
            | "firefox.exe"
            | "chromium.exe"
            | "opera.exe"
            | "opera_gx.exe"
            | "vivaldi.exe"
            | "waterfox.exe"
            | "librewolf.exe"
            | "zen.exe"
            | "arc.exe"
            | "msedgewebview2.exe"
    )
}

#[derive(Clone, Debug)]
pub struct FocusCtx {
    pub hwnd: HWND,
    pub focus: HWND,
    pub pid: u32,
    pub hkl: HKL,
    pub is_password: bool,
    /// Lower-case executable name (empty if it could not be queried).
    pub process: String,
}

/// Reads the live foreground context. No PID or HWND safety classification is cached:
/// Windows may reuse process IDs and window handles after their owners exit.
#[derive(Default)]
pub struct FocusMonitor;

/// A missing or malformed executable path must never become an allowed process.
fn process_basename(path: &str) -> Option<String> {
    let filename = path.rsplit(['\\', '/']).next()?.trim();
    if filename.is_empty() || filename == "." || filename == ".." {
        None
    } else {
        Some(filename.to_lowercase())
    }
}

/// A zero style for an Edit control is ambiguous (GetWindowLongW can fail).
/// Reject it, while other control classes must still pass the separate UIA gate.
fn win32_password_status(class: &str, style: i32) -> Option<bool> {
    if class.eq_ignore_ascii_case("Edit") {
        if style == 0 {
            None
        } else {
            Some(style & ES_PASSWORD != 0)
        }
    } else {
        Some(false)
    }
}

impl FocusMonitor {
    pub fn new() -> Self {
        Self
    }

    /// Return None if the window, keyboard focus, layout, process or class cannot
    /// be positively identified. The caller treats None as a hard run break.
    pub fn current(&mut self) -> Option<FocusCtx> {
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
            let mut gti: GUITHREADINFO = std::mem::zeroed();
            gti.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
            if GetGUIThreadInfo(tid, &mut gti) == 0 || gti.hwndFocus == 0 {
                return None;
            }
            let focus = gti.hwndFocus;
            let hkl = GetKeyboardLayout(tid);
            if hkl == 0 {
                return None;
            }
            let is_password = Self::is_password(focus)?;
            let process = Self::process_name(pid)?;

            // A window could disappear while the process and class are queried.
            // Do not return a context for a different foreground owner.
            if GetForegroundWindow() != hwnd {
                return None;
            }
            let mut confirmed_pid = 0u32;
            let confirmed_tid = GetWindowThreadProcessId(hwnd, &mut confirmed_pid);
            if confirmed_tid != tid || confirmed_pid != pid {
                return None;
            }
            Some(FocusCtx {
                hwnd,
                focus,
                pid,
                hkl,
                is_password,
                process,
            })
        }
    }

    /// Classic Win32 Edit controls: never cache a password decision by HWND.
    /// Non-Edit controls still require current UI Automation classification.
    fn is_password(focus: HWND) -> Option<bool> {
        unsafe {
            let mut buf = [0u16; 128];
            let n = GetClassNameW(focus, buf.as_mut_ptr(), buf.len() as i32);
            if n <= 0 || n as usize >= buf.len() - 1 {
                return None;
            }
            let class = String::from_utf16_lossy(&buf[..n as usize]);
            let style = if class.eq_ignore_ascii_case("Edit") {
                GetWindowLongW(focus, GWL_STYLE)
            } else {
                0
            };
            win32_password_status(&class, style)
        }
    }

    /// Re-query the process executable on each context read instead of using a
    /// PID-only cache. Query failure fails closed; the handle is always closed.
    fn process_name(pid: u32) -> Option<String> {
        if pid == 0 {
            return None;
        }
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h == 0 {
                return None;
            }
            struct Handle(HANDLE);
            impl Drop for Handle {
                fn drop(&mut self) {
                    unsafe {
                        CloseHandle(self.0);
                    }
                }
            }
            let _handle = Handle(h);
            let mut buf = [0u16; 520];
            let mut len = buf.len() as u32;
            if QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len) == 0
                || len == 0
                || len as usize > buf.len()
            {
                return None;
            }
            let path = String::from_utf16_lossy(&buf[..len as usize]);
            process_basename(&path)
        }
    }
}

#[cfg(test)]
mod sensitive_host_tests {
    use super::is_sensitive_host;

    #[test]
    fn known_browsers_and_unknown_process_are_protected() {
        for name in [
            "",
            "brave.exe",
            "chrome.exe",
            "msedge.exe",
            "firefox.exe",
            "chromium.exe",
            "vivaldi.exe",
            "opera.exe",
            "msedgewebview2.exe",
        ] {
            assert!(is_sensitive_host(name), "unprotected: {name:?}");
        }
    }

    #[test]
    fn normal_desktop_apps_are_not_blanket_blocked() {
        for name in ["notepad.exe", "winword.exe", "calc.exe"] {
            assert!(!is_sensitive_host(name), "unexpectedly blocked: {name:?}");
        }
    }
}

#[cfg(test)]
mod fresh_context_tests {
    use super::{process_basename, win32_password_status, ES_PASSWORD};

    #[test]
    fn process_names_are_normalized_from_live_paths() {
        assert_eq!(
            process_basename(r"C:\Program Files\Browser\CHROME.EXE"),
            Some("chrome.exe".into())
        );
        assert_eq!(
            process_basename("/opt/Apps/MsEdgeWebView2.EXE"),
            Some("msedgewebview2.exe".into())
        );
    }

    #[test]
    fn empty_or_malformed_process_names_are_rejected() {
        for path in ["", "C:\\", "/", ".", "..", "  "] {
            assert!(
                process_basename(path).is_none(),
                "unexpected process: {path:?}"
            );
        }
    }

    #[test]
    fn classic_edit_password_checks_fail_closed_on_missing_style() {
        assert_eq!(
            win32_password_status("Edit", ES_PASSWORD | 0x4000_0000),
            Some(true)
        );
        assert_eq!(win32_password_status("eDiT", 0x4000_0000), Some(false));
        assert_eq!(win32_password_status("Edit", 0), None);
        assert_eq!(win32_password_status("Chrome_WidgetWin_1", 0), Some(false));
    }
}
