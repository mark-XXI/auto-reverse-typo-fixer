use crate::dict::Dictionary;
use crate::ngram::NgramModel;
use crate::normalize::{core, normalize};
use crate::types::Lang;
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Evidence {
    None,
    /// Probable: n-gram log-likelihood ratio (target model minus typed-language model).
    TierB(f32),
    /// Strong: dictionary hit (possibly after affix stripping).
    TierA,
    /// Symbol-led known file extension such as `.md`. Exempt from the minimum length.
    Extension,
}

pub struct LangData {
    pub dict: Box<dyn Dictionary>,
    pub model: Option<NgramModel>,
}

pub struct Validator {
    ar: LangData,
    en: LangData,
    user_ar: HashSet<String>,
    user_en: HashSet<String>,
}

const AR_PREFIXES: [&str; 12] = [
    "وال", "بال", "كال", "فال", "لل", "ال", "و", "ف", "ب", "ك", "ل", "س",
];
const AR_SUFFIXES: [&str; 14] = [
    "كم", "هم", "هن", "كن", "ها", "نا", "ني", "ون", "ين", "ان", "ات", "ه", "ك", "ي",
];
const MIN_STEM: usize = 3;

const EXTENSIONS: &[&str] = &[
    "md",
    "txt",
    "py",
    "rs",
    "js",
    "ts",
    "jsx",
    "tsx",
    "json",
    "html",
    "htm",
    "css",
    "c",
    "h",
    "cpp",
    "hpp",
    "java",
    "kt",
    "go",
    "rb",
    "php",
    "sh",
    "bat",
    "ps1",
    "sql",
    "yml",
    "yaml",
    "toml",
    "xml",
    "csv",
    "pdf",
    "doc",
    "docx",
    "xls",
    "xlsx",
    "ppt",
    "pptx",
    "zip",
    "rar",
    "7z",
    "png",
    "jpg",
    "jpeg",
    "gif",
    "svg",
    "mp3",
    "mp4",
    "exe",
    "dll",
    "ini",
    "cfg",
    "log",
    "lock",
    "ipynb",
    "dart",
    "lua",
    "cs",
    "swift",
    "tex",
    "env",
    "gitignore",
    "dockerfile",
    "vue",
    "scss",
];

pub fn is_known_extension(token: &str) -> bool {
    match token.strip_prefix('.') {
        Some(ext) => EXTENSIONS.contains(&ext.to_lowercase().as_str()),
        None => false,
    }
}

impl Validator {
    pub fn new(ar: Box<dyn Dictionary>, en: Box<dyn Dictionary>, build_models: bool) -> Self {
        let make = |lang: Lang, dict: Box<dyn Dictionary>| {
            let model = build_models.then(|| {
                let mut m = NgramModel::new();
                dict.for_each_word(&mut |w| {
                    if w.chars().count() >= 3 {
                        m.add_word(w)
                    }
                });
                m
            });
            let _ = lang;
            LangData { dict, model }
        };
        Validator {
            ar: make(Lang::Arabic, ar),
            en: make(Lang::English, en),
            user_ar: HashSet::new(),
            user_en: HashSet::new(),
        }
    }

    fn data(&self, lang: Lang) -> &LangData {
        match lang {
            Lang::Arabic => &self.ar,
            Lang::English => &self.en,
        }
    }

    pub fn add_user_word(&mut self, lang: Lang, word: &str) {
        let w = normalize(lang, core(word));
        match lang {
            Lang::Arabic => self.user_ar.insert(w),
            Lang::English => self.user_en.insert(w),
        };
    }

    pub fn is_user_word(&self, lang: Lang, word: &str) -> bool {
        let w = normalize(lang, core(word));
        match lang {
            Lang::Arabic => self.user_ar.contains(&w),
            Lang::English => self.user_en.contains(&w),
        }
    }

    /// Dictionary membership, including light affix handling (Tier A evidence).
    pub fn is_word(&self, lang: Lang, word: &str) -> bool {
        let w = normalize(lang, core(word));
        if w.is_empty() {
            return false;
        }
        let dict = &self.data(lang).dict;
        if dict.contains(&w) {
            return true;
        }
        match lang {
            Lang::Arabic => arabic_stems(&w).iter().any(|s| dict.contains(s)),
            Lang::English => english_stems(&w).iter().any(|s| dict.contains(s)),
        }
    }

