//! In-memory run buffer. Holds only the current run of words (bounded), zeroized on reset/drop.

use crate::types::{Key, KeyboardLayout, LayoutId};
use std::collections::VecDeque;
use zeroize::Zeroize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WordState {
    Original,
    Converted,
    Rejected,
}

#[derive(Debug)]
pub struct Word {
    parts: Vec<(Key, String)>,
    pub layout: LayoutId,
    pub trailing_spaces: usize,
    pub state: WordState,
}

impl Word {
    fn new(layout: LayoutId) -> Self {
        Word {
            parts: Vec::new(),
            layout,
            trailing_spaces: 0,
            state: WordState::Original,
        }
    }
    pub fn keys(&self) -> impl Iterator<Item = &Key> + '_ {
        self.parts.iter().map(|(k, _)| k)
    }
    /// What the word looks like on screen as typed.
    pub fn typed(&self) -> String {
        self.parts.iter().map(|(_, s)| s.as_str()).collect()
    }
    /// Number of characters the word occupies in the target application.
    pub fn typed_chars(&self) -> usize {
        self.parts.iter().map(|(_, s)| s.chars().count()).sum()
    }
    pub fn total_chars(&self) -> usize {
        self.typed_chars() + self.trailing_spaces
    }
}

impl Drop for Word {
    fn drop(&mut self) {
        for (k, s) in self.parts.iter_mut() {
            k.zeroize();
            s.zeroize();
        }
    }
}

pub struct KeyBuffer {
    run: VecDeque<Word>,
    current: Option<Word>,
    max_words: usize,
    max_chars: usize,
    idle_ms: u64,
    last_activity_ms: Option<u64>,
}

impl KeyBuffer {
    pub fn new(max_words: usize, max_chars: usize, idle_ms: u64) -> Self {
        KeyBuffer {
            run: VecDeque::new(),
            current: None,
            max_words,
            max_chars,
            idle_ms,
            last_activity_ms: None,
        }
    }

    fn touch(&mut self, now_ms: u64) {
        if let Some(last) = self.last_activity_ms {
            if now_ms.saturating_sub(last) > self.idle_ms {
                self.reset();
            }
        }
        self.last_activity_ms = Some(now_ms);
    }

    /// Drops everything (run break). Contents are zeroized.
    pub fn reset(&mut self) {
        self.run.clear();
        self.current = None;
    }

    pub fn tick(&mut self, now_ms: u64) {
        if let Some(last) = self.last_activity_ms {
            if now_ms.saturating_sub(last) > self.idle_ms {
                self.reset();
            }
        }
    }

    /// Records a typing key. Returns `false` (and resets) if the key cannot be tracked.
    pub fn push_key(
        &mut self,
        key: Key,
        layout_id: LayoutId,
        layout: &dyn KeyboardLayout,
        now_ms: u64,
    ) -> bool {
        self.touch(now_ms);
        if self.current.as_ref().is_some_and(|c| c.layout != layout_id) {
            self.reset(); // layout changed mid-word
        }
        let Some(text) = layout.translate(key) else {
            self.reset();
            return false;
        };
        self.current
            .get_or_insert_with(|| Word::new(layout_id))
            .parts
            .push((key, text));
        true
    }

    /// Space pressed. Returns the index (in the run) of the word it finished, if any.
    pub fn push_space(&mut self, now_ms: u64) -> Option<usize> {
        self.touch(now_ms);
        match self.current.take() {
            Some(mut w) if !w.parts.is_empty() => {
                w.trailing_spaces = 1;
                self.run.push_back(w);
                self.trim();
                Some(self.run.len() - 1)
            }
            _ => {
                if let Some(last) = self.run.back_mut() {
                    last.trailing_spaces += 1;
                }
                None
            }
        }
    }

    pub fn push_backspace(&mut self, now_ms: u64) {
        self.touch(now_ms);
        if let Some(cur) = self.current.as_mut() {
            if let Some((mut k, mut s)) = cur.parts.pop() {
                k.zeroize();
                s.zeroize();
            }
            if cur.parts.is_empty() {
                self.current = None;
            }
            return;
        }
        let Some(last) = self.run.back_mut() else {
            return;
        };
        if last.state != WordState::Original {
            self.reset();
        } else if last.trailing_spaces > 1 {
            last.trailing_spaces -= 1;
        } else if let Some(mut w) = self.run.pop_back() {
            w.trailing_spaces = 0; // the space was deleted: the word is open again
            self.current = Some(w);
        }
    }

    fn trim(&mut self) {
        loop {
            let chars: usize = self.run.iter().map(Word::total_chars).sum();
            if self.run.len() > self.max_words || (chars > self.max_chars && self.run.len() > 1) {
                self.run.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn run(&self) -> &VecDeque<Word> {
        &self.run
    }
    pub fn run_mut(&mut self) -> &mut VecDeque<Word> {
        &mut self.run
    }
    pub fn has_open_word(&self) -> bool {
        self.current.is_some()
    }
}

impl Drop for KeyBuffer {
    fn drop(&mut self) {
        self.reset();
    }
}
