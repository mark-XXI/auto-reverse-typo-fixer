//! Worker thread: turns raw hook events into engine calls and applies corrections.

use crate::focus::{is_sensitive_host, FocusCtx, FocusMonitor};
use crate::hook::{self, RawEvent};
use crate::inject::{self, Mode};
use crate::layout_win;
use crate::overlay::Overlay;
use crate::sys::HWND;
use crate::uia_guard::{UiaGuard, WinFocus};
use arf_core::config::AppConfig;
use arf_core::types::scan;
use arf_core::{Engine, Key, UndoPause};
use std::collections::VecDeque;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Mods {
    shift: bool,
    ctrl: bool,
    alt: bool,
    win: bool,
    caps: bool,
}

impl Mods {
    /// Returns `true` if `vk` was a modifier key (and updates the state).
    fn update(&mut self, vk: u32, down: bool) -> bool {
        match vk {
            0x10 | 0xA0 | 0xA1 => self.shift = down,
            0x11 | 0xA2 | 0xA3 => self.ctrl = down,
            0x12 | 0xA4 | 0xA5 => self.alt = down,
            0x5B | 0x5C => self.win = down,
            0x14 => {
                if down {
                    self.caps = !self.caps;
                }
            }
            _ => return false,
        }
        true
    }
}

pub struct App {
    engine: Engine,
    cfg: AppConfig,
    rx: Receiver<RawEvent>,
    pending: VecDeque<RawEvent>,
    mods: Mods,
    focus: FocusMonitor,
    uia: UiaGuard,
    last_uia_epoch: Option<u64>,
    last_focus: (HWND, HWND),
    /// Layout we asked the OS to switch to, and when. Verified on a later key press.
    expected: Option<(isize, u64)>,
    overlay: Option<Overlay>,
    start: Instant,
}

impl App {
    pub fn new(
        engine: Engine,
        cfg: AppConfig,
        rx: Receiver<RawEvent>,
        overlay: Option<Overlay>,
    ) -> Self {
        let mods = Mods {
            caps: inject::caps_lock_on(),
            ..Mods::default()
        };
        App {
            engine,
            cfg,
            rx,
            pending: VecDeque::new(),
            mods,
            focus: FocusMonitor::new(),
            uia: UiaGuard::start(),
            last_uia_epoch: None,
            last_focus: (0, 0),
            expected: None,
            overlay,
            start: Instant::now(),
        }
    }

    fn now(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }

    pub fn run(mut self) {
        loop {
            if !hook::event_stream_healthy() {
                self.engine.on_break();
                eprintln!("error: keyboard event queue overflow/disconnect; corrections disabled until restart");
                return;
            }
            let ev = match self.pending.pop_front() {
                Some(e) => e,
                None => match self.rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(e) => e,
                    Err(RecvTimeoutError::Timeout) => {
                        let now = self.now();
                        self.engine.tick(now);
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                },
            };
            self.handle(ev);
        }
    }

    fn handle(&mut self, ev: RawEvent) {
        if !hook::event_stream_healthy() {
            self.engine.on_break();
            return;
        }
        match ev {
            RawEvent::Mouse | RawEvent::Foreign => self.engine.on_break(),
            RawEvent::Key {
                vk,
                scan,
                extended,
                down,
                activity,
            } => self.handle_key(vk, scan, extended, down, activity),
        }
    }

    fn undo_hotkey_matches(&self, vk: u32) -> bool {
        let h = &self.cfg.undo_hotkey;
        h.vk == vk
            && h.ctrl == self.mods.ctrl
            && h.alt == self.mods.alt
            && h.shift == self.mods.shift
            && h.win == self.mods.win
    }

