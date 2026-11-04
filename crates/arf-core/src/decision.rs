//! Decision engine: when to correct, run-based (multi-word) correction, anti-loop, undo.

use crate::converter::translate_keys;
use crate::keybuffer::{KeyBuffer, WordState};
use crate::layouts::LayoutRegistry;
use crate::normalize::{core, letter_count};
use crate::prefilter::skip_typed;
use crate::types::{Key, Lang, LayoutId};
use crate::validator::{Evidence, Validator};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensitivity {
    /// Tier A (dictionary) evidence only. Default.
    Conservative,
    Balanced,
    Aggressive,
}

/// How long auto-correction stays off after the user presses Undo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UndoPause {
    /// Keep correcting immediately.
    None,
    /// Until the current typing run ends (Enter/Tab, mouse click, focus change, shortcut, idle). Default.
    Run,
    /// For the next N finished words.
    Words(u32),
    /// For N seconds.
    Seconds(u32),
}

#[derive(Clone, Copy, Debug)]
enum Pause {
    Run,
    Words(u32),
    Until(u64),
}

#[derive(Clone, Debug)]
pub struct Config {
    /// Minimum letters (of the converted word) for a standalone correction.
    pub min_len: usize,
    pub sensitivity: Sensitivity,
    pub max_run_words: usize,
    pub max_run_chars: usize,
    pub idle_timeout_ms: u64,
    pub undo_window_ms: u64,
    /// Policy applied after an Undo.
    pub undo_pause: UndoPause,
    /// Tier B threshold (n-gram LLR per char) for standalone corrections in Balanced mode.
    pub tier_b_strict: f32,
    /// Tier B threshold in run context / Aggressive mode.
    pub tier_b_relaxed: f32,
    /// Source languages that may be corrected (the language of the layout the user typed in).
    pub correct_from: Vec<Lang>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            min_len: 3,
            sensitivity: Sensitivity::Conservative,
            max_run_words: 10,
            max_run_chars: 120,
            idle_timeout_ms: 10_000,
            undo_window_ms: 5_000,
            undo_pause: UndoPause::Run,
            tier_b_strict: 1.0,
            tier_b_relaxed: 0.5,
            correct_from: vec![Lang::Arabic, Lang::English],
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct WordFix {
    pub original: String,
    pub corrected: String,
    pub spaces: usize,
}

/// A proposed correction. Nothing is changed until [`Engine::commit`] is called,
/// so the caller can still abort (e.g. if the user typed another key meanwhile).
#[derive(Clone, Debug)]
pub struct Correction {
    first: usize,
    last: usize,
    pub words: Vec<WordFix>,
    /// Characters to delete in the application (the typed words plus their spaces).
    pub backspaces: usize,
    /// Text to type instead (corrected words plus their spaces).
    pub text: String,
    original_text: String,
    pub target_layout: LayoutId,
    pub previous_layout: LayoutId,
    pub target_lang: Lang,
}

#[derive(Clone, Debug, PartialEq)]
pub struct UndoPlan {
    pub backspaces: usize,
    pub text: String,
    pub restore_layout: LayoutId,
}

struct UndoEntry {
    first: usize,
    last: usize,
    original_text: String,
    corrected_chars: usize,
    restore_layout: LayoutId,
    at_ms: u64,
}

pub struct Engine {
    cfg: Config,
    validator: Validator,
    layouts: LayoutRegistry,
    buf: KeyBuffer,
    /// >0 while a layout switch is known to have failed: relaxed thresholds for the next words.
    sticky: u8,
    last: Option<UndoEntry>,
    pause: Option<Pause>,
    last_input_ms: Option<u64>,
}

impl Engine {
    pub fn new(cfg: Config, validator: Validator, layouts: LayoutRegistry) -> Self {
        let buf = KeyBuffer::new(cfg.max_run_words, cfg.max_run_chars, cfg.idle_timeout_ms);
        Engine {
            cfg,
            validator,
            layouts,
            buf,
            sticky: 0,
            last: None,
            pause: None,
            last_input_ms: None,
        }
    }

    pub fn layouts(&self) -> &LayoutRegistry {
        &self.layouts
    }
    pub fn layouts_mut(&mut self) -> &mut LayoutRegistry {
        &mut self.layouts
    }
    pub fn validator_mut(&mut self) -> &mut Validator {
        &mut self.validator
    }
    pub fn config(&self) -> &Config {
        &self.cfg
    }

    // ---- input ----

    /// A typing key (letters, digits, symbols). Returns `false` if the run had to be dropped.
    pub fn on_key(&mut self, key: Key, layout: LayoutId, now_ms: u64) -> bool {
        self.touch(now_ms);
        self.last = None; // undo is valid only until the next regular key
        match self.layouts.get(layout) {
            Some(l) => self.buf.push_key(key, layout, l, now_ms),
            None => {
                self.buf.reset();
                false
            }
        }
    }