    pub fn tier_b(
        &self,
        target: Lang,
        conv_core: &str,
        typed_lang: Lang,
        typed_core: &str,
    ) -> Option<f32> {
        let tm = self.data(target).model.as_ref()?;
        let sm = self.data(typed_lang).model.as_ref()?;
        if tm.is_empty() || sm.is_empty() {
            return None;
        }
        let t = tm.score(&normalize(target, conv_core));
        let s = sm.score(&normalize(typed_lang, typed_core));
        Some(t - s)
    }

    /// Evidence that `conv` (the keys re-interpreted in the target language) is intended text.
    pub fn judge(&self, target: Lang, conv: &str, typed_lang: Lang, typed: &str) -> Evidence {
        if target == Lang::English && is_known_extension(conv) {
            return Evidence::Extension;
        }
        let cc = core(conv);
        if cc.is_empty() {
            return Evidence::None;
        }
        if self.is_word(target, cc) {
            return Evidence::TierA;
        }
        match self.tier_b(target, cc, typed_lang, core(typed)) {
            Some(s) => Evidence::TierB(s),
            None => Evidence::None,
        }
    }
}

fn arabic_stems(w: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut starts: Vec<usize> = vec![0];
    for p in AR_PREFIXES {
        if w.starts_with(p) {
            starts.push(p.len());
        }
    }
    let mut ends: Vec<usize> = vec![w.len()];
    for s in AR_SUFFIXES {
        if w.ends_with(s) {
            ends.push(w.len() - s.len());
        }
    }
    for &a in &starts {
        for &b in &ends {
            if (a, b) == (0, w.len()) || a >= b {
                continue;
            }
            let stem = &w[a..b];
            if stem.chars().count() >= MIN_STEM {
                out.push(stem);
            }
        }
    }
    out
}

fn english_stems(w: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut push = |s: String| {
        if s.chars().count() >= MIN_STEM {
            out.push(s)
        }
    };
    let undouble = |s: &str| -> Option<String> {
        let c: Vec<char> = s.chars().collect();
        (c.len() >= 2 && c[c.len() - 1] == c[c.len() - 2])
            .then(|| c[..c.len() - 1].iter().collect())
    };
    if let Some(s) = w.strip_suffix("ies") {
        push(format!("{s}y"));
    }
    for suf in ["es", "s", "ly", "er", "'s"] {
        if let Some(s) = w.strip_suffix(suf) {
            push(s.to_string());
        }
    }
    for suf in ["ed", "ing"] {
        if let Some(s) = w.strip_suffix(suf) {
            push(s.to_string());
            push(format!("{s}e"));
            if let Some(u) = undouble(s) {
                push(u);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dict::WordSet;

    fn v(ar: &str, en: &str) -> Validator {
        Validator::new(
            Box::new(WordSet::from_text(Lang::Arabic, ar)),
            Box::new(WordSet::from_text(Lang::English, en)),
            false,
        )
    }

    #[test]
    fn arabic_affixes() {
        let val = v("علي\nكتاب\nسلام", "");
        assert!(val.is_word(Lang::Arabic, "عليكم")); // suffix كم
        assert!(val.is_word(Lang::Arabic, "وعليكم")); // prefix و + suffix كم
        assert!(val.is_word(Lang::Arabic, "السلام")); // prefix ال
        assert!(val.is_word(Lang::Arabic, "بالكتاب")); // prefix بال
        assert!(!val.is_word(Lang::Arabic, "ضصثقف"));
    }

    #[test]
    fn arabic_normalization() {
        let val = v("أحمد", "");
        assert!(val.is_word(Lang::Arabic, "احمد"));
        assert!(val.is_word(Lang::Arabic, "أَحْمَد"));
    }

    #[test]
    fn english_inflections() {
        let val = v("", "run\nmake\nstudy\nwork");
        for w in ["running", "making", "studies", "worked", "works", "Working"] {
            assert!(val.is_word(Lang::English, w), "{w}");
        }
        assert!(!val.is_word(Lang::English, "zzzing"));
    }

    #[test]
    fn extensions() {
        assert!(is_known_extension(".md"));
        assert!(is_known_extension(".PY"));
        assert!(!is_known_extension("md"));
        assert!(!is_known_extension(".qzx"));
    }
}