    fn handle_key(&mut self, vk: u32, sc: u16, extended: bool, down: bool, activity: u64) {
        if self.mods.update(vk, down) || !down {
            return;
        }
        let now = self.now();

        // Check context before Undo: never rewrite content in a different window,
        // a password field, or a browser whose focus safety is not yet verified.
        let Some(ctx) = self.focus.current() else {
            self.engine.on_break();
            return;
        };
        if (ctx.hwnd, ctx.focus) != self.last_focus {
            self.last_focus = (ctx.hwnd, ctx.focus);
            self.engine.on_break();
        }
        let verdict = self.uia.verdict(WinFocus {
            hwnd: ctx.hwnd,
            focus: ctx.focus,
            pid: ctx.pid,
        });
        // A focus event must break the buffered run even when the top-level
        // window and the Win32 focus HWND remain unchanged.
        if self.last_uia_epoch != Some(verdict.epoch) {
            self.last_uia_epoch = Some(verdict.epoch);
            self.engine.on_break();
        }
        if !self.cfg.enabled
            || !verdict.allows_text()
            || ctx.is_password
            || is_sensitive_host(&ctx.process)
            || self.cfg.blacklist.contains(&ctx.process)
        {
            self.engine.on_break();
            return;
        }

        if self.undo_hotkey_matches(vk) && self.engine.can_undo(now) {
            self.do_undo(now, &ctx, verdict.epoch, activity);
            return;
        }
        if self.mods.ctrl || self.mods.alt || self.mods.win {
            self.engine.on_break(); // shortcuts: caret/text may change in ways we cannot track
            return;
        }
        layout_win::register(self.engine.layouts_mut(), ctx.hkl);
        self.verify_switch(&ctx, now);

        if extended {
            self.engine.on_break(); // arrows, Home/End, Delete, numpad Enter, ...
            return;
        }
        match sc {
            scan::SPACE => self.on_space(now, &ctx, verdict.epoch, activity),
            scan::BACKSPACE => self.engine.on_backspace(now),
            s if scan::is_typing_key(s) => {
                let key = Key {
                    scan: s,
                    shift: self.mods.shift,
                    caps: self.mods.caps,
                };
                self.engine.on_key(key, ctx.hkl as u64, now);
            }
            _ => self.engine.on_break(), // Enter, Tab, Esc, F-keys, numpad, ...
        }
    }

    /// A later key press tells us whether the OS honored our layout-switch request.
    fn verify_switch(&mut self, ctx: &FocusCtx, now: u64) {
        if let Some((expected, at)) = self.expected {
            if now.saturating_sub(at) >= 150 {
                self.expected = None;
                if ctx.hkl != expected {
                    self.engine.report_layout_switch_failed();
                }
            }
        }
    }

    fn mode_for(&self, process: &str) -> Mode {
        if self.cfg.paced_injection || self.cfg.paced_apps.iter().any(|p| p == process) {
            Mode::Paced
        } else {
            Mode::Batch
        }
    }

    fn toast(&self, text: &str) {
        if self.cfg.overlay {
            if let Some(o) = &self.overlay {
                o.show(text);
            }
        }
    }