    pub fn on_backspace(&mut self, now_ms: u64) {
        self.touch(now_ms);
        self.last = None;
        self.buf.push_backspace(now_ms);
    }

    /// Mouse click, navigation, Enter/Tab/Esc, shortcuts, focus change, paste...
    pub fn on_break(&mut self) {
        self.last = None;
        self.buf.reset();
        if matches!(self.pause, Some(Pause::Run)) {
            self.pause = None; // the run the user rejected is over
        }
    }

    pub fn tick(&mut self, now_ms: u64) {
        self.buf.tick(now_ms);
        if self
            .last_input_ms
            .is_some_and(|l| now_ms.saturating_sub(l) > self.cfg.idle_timeout_ms)
            && matches!(self.pause, Some(Pause::Run))
        {
            self.pause = None;
        }
    }

    /// Idle for longer than the timeout ends a "Run" pause (the run itself is over).
    fn touch(&mut self, now_ms: u64) {
        self.tick(now_ms);
        self.last_input_ms = Some(now_ms);
    }

    /// `true` while auto-correction is suspended by the undo policy.
    pub fn is_paused(&self, now_ms: u64) -> bool {
        match self.pause {
            Some(Pause::Run) => true,
            Some(Pause::Words(n)) => n > 0,
            Some(Pause::Until(t)) => now_ms < t,
            None => false,
        }
    }

    /// Space pressed: finalizes the word and proposes a correction, if warranted.
    pub fn on_space(&mut self, now_ms: u64) -> Option<Correction> {
        self.touch(now_ms);
        self.last = None;
        let idx = self.buf.push_space(now_ms)?;
        if self.consume_pause(now_ms) {
            // Suspended after an Undo: the word stays as typed and can never be rewritten later.
            if let Some(w) = self.buf.run_mut().get_mut(idx) {
                w.state = WordState::Rejected;
            }
            return None;
        }
        let result = self.evaluate(idx);
        self.sticky = self.sticky.saturating_sub(1);
        result
    }

    /// Returns `true` if this finished word falls inside an active pause (and consumes it).
    fn consume_pause(&mut self, now_ms: u64) -> bool {
        match self.pause {
            Some(Pause::Run) => true,
            Some(Pause::Words(n)) => {
                self.pause = if n > 1 {
                    Some(Pause::Words(n - 1))
                } else {
                    None
                };
                n > 0
            }
            Some(Pause::Until(t)) => {
                if now_ms < t {
                    true
                } else {
                    self.pause = None;
                    false
                }
            }
            None => false,
        }
    }

    /// Call after the correction was actually applied.
    pub fn commit(&mut self, c: &Correction, now_ms: u64) {
        for i in c.first..=c.last {
            if let Some(w) = self.buf.run_mut().get_mut(i) {
                w.state = WordState::Converted;
            }
        }
        self.last = Some(UndoEntry {
            first: c.first,
            last: c.last,
            original_text: c.original_text.clone(),
            corrected_chars: c.text.chars().count(),
            restore_layout: c.previous_layout,
            at_ms: now_ms,
        });
    }

    pub fn can_undo(&self, now_ms: u64) -> bool {
        self.last
            .as_ref()
            .is_some_and(|e| now_ms.saturating_sub(e.at_ms) <= self.cfg.undo_window_ms)
    }

    /// Build an Undo plan without consuming it or changing rejection/pause state.
    /// If the OS rewrite fails, the caller must NOT commit this plan.
    pub fn prepare_undo(&self, now_ms: u64) -> Option<UndoPlan> {
        let e = self.last.as_ref()?;
        if now_ms.saturating_sub(e.at_ms) > self.cfg.undo_window_ms {
            return None;
        }
        Some(UndoPlan {
            backspaces: e.corrected_chars,
            text: e.original_text.clone(),
            restore_layout: e.restore_layout,
        })
    }

    /// Finalize a previously prepared Undo after its external rewrite succeeded.
    /// This engine is owned by one worker; no other engine operation can interleave
    /// with its synchronous SendInput call. Verify the plan still matches anyway.
    pub fn commit_undo(&mut self, plan: &UndoPlan, now_ms: u64) -> bool {
        let Some(candidate) = self.last.as_ref() else {
            return false;
        };
        if now_ms.saturating_sub(candidate.at_ms) > self.cfg.undo_window_ms
            || candidate.corrected_chars != plan.backspaces
            || candidate.original_text != plan.text
            || candidate.restore_layout != plan.restore_layout
        {
            return false;
        }
        let e = self.last.take().expect("matching Undo entry was checked");
        // Remember what the user rejected: these words are protected for the rest of the session.
        let rejected: Vec<(Lang, String)> = (e.first..=e.last)
            .filter_map(|i| {
                let w = self.buf.run().get(i)?;
                Some((self.layouts.get(w.layout)?.lang(), w.typed()))
            })
            .collect();
        for (lang, typed) in rejected {
            self.validator.add_user_word(lang, &typed);
        }
        for i in e.first..=e.last {
            if let Some(w) = self.buf.run_mut().get_mut(i) {
                w.state = WordState::Rejected;
            }
        }
        self.pause = match self.cfg.undo_pause {
            UndoPause::None => None,
            UndoPause::Run => Some(Pause::Run),
            UndoPause::Words(n) => Some(Pause::Words(n)),
            UndoPause::Seconds(s) => Some(Pause::Until(now_ms + s as u64 * 1000)),
        };
        true
    }

