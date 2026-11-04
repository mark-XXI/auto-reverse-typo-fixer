use std::collections::{HashMap, HashSet};

const BOS: char = '\u{2}';
const EOS: char = '\u{3}';
const K: f32 = 0.05;

/// Character trigram model with add-k smoothing. Used for Tier B evidence only.
#[derive(Default)]
pub struct NgramModel {
    tri: HashMap<[char; 3], u32>,
    ctx: HashMap<[char; 2], u32>,
    seen: HashSet<char>,
}

impl NgramModel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_word(&mut self, w: &str) {
        let chars: Vec<char> = [BOS, BOS]
            .into_iter()
            .chain(w.chars())
            .chain([EOS])
            .collect();
        self.seen.extend(w.chars());
        for win in chars.windows(3) {
            *self.tri.entry([win[0], win[1], win[2]]).or_insert(0) += 1;
            *self.ctx.entry([win[0], win[1]]).or_insert(0) += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tri.is_empty()
    }

    /// Average log-probability per character (higher = more language-like).
    pub fn score(&self, w: &str) -> f32 {
        let chars: Vec<char> = [BOS, BOS]
            .into_iter()
            .chain(w.chars())
            .chain([EOS])
            .collect();
        let v = (self.seen.len() + 1) as f32;
        let mut total = 0.0f32;
        let mut n = 0u32;
        for win in chars.windows(3) {
            let t = *self.tri.get(&[win[0], win[1], win[2]]).unwrap_or(&0) as f32;
            let c = *self.ctx.get(&[win[0], win[1]]).unwrap_or(&0) as f32;
            total += ((t + K) / (c + K * v)).ln();
            n += 1;
        }
        if n == 0 {
            f32::NEG_INFINITY
        } else {
            total / n as f32
        }
    }
}