    /// Asks the OS to switch layouts, after a short delay so the injected text is consumed first.
    fn switch_layout(&mut self, hkl: u64, now: u64, ctx: &FocusCtx, epoch: u64, activity: u64) {
        if self.cfg.switch_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.cfg.switch_delay_ms));
        }
        // Another real key or mouse click after the injection invalidates the
        // original context. Preserve those events for normal processing.
        while let Ok(ev) = self.rx.try_recv() {
            self.pending.push_back(ev);
        }
        if self.pending.iter().any(RawEvent::is_press)
            || !self.safe_for_activity(ctx, epoch, activity)
        {
            self.engine.on_break();
            self.expected = None;
            return;
        }
        if inject::request_layout_if_foreground(ctx.hwnd, hkl as isize) {
            self.expected = Some((hkl as isize, now));
        } else {
            self.engine.on_break();
            self.expected = None;
        }
    }

    /// Recheck context and event generation immediately before changing text.
    /// This does not make missed UIA events impossible; browser denylist remains.
    fn still_safe(&mut self, original: &FocusCtx, epoch: u64) -> bool {
        let Some(current) = self.focus.current() else {
            return false;
        };
        let verdict = self.uia.verdict(WinFocus {
            hwnd: current.hwnd,
            focus: current.focus,
            pid: current.pid,
        });
        self.cfg.enabled
            && verdict.allows_text()
            && verdict.epoch == epoch
            && current.pid == original.pid
            && current.hwnd == original.hwnd
            && current.focus == original.focus
            && current.hkl == original.hkl
            && !current.is_password
            && !is_sensitive_host(&current.process)
            && !self.cfg.blacklist.contains(&current.process)
    }

    /// A UIA focus verdict alone cannot detect a new physical key pressed inside
    /// the same control. Check the physical-input generation on both sides of
    /// the Win32/UIA focus check to reject an intervening press.
    fn safe_for_activity(&mut self, ctx: &FocusCtx, epoch: u64, activity: u64) -> bool {
        hook::event_stream_healthy()
            && hook::activity_is_current(activity)
            && self.still_safe(ctx, epoch)
            && hook::activity_is_current(activity)
            && hook::event_stream_healthy()
    }

    fn on_space(&mut self, now: u64, ctx: &FocusCtx, epoch: u64, activity: u64) {
        // Multi-call paced rewriting may erase text before it can be restored.
        // Do not attempt automatic corrections in apps configured for Paced mode.
        if self.mode_for(&ctx.process) == Mode::Paced {
            self.engine.on_break();
            return;
        }
        let Some(c) = self.engine.on_space(now) else {
            return;
        };

        // If anything else was pressed since the Space, the text in the application is no
        // longer what we computed: abort instead of deleting the wrong characters.
        while let Ok(ev) = self.rx.try_recv() {
            self.pending.push_back(ev);
        }
        if self.pending.iter().any(RawEvent::is_press)
            || !self.safe_for_activity(ctx, epoch, activity)
        {
            self.engine.on_break();
            return;
        }
        let mode = self.mode_for(&ctx.process);
        if inject::send_replacement_checked(c.backspaces, &c.text, mode, || {
            self.safe_for_activity(ctx, epoch, activity)
        })
        .is_err()
        {
            self.engine.on_break();
            return;
        }
        if !self.safe_for_activity(ctx, epoch, activity) {
            self.engine.on_break();
            return;
        }
        self.engine.commit(&c, now);
        self.switch_layout(c.target_layout, now, ctx, epoch, activity);
        let msg = format!("تم التصحيح  ·  للتراجع: {}", self.cfg.undo_hotkey.display());
        self.toast(&msg);
    }

    fn pause_description(&self) -> String {
        match self.cfg.engine.undo_pause {
            UndoPause::None => "تم التراجع".to_string(),
            UndoPause::Run => "تم التراجع  ·  التصحيح متوقف حتى نهاية الكتابة الحالية".to_string(),
            UndoPause::Words(n) => format!("تم التراجع  ·  التصحيح متوقف للكلمات {n} التالية"),
            UndoPause::Seconds(s) => format!("تم التراجع  ·  التصحيح متوقف {s} ثانية"),
        }
    }

    fn do_undo(&mut self, now: u64, ctx: &FocusCtx, epoch: u64, activity: u64) {
        // Never consume an Undo plan for an unsupported multi-call injection mode.
        if self.mode_for(&ctx.process) == Mode::Paced {
            self.engine.on_break();
            return;
        }
        inject::wait_modifiers_released(500);
        // Waiting for modifiers can outlive the focus which triggered Undo.
        if !self.safe_for_activity(ctx, epoch, activity) {
            self.engine.on_break();
            return;
        }
        // Preview without mutating engine state; only commit after a complete rewrite.
        let Some(plan) = self.engine.prepare_undo(now) else {
            return;
        };
        let mode = self.mode_for(&ctx.process);
        if inject::send_replacement_checked(plan.backspaces, &plan.text, mode, || {
            self.safe_for_activity(ctx, epoch, activity)
        })
        .is_err()
        {
            self.engine.on_break();
            return;
        }
        // Revalidate after SendInput before committing rejection/pause state.
        // If focus or physical input changed, leave Undo uncommitted.
        if !self.safe_for_activity(ctx, epoch, activity) {
            self.engine.on_break();
            return;
        }
        if !self.engine.commit_undo(&plan, now) {
            self.engine.on_break();
            return;
        }
        self.switch_layout(plan.restore_layout, now, ctx, epoch, activity);
        let msg = self.pause_description();
        self.toast(&msg);
    }
}
