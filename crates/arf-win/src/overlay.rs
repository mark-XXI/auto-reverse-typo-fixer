//! Small click-through "toast" shown next to the caret after a correction / undo.
//!
//! It runs on its own thread with its own window and never takes focus, so the text the user
//! is typing is never touched (unlike selecting the corrected word, which the next keystroke
//! would overwrite). Content is a fixed status message - never the typed or corrected text.

use crate::sys::*;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{mpsc, Mutex};

static HWND_OVERLAY: AtomicIsize = AtomicIsize::new(0);
static TOAST: Mutex<Option<String>> = Mutex::new(None);
const TIMER_HIDE: usize = 1;
const SHOW_MS: u32 = 1800;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

pub struct Overlay;

impl Overlay {
    /// Starts the overlay thread. Returns `None` if the window could not be created.
    pub fn spawn() -> Option<Overlay> {
        let (tx, rx) = mpsc::channel::<bool>();
        std::thread::spawn(move || unsafe { overlay_thread(tx) });
        rx.recv().ok().filter(|ok| *ok).map(|_| Overlay)
    }

    /// Shows `text` near the caret of the foreground window (falls back to the window's bottom edge).
    pub fn show(&self, text: &str) {
        if let Ok(mut t) = TOAST.lock() {
            *t = Some(text.to_string());
        }
        let hwnd = HWND_OVERLAY.load(Ordering::Acquire);
        if hwnd != 0 {
            unsafe {
                PostMessageW(hwnd, WM_APP, 0, 0);
            }
        }
    }
}

unsafe fn overlay_thread(ready: mpsc::Sender<bool>) {
    let module = GetModuleHandleW(std::ptr::null());
    let class = wide("ARF_Overlay");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: module,
        hIcon: 0,
        hCursor: 0,
        hbrBackground: 0,
        lpszMenuName: std::ptr::null(),
        lpszClassName: class.as_ptr(),
        hIconSm: 0,
    };
    if RegisterClassExW(&wc) == 0 {
        let _ = ready.send(false);
        return;
    }
    let hwnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE,
        class.as_ptr(),
        std::ptr::null(),
        WS_POPUP,
        0,
        0,
        10,
        10,
        0,
        0,
        module,
        std::ptr::null_mut(),
    );
    if hwnd == 0 {
        let _ = ready.send(false);
        return;
    }
    SetLayeredWindowAttributes(hwnd, 0, 235, LWA_ALPHA);
    HWND_OVERLAY.store(hwnd, Ordering::Release);
    let _ = ready.send(true);

    let mut msg: MSG = std::mem::zeroed();
    while GetMessageW(&mut msg, 0, 0, 0) > 0 {
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

unsafe fn make_font() -> HFONT {
    let face = wide("Segoe UI");
    CreateFontW(-20, 0, 0, 0, 400, 0, 0, 0, 1, 0, 0, 5, 0, face.as_ptr())
}

/// Where to put a toast of size `w` x `h`: above the caret, or at the bottom of the window.
unsafe fn anchor(w: i32, h: i32) -> (i32, i32) {
    let fg = GetForegroundWindow();
    let mut pid = 0u32;
    let tid = GetWindowThreadProcessId(fg, &mut pid);
    let mut gti: GUITHREADINFO = std::mem::zeroed();
    gti.cbSize = std::mem::size_of::<GUITHREADINFO>() as u32;
    let (mut x, mut y);
    let has_caret = GetGUIThreadInfo(tid, &mut gti) != 0
        && gti.hwndCaret != 0
        && (gti.rcCaret.right > gti.rcCaret.left || gti.rcCaret.bottom > gti.rcCaret.top);
    if has_caret {
        let mut p = POINT {
            x: gti.rcCaret.left,
            y: gti.rcCaret.top,
        };
        ClientToScreen(gti.hwndCaret, &mut p);
        x = p.x;
        y = p.y - h - 10;
    } else {
        let mut r = RECT::default();
        GetWindowRect(fg, &mut r);
        x = r.left + (r.right - r.left - w) / 2;
        y = r.bottom - h - 70;
    }
    let (sw, sh) = (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN));
    x = x.clamp(0, (sw - w).max(0));
    y = y.clamp(0, (sh - h).max(0));
    (x, y)
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_APP => {
            let text = TOAST.lock().ok().and_then(|mut t| t.take());
            if let Some(text) = text {
                let wtext = wide(&text);
                let dc = GetDC(0);
                let font = make_font();
                let old = SelectObject(dc, font);
                let mut size = SIZE::default();
                GetTextExtentPoint32W(dc, wtext.as_ptr(), (wtext.len() - 1) as i32, &mut size);
                SelectObject(dc, old);
                DeleteObject(font);
                ReleaseDC(0, dc);
                let (w, h) = (size.cx + 36, size.cy + 18);
                let (x, y) = anchor(w, h);
                SetWindowRgn(hwnd, CreateRoundRectRgn(0, 0, w + 1, h + 1, 14, 14), 1);
                SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    x,
                    y,
                    w,
                    h,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                );
                // keep the text for painting
                if let Ok(mut t) = TOAST.lock() {
                    *t = Some(text);
                }
                InvalidateRect(hwnd, std::ptr::null(), 1);
                SetTimer(hwnd, TIMER_HIDE, SHOW_MS, std::ptr::null());
            }
            0
        }
        WM_TIMER => {
            KillTimer(hwnd, TIMER_HIDE);
            ShowWindow(hwnd, SW_HIDE);
            0
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = std::mem::zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            GetClientRect(hwnd, &mut rc);
            let brush = CreateSolidBrush(0x0033_2B26); // BGR: dark slate
            FillRect(dc, &rc, brush);
            DeleteObject(brush);
            let text = TOAST.lock().ok().and_then(|t| t.clone());
            if let Some(text) = text {
                let wtext = wide(&text);
                let font = make_font();
                let old = SelectObject(dc, font);
                SetBkMode(dc, TRANSPARENT);
                SetTextColor(dc, 0x00FF_FFFF);
                DrawTextW(
                    dc,
                    wtext.as_ptr(),
                    (wtext.len() - 1) as i32,
                    &mut rc,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX | DT_RTLREADING,
                );
                SelectObject(dc, old);
                DeleteObject(font);
            }
            EndPaint(hwnd, &ps);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
