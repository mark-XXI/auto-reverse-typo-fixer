//! Static reference layouts (test fixtures / offline fallback) and the layout registry.
//!
//! The Windows build does NOT rely on these tables: it asks the OS (`ToUnicodeEx`)
//! so that every installed layout works. The Arabic table below is a fixture and must
//! be verified against the real layout before being trusted for anything else.

use crate::types::{scan::TYPING_KEYS, Key, KeyboardLayout, Lang, LayoutId};
use std::collections::HashMap;

pub struct StaticLayout {
    lang: Lang,
    normal: HashMap<u16, &'static str>,
    shifted: HashMap<u16, &'static str>,
    /// CapsLock inverts Shift for ASCII letters (English). Arabic ignores it.
    caps_flips_letters: bool,
}

impl StaticLayout {
    fn build(
        lang: Lang,
        normal: &[&'static str; 47],
        shifted: &[&'static str; 47],
        caps: bool,
    ) -> Self {
        let mut n = HashMap::new();
        let mut s = HashMap::new();
        for (i, sc) in TYPING_KEYS.iter().enumerate() {
            n.insert(*sc, normal[i]);
            s.insert(*sc, shifted[i]);
        }
        StaticLayout {
            lang,
            normal: n,
            shifted: s,
            caps_flips_letters: caps,
        }
    }

    /// Reverse lookup used by tests: the key that types `c` under this layout.
    pub fn key_for_char(&self, c: char) -> Option<Key> {
        let mut buf = [0u8; 4];
        let target: &str = c.encode_utf8(&mut buf);
        for sc in TYPING_KEYS {
            if self.normal[&sc] == target {
                return Some(Key {
                    scan: sc,
                    shift: false,
                    caps: false,
                });
            }
        }
        for sc in TYPING_KEYS {
            if self.shifted[&sc] == target {
                return Some(Key {
                    scan: sc,
                    shift: true,
                    caps: false,
                });
            }
        }
        None
    }

    pub fn keys_for_text(&self, text: &str) -> Vec<Key> {
        text.chars().filter_map(|c| self.key_for_char(c)).collect()
    }
}

impl KeyboardLayout for StaticLayout {
    fn lang(&self) -> Lang {
        self.lang
    }

    fn translate(&self, key: Key) -> Option<String> {
        let base = self.normal.get(&key.scan)?;
        let is_ascii_letter = base.len() == 1 && base.as_bytes()[0].is_ascii_alphabetic();
        let shift = if self.caps_flips_letters && is_ascii_letter {
            key.shift ^ key.caps
        } else {
            key.shift
        };
        let table = if shift { &self.shifted } else { &self.normal };
        table.get(&key.scan).map(|s| s.to_string())
    }
}

const US_NORMAL: [&str; 47] = [
    "`", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "=", "q", "w", "e", "r", "t", "y",
    "u", "i", "o", "p", "[", "]", "\\", "a", "s", "d", "f", "g", "h", "j", "k", "l", ";", "'", "z",
    "x", "c", "v", "b", "n", "m", ",", ".", "/",
];
const US_SHIFTED: [&str; 47] = [
    "~", "!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "_", "+", "Q", "W", "E", "R", "T", "Y",
    "U", "I", "O", "P", "{", "}", "|", "A", "S", "D", "F", "G", "H", "J", "K", "L", ":", "\"", "Z",
    "X", "C", "V", "B", "N", "M", "<", ">", "?",
];
const AR_NORMAL: [&str; 47] = [
    "ذ", "1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "-", "=", "ض", "ص", "ث", "ق", "ف", "غ",
    "ع", "ه", "خ", "ح", "ج", "د", "\\", "ش", "س", "ي", "ب", "ل", "ا", "ت", "ن", "م", "ك", "ط", "ئ",
    "ء", "ؤ", "ر", "لا", "ى", "ة", "و", "ز", "ظ",
];
// Shift layer as reported by the project owner: Shift+[ / Shift+] and Shift+, / Shift+.
// both yield '<' '>' in Arabic, which is exactly the ambiguity the key-based design solves.
const AR_SHIFTED: [&str; 47] = [
    "\u{0651}", "!", "@", "#", "$", "%", "^", "&", "*", "(", ")", "_", "+", "\u{064E}", "\u{064B}",
    "\u{064F}", "\u{064C}", "لإ", "إ", "\u{2018}", "\u{00F7}", "\u{00D7}", "\u{061B}", "<", ">",
    "|", "\u{0650}", "\u{064D}", "]", "[", "لأ", "أ", "\u{0640}", "\u{060C}", "/", ":", "\"", "~",
    "\u{0652}", "}", "{", "لآ", "آ", "\u{2019}", "<", ">", "\u{061F}",
];

pub fn us_qwerty() -> StaticLayout {
    StaticLayout::build(Lang::English, &US_NORMAL, &US_SHIFTED, true)
}

pub fn arabic_101_fixture() -> StaticLayout {
    StaticLayout::build(Lang::Arabic, &AR_NORMAL, &AR_SHIFTED, false)
}

/// Layouts known to the engine, plus which one is the conversion target per language.
#[derive(Default)]
pub struct LayoutRegistry {
    layouts: HashMap<LayoutId, Box<dyn KeyboardLayout>>,
    preferred: HashMap<Lang, LayoutId>,
}

impl LayoutRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a layout; the first layout of a language becomes its preferred target.
    pub fn add(&mut self, id: LayoutId, layout: Box<dyn KeyboardLayout>) {
        let lang = layout.lang();
        self.preferred.entry(lang).or_insert(id);
        self.layouts.insert(id, layout);
    }

    pub fn set_preferred(&mut self, lang: Lang, id: LayoutId) {
        if self.layouts.get(&id).is_some_and(|l| l.lang() == lang) {
            self.preferred.insert(lang, id);
        }
    }

    pub fn get(&self, id: LayoutId) -> Option<&dyn KeyboardLayout> {
        self.layouts.get(&id).map(|b| b.as_ref())
    }

    pub fn contains(&self, id: LayoutId) -> bool {
        self.layouts.contains_key(&id)
    }

    pub fn preferred(&self, lang: Lang) -> Option<LayoutId> {
        self.preferred.get(&lang).copied()
    }
}
