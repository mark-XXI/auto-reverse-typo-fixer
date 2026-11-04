use crate::normalize::normalize;
use crate::types::Lang;
use fst::Streamer;
use std::collections::{BTreeSet, HashSet};

/// Word lookup. Keys are expected to be already normalized for the language.
pub trait Dictionary: Send + Sync {
    fn contains(&self, normalized: &str) -> bool;
    fn for_each_word(&self, f: &mut dyn FnMut(&str));
}

fn clean_lines<'a>(lang: Lang, text: &'a str) -> impl Iterator<Item = String> + 'a {
    text.lines().filter_map(move |l| {
        let w = normalize(lang, l.trim());
        (!w.is_empty() && w.chars().all(|c| c.is_alphabetic())).then_some(w)
    })
}

/// Simple in-memory set (used for tests, the seed lists and user dictionaries).
pub struct WordSet {
    set: HashSet<String>,
}

impl WordSet {
    pub fn from_text(lang: Lang, text: &str) -> Self {
        WordSet {
            set: clean_lines(lang, text).collect(),
        }
    }
}

impl Dictionary for WordSet {
    fn contains(&self, w: &str) -> bool {
        self.set.contains(w)
    }
    fn for_each_word(&self, f: &mut dyn FnMut(&str)) {
        self.set.iter().for_each(|w| f(w));
    }
}

/// Memory-compact dictionary backed by a finite-state transducer.
pub struct FstDictionary {
    set: fst::Set<Vec<u8>>,
}

impl FstDictionary {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, fst::Error> {
        Ok(FstDictionary {
            set: fst::Set::new(bytes)?,
        })
    }

    /// Builds FST bytes from a one-word-per-line list (normalizes, de-duplicates, sorts).
    pub fn build_bytes(lang: Lang, text: &str) -> Result<Vec<u8>, fst::Error> {
        let sorted: BTreeSet<String> = clean_lines(lang, text).collect();
        let mut b = fst::SetBuilder::memory();
        for w in &sorted {
            b.insert(w)?;
        }
        b.into_inner()
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

impl Dictionary for FstDictionary {
    fn contains(&self, w: &str) -> bool {
        self.set.contains(w)
    }
    fn for_each_word(&self, f: &mut dyn FnMut(&str)) {
        let mut s = self.set.stream();
        while let Some(k) = s.next() {
            if let Ok(w) = std::str::from_utf8(k) {
                f(w);
            }
        }
    }
}