    /// Convenience API for callers that do not have an external editor rewrite.
    /// The Windows worker uses prepare_undo + commit_undo instead.
    pub fn undo(&mut self, now_ms: u64) -> Option<UndoPlan> {
        let plan = self.prepare_undo(now_ms)?;
        self.commit_undo(&plan, now_ms).then_some(plan)
    }

    /// The OS did not honor the layout switch: apply relaxed thresholds to the next few words.
    pub fn report_layout_switch_failed(&mut self) {
        self.sticky = 3;
    }

    // ---- decisions ----

    fn evaluate(&self, idx: usize) -> Option<Correction> {
        let run = self.buf.run();
        let w = run.get(idx)?;
        if w.state != WordState::Original {
            return None; // one conversion per word
        }
        let typed_lang = self.layouts.get(w.layout)?.lang();
        if !self.cfg.correct_from.contains(&typed_lang) {
            return None;
        }
        let target_id = self.layouts.preferred(typed_lang.other())?;
        let target = self.layouts.get(target_id)?;
        let typed = w.typed();
        if skip_typed(&typed) {
            return None;
        }
        let conv = translate_keys(w.keys(), target)?;
        let ok = if self.sticky > 0 {
            self.qualifies_relaxed(typed_lang, &typed, &conv)
        } else {
            self.qualifies_standalone(typed_lang, &typed, &conv)
        };
        if !ok {
            return None;
        }

        // Walk back through the same run: earlier words typed under the same layout
        // are corrected too, until one is confidently valid as typed (the run boundary).
        let mut first = idx;
        while first > 0 {
            let p = &run[first - 1];
            if p.state != WordState::Original || p.layout != w.layout {
                break;
            }
            let ptyped = p.typed();
            if skip_typed(&ptyped) {
                break;
            }
            let Some(pconv) = translate_keys(p.keys(), target) else {
                break;
            };
            if !self.qualifies_relaxed(typed_lang, &ptyped, &pconv) {
                break;
            }
            first -= 1;
        }

        let mut words = Vec::new();
        let (mut backspaces, mut text, mut original_text) = (0, String::new(), String::new());
        for wi in run.iter().take(idx + 1).skip(first) {
            let corrected = translate_keys(wi.keys(), target)?;
            let original = wi.typed();
            let spaces = " ".repeat(wi.trailing_spaces);
            backspaces += wi.total_chars();
            text.push_str(&corrected);
            text.push_str(&spaces);
            original_text.push_str(&original);
            original_text.push_str(&spaces);
            words.push(WordFix {
                original,
                corrected,
                spaces: wi.trailing_spaces,
            });
        }
        Some(Correction {
            first,
            last: idx,
            words,
            backspaces,
            text,
            original_text,
            target_layout: target_id,
            previous_layout: w.layout,
            target_lang: typed_lang.other(),
        })
    }

    fn typed_is_protected(&self, typed_lang: Lang, typed: &str) -> bool {
        let tc = core(typed);
        self.validator.is_user_word(typed_lang, tc) || self.validator.is_word(typed_lang, tc)
    }

    fn qualifies_standalone(&self, typed_lang: Lang, typed: &str, conv: &str) -> bool {
        if self.typed_is_protected(typed_lang, typed) {
            return false; // valid as typed (or user-protected): the typed word wins
        }
        let letters = letter_count(core(conv));
        match self
            .validator
            .judge(typed_lang.other(), conv, typed_lang, typed)
        {
            Evidence::Extension => true,
            Evidence::TierA => letters >= self.cfg.min_len,
            Evidence::TierB(s) => match self.cfg.sensitivity {
                Sensitivity::Conservative => false,
                Sensitivity::Balanced => letters >= 5 && s >= self.cfg.tier_b_strict,
                Sensitivity::Aggressive => letters >= 4 && s >= self.cfg.tier_b_relaxed,
            },
            Evidence::None => false,
        }
    }

    /// Used for earlier words of a run and in sticky mode: the run is extra evidence,
    /// so no minimum length and a lower Tier B threshold.
    fn qualifies_relaxed(&self, typed_lang: Lang, typed: &str, conv: &str) -> bool {
        if self.typed_is_protected(typed_lang, typed) {
            return false;
        }
        match self
            .validator
            .judge(typed_lang.other(), conv, typed_lang, typed)
        {
            Evidence::Extension | Evidence::TierA => true,
            Evidence::TierB(s) => s >= self.cfg.tier_b_relaxed,
            Evidence::None => false,
        }
    }
}
