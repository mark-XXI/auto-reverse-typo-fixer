//! Engine-level tests using the static fixture layouts and the seed dictionaries.

use crate::converter::translate_keys;
use crate::decision::{Config, Correction, Engine};
use crate::keybuffer::KeyBuffer;
use crate::layouts::{arabic_101_fixture, us_qwerty, LayoutRegistry, StaticLayout};
use crate::ngram::NgramModel;
use crate::seed::{seed_validator, SEED_AR, SEED_EN};
use crate::types::{Key, KeyboardLayout, Lang};

const EN: u64 = 1;
const AR: u64 = 2;

fn engine_with(cfg: Config) -> Engine {
    let mut reg = LayoutRegistry::new();
    reg.add(EN, Box::new(us_qwerty()));
    reg.add(AR, Box::new(arabic_101_fixture()));
    Engine::new(cfg, seed_validator(true), reg)
}

fn engine() -> Engine {
    engine_with(Config::default())
}

struct Typist {
    en: StaticLayout,
    ar: StaticLayout,
    now: u64,
}

impl Typist {
    fn new() -> Self {
        Typist {
            en: us_qwerty(),
            ar: arabic_101_fixture(),
            now: 1_000,
        }
    }

    /// Types `text` (as it would appear under `layout`) and presses Space. Returns the proposal.
    fn word(&mut self, e: &mut Engine, layout: u64, text: &str) -> Option<Correction> {
        let l = if layout == EN { &self.en } else { &self.ar };
        for k in l.keys_for_text(text) {
            self.now += 80;
            e.on_key(k, layout, self.now);
        }
        self.now += 80;
        e.on_space(self.now)
    }
}

// ---------- converter ----------

#[test]
fn spec_examples_convert_by_keys() {
    let (en, ar) = (us_qwerty(), arabic_101_fixture());
    let k = en.keys_for_text("hgsghl");
    assert_eq!(translate_keys(&k, &ar).unwrap(), "السلام");
    let k = en.keys_for_text("ugd;l");
    assert_eq!(translate_keys(&k, &ar).unwrap(), "عليكم");
    let k = ar.keys_for_text("اثممخ");
    assert_eq!(translate_keys(&k, &en).unwrap(), "hello");
    let k = ar.keys_for_text("زةي");
    assert_eq!(translate_keys(&k, &en).unwrap(), ".md");
}

#[test]
fn identical_symbols_are_resolved_through_keys() {
    let (en, ar) = (us_qwerty(), arabic_101_fixture());
    // Shift+[ and Shift+, both print '<' in the Arabic fixture...
    let a = Key {
        scan: 0x1A,
        shift: true,
        caps: false,
    };
    let b = Key {
        scan: 0x33,
        shift: true,
        caps: false,
    };
    assert_eq!(ar.translate(a), ar.translate(b));
    // ...but they are different keys, so the English interpretation differs.
    assert_eq!(en.translate(a).unwrap(), "{");
    assert_eq!(en.translate(b).unwrap(), "<");
}

#[test]
fn caps_and_shift() {
    let (en, ar) = (us_qwerty(), arabic_101_fixture());
    let h = |shift, caps| Key {
        scan: 0x23,
        shift,
        caps,
    };
    assert_eq!(en.translate(h(false, false)).unwrap(), "h");
    assert_eq!(en.translate(h(true, false)).unwrap(), "H");
    assert_eq!(en.translate(h(false, true)).unwrap(), "H");
    assert_eq!(en.translate(h(true, true)).unwrap(), "h");
    assert_eq!(ar.translate(h(false, true)).unwrap(), "ا"); // Arabic ignores CapsLock
}

#[test]
fn multi_char_key_counts_actual_characters() {
    let ar = arabic_101_fixture();
    let b = Key {
        scan: 0x30,
        shift: false,
        caps: false,
    };
    assert_eq!(ar.translate(b).unwrap().chars().count(), 2); // "لا"
    let mut buf = KeyBuffer::new(10, 120, 10_000);
    assert!(buf.push_key(b, AR, &ar, 1));
    assert_eq!(buf.push_space(2), Some(0));
    assert_eq!(buf.run()[0].total_chars(), 3); // two letters + the space
}

// ---------- engine: corrections ----------

#[test]
fn english_keys_to_arabic() {
    let (mut e, mut t) = (engine(), Typist::new());
    let c = t.word(&mut e, EN, "hgsghl").expect("correction");
    assert_eq!(c.text, "السلام ");
    assert_eq!(c.backspaces, 7);
    assert_eq!(c.target_layout, AR);
    assert_eq!(c.previous_layout, EN);
    assert_eq!(c.target_lang, Lang::Arabic);
}

#[test]
fn arabic_keys_to_english() {
    let (mut e, mut t) = (engine(), Typist::new());
    let c = t.word(&mut e, AR, "اثممخ").expect("correction");
    assert_eq!(c.text, "hello ");
    assert_eq!(c.backspaces, 6);
    assert_eq!(c.target_layout, EN);
}

#[test]
fn symbol_led_extension() {
    let (mut e, mut t) = (engine(), Typist::new());
    let c = t.word(&mut e, AR, "زةي").expect("correction");
    assert_eq!(c.text, ".md ");
    assert_eq!(c.backspaces, 4);
}

#[test]
fn digits_are_neutral() {
    let (mut e, mut t) = (engine(), Typist::new());
    let c = t.word(&mut e, EN, "hgsghl1").expect("correction");
    assert_eq!(c.text, "السلام1 ");
    assert_eq!(c.backspaces, 8);
}

#[test]
fn correct_text_is_left_alone() {
    let (mut e, mut t) = (engine(), Typist::new());
    assert!(t.word(&mut e, EN, "hello").is_none());
    assert!(t.word(&mut e, AR, "السلام").is_none());
}

#[test]
fn nonsense_is_left_alone() {
    let (mut e, mut t) = (engine(), Typist::new());
    assert!(t.word(&mut e, EN, "xqzvw").is_none());
}

#[test]
fn minimum_length_applies_to_standalone_words() {
    let (mut e, mut t) = (engine(), Typist::new());
    // "lk" -> "من" is a dictionary word but only 2 letters.
    assert!(t.word(&mut e, EN, "lk").is_none());
}

#[test]
fn prefilter_protects_identifiers() {
    let (mut e, mut t) = (engine(), Typist::new());
    for w in [
        "github.com/hgsghl",
        "camelCase",
        "snake_case",
        "user@host.com",
        "--hgsghl",
        "12345",
    ] {
        assert!(t.word(&mut e, EN, w).is_none(), "{w}");
    }
}

#[test]
fn user_dictionary_protects_words() {
    let (mut e, mut t) = (engine(), Typist::new());
    e.validator_mut().add_user_word(Lang::English, "hgsghl");
    assert!(t.word(&mut e, EN, "hgsghl").is_none());
}

#[test]
fn disabled_source_language() {
    let cfg = Config {
        correct_from: vec![Lang::Arabic],
        ..Config::default()
    };
    let (mut e, mut t) = (engine_with(cfg), Typist::new());
    assert!(t.word(&mut e, EN, "hgsghl").is_none());
    assert!(t.word(&mut e, AR, "اثممخ").is_some());
}

// ---------- engine: runs (multi-word) ----------

#[test]
fn run_walks_back_over_short_words() {
    let (mut e, mut t) = (engine(), Typist::new());
    // "في" typed with English keys is "td": too short alone, rescued by the run.
    assert!(t.word(&mut e, EN, "td").is_none());
    let c = t.word(&mut e, EN, "hgsghl").expect("correction");
    assert_eq!(c.text, "في السلام ");
    assert_eq!(c.backspaces, 10);
    assert_eq!(c.words.len(), 2);
}

#[test]
fn run_stops_at_a_word_valid_as_typed() {
    let (mut e, mut t) = (engine(), Typist::new());
    assert!(t.word(&mut e, EN, "hello").is_none()); // valid English: run boundary
    assert!(t.word(&mut e, EN, "td").is_none());
    let c = t.word(&mut e, EN, "hgsghl").expect("correction");
    assert_eq!(c.text, "في السلام ");
    assert_eq!(c.words.len(), 2);
}

#[test]
fn run_stops_at_unknown_long_words() {
    let (mut e, mut t) = (engine(), Typist::new());
    assert!(t.word(&mut e, EN, "xqzvw").is_none()); // could be a name: not rewritten
    let c = t.word(&mut e, EN, "hgsghl").expect("correction");
    assert_eq!(c.words.len(), 1);
}

#[test]
fn extra_spaces_are_preserved() {
    let (mut e, mut t) = (engine(), Typist::new());
    assert!(t.word(&mut e, EN, "td").is_none());
    t.now += 80;
    assert!(e.on_space(t.now).is_none()); // second space after "td"
    let c = t.word(&mut e, EN, "hgsghl").unwrap();
    assert_eq!(c.text, "في  السلام ");
    assert_eq!(c.backspaces, 2 + 2 + 6 + 1);
}

#[test]
fn run_break_forgets_previous_words() {
    let (mut e, mut t) = (engine(), Typist::new());
    assert!(t.word(&mut e, EN, "td").is_none());
    e.on_break(); // e.g. mouse click
    let c = t.word(&mut e, EN, "hgsghl").unwrap();
    assert_eq!(c.words.len(), 1);
}

#[test]
fn idle_timeout_resets_run() {
    let (mut e, mut t) = (engine(), Typist::new());
    assert!(t.word(&mut e, EN, "td").is_none());
    t.now += 60_000;
    let c = t.word(&mut e, EN, "hgsghl").unwrap();
    assert_eq!(c.words.len(), 1);
}

#[test]
fn layout_change_mid_word_resets() {
    let (mut e, t) = (engine(), Typist::new());
    let k = t.en.keys_for_text("hg");
    e.on_key(k[0], EN, 10);
    e.on_key(k[1], AR, 20); // layout switched by the user in the middle of a word
    assert!(e.on_space(30).is_none()); // run was dropped; the lone key is too short to judge
}

#[test]
fn backspace_edits_the_word() {
    let (mut e, t) = (engine(), Typist::new());
    let k = t.en.keys_for_text("hgsghlx");
    for (i, key) in k.iter().enumerate() {
        e.on_key(*key, EN, 10 + i as u64);
    }
    e.on_backspace(100); // delete the stray 'x'
    let c = e.on_space(110).expect("correction");
    assert_eq!(c.text, "السلام ");
}

// ---------- engine: anti-loop / commit / undo ----------

#[test]
fn proposal_changes_nothing_until_commit() {
    let (mut e, mut t) = (engine(), Typist::new());
    let c = t.word(&mut e, EN, "hgsghl").unwrap();
    assert!(!e.can_undo(t.now)); // not committed (aborted, e.g. user kept typing)
    e.commit(&c, t.now);
    assert!(e.can_undo(t.now));
}

#[test]
fn converted_words_are_never_reevaluated() {
    let (mut e, mut t) = (engine(), Typist::new());
    // "td" is rescued by the run, then committed as Converted.
    assert!(t.word(&mut e, EN, "td").is_none());
    let c = t.word(&mut e, EN, "hgsghl").unwrap();
    e.commit(&c, t.now);
    // The user now types a new wrong-layout word: the old converted words are a boundary.
    let c2 = t.word(&mut e, AR, "اثممخ").unwrap();
    assert_eq!(c2.words.len(), 1);
    assert_eq!(c2.text, "hello ");
}

#[test]
fn undo_restores_original_text_and_layout() {
    let (mut e, mut t) = (engine(), Typist::new());
    assert!(t.word(&mut e, EN, "td").is_none());
    let c = t.word(&mut e, EN, "hgsghl").unwrap();
    e.commit(&c, t.now);
    let u = e.undo(t.now + 1_000).expect("undo");
    assert_eq!(u.text, "td hgsghl ");
    assert_eq!(u.backspaces, "في السلام ".chars().count());
    assert_eq!(u.restore_layout, EN);
    assert!(e.undo(t.now + 1_001).is_none()); // single entry
}

#[test]
fn failed_external_undo_does_not_consume_engine_entry() {
    let (mut e, mut t) = (engine(), Typist::new());
    let correction = t.word(&mut e, EN, "hgsghl").expect("correction");
    e.commit(&correction, t.now);
    let preview = e.prepare_undo(t.now + 100).expect("prepare Undo");
    // Simulate an external injection failure: no commit_undo call takes place.
    assert!(e.can_undo(t.now + 150));
    assert!(!e.is_paused(t.now + 150));
    assert_eq!(e.prepare_undo(t.now + 150), Some(preview.clone()));
    // Success commits the exact plan and applies the Undo pause/rejection.
    assert!(e.commit_undo(&preview, t.now + 150));
    assert!(!e.can_undo(t.now + 150));
    assert!(e.is_paused(t.now + 150));
    assert!(e.prepare_undo(t.now + 150).is_none());
}

#[test]
fn invalid_undo_plan_cannot_change_engine_state() {
    let (mut e, mut t) = (engine(), Typist::new());
    let correction = t.word(&mut e, EN, "hgsghl").expect("correction");
    e.commit(&correction, t.now);
    let mut plan = e.prepare_undo(t.now + 100).expect("prepare Undo");
    plan.backspaces += 1;
    assert!(!e.commit_undo(&plan, t.now + 100));
    assert!(e.can_undo(t.now + 100));
    assert!(!e.is_paused(t.now + 100));
}

#[test]
fn expired_undo_cannot_be_prepared_or_committed() {
    let (mut e, mut t) = (engine(), Typist::new());
    let correction = t.word(&mut e, EN, "hgsghl").expect("correction");
    e.commit(&correction, t.now);
    let plan = e.prepare_undo(t.now + 100).expect("prepare Undo");
    assert!(e.prepare_undo(t.now + 10_000).is_none());
    assert!(!e.commit_undo(&plan, t.now + 10_000));
    assert!(!e.is_paused(t.now + 10_000));
}

#[test]
fn undo_expires_and_is_cleared_by_next_key() {
    let (mut e, mut t) = (engine(), Typist::new());
    let c = t.word(&mut e, EN, "hgsghl").unwrap();
    e.commit(&c, t.now);
    assert!(e.undo(t.now + 10_000).is_none()); // window is 5 s

    let c = t.word(&mut e, EN, "hgsghl").unwrap();
    e.commit(&c, t.now);
    e.on_key(
        Key {
            scan: 0x1E,
            shift: false,
            caps: false,
        },
        AR,
        t.now + 10,
    );
    assert!(!e.can_undo(t.now + 20));
}

#[test]
fn rejected_words_are_not_dragged_into_later_corrections() {
    use crate::decision::UndoPause;
    let cfg = Config {
        undo_pause: UndoPause::None,
        ..Config::default()
    };
    let (mut e, mut t) = (engine_with(cfg), Typist::new());
    assert!(t.word(&mut e, EN, "td").is_none());
    let c = t.word(&mut e, EN, "hgsghl").unwrap();
    e.commit(&c, t.now);
    e.undo(t.now + 100).unwrap();
    let c2 = t.word(&mut e, EN, "ugd;l").unwrap();
    assert_eq!(c2.words.len(), 1);
}

#[test]
fn sticky_mode_relaxes_thresholds_after_failed_switch() {
    let (mut e, mut t) = (engine(), Typist::new());
    assert!(t.word(&mut e, EN, "lk").is_none()); // too short normally
    e.report_layout_switch_failed();
    // The new word qualifies in relaxed mode and, as part of the same run,
    // pulls the earlier short word along with it.
    let c = t.word(&mut e, EN, "lk").expect("relaxed");
    assert_eq!(c.text, "من من ");
    assert_eq!(c.words.len(), 2);
}

// ---------- n-gram (Tier B) ----------

#[test]
fn ngram_prefers_trained_language() {
    let mut ar = NgramModel::new();
    for w in SEED_AR.lines().filter(|w| w.chars().count() >= 3) {
        ar.add_word(w);
    }
    let mut en = NgramModel::new();
    for w in SEED_EN.lines().filter(|w| w.chars().count() >= 3) {
        en.add_word(w);
    }
    assert!(ar.score("السلام") > ar.score("ملسمالا"));
    assert!(en.score("hello") > en.score("lhloe"));
}

#[test]
fn tier_b_is_off_in_conservative_mode() {
    // An unknown-but-plausible Arabic word must not be corrected in the default mode.
    let (mut e, mut t) = (engine(), Typist::new());
    // "ابلغتك" is not in the seed dictionary
    let l = us_qwerty();
    let ar = arabic_101_fixture();
    let keys = ar.keys_for_text("ابلغتك");
    let typed_en = translate_keys(&keys, &l).unwrap();
    assert!(t.word(&mut e, EN, &typed_en).is_none());
}

// ---------- undo policy ----------

fn undone_engine(pause: crate::decision::UndoPause) -> (Engine, Typist) {
    let cfg = Config {
        undo_pause: pause,
        ..Config::default()
    };
    let (mut e, mut t) = (engine_with(cfg), Typist::new());
    let c = t.word(&mut e, EN, "hgsghl").unwrap();
    e.commit(&c, t.now);
    e.undo(t.now + 100).unwrap();
    t.now += 200;
    (e, t)
}

#[test]
fn after_undo_the_same_word_is_protected() {
    let (mut e, mut t) = undone_engine(crate::decision::UndoPause::None);
    assert!(t.word(&mut e, EN, "hgsghl").is_none()); // user rejected it: not corrected again
    assert!(t.word(&mut e, EN, "ugd;l").is_some()); // other words are fine (policy: none)
}

#[test]
fn undo_pause_run_lasts_until_the_run_ends() {
    use crate::decision::UndoPause;
    let (mut e, mut t) = undone_engine(UndoPause::Run);
    assert!(e.is_paused(t.now));
    assert!(t.word(&mut e, EN, "ugd;l").is_none());
    assert!(t.word(&mut e, AR, "اثممخ").is_none());
    e.on_break(); // Enter / click / focus change
    assert!(!e.is_paused(t.now));
    assert!(t.word(&mut e, AR, "اثممخ").is_some());
}

#[test]
fn undo_pause_run_ends_after_idle() {
    use crate::decision::UndoPause;
    let (mut e, mut t) = undone_engine(UndoPause::Run);
    assert!(t.word(&mut e, EN, "ugd;l").is_none());
    t.now += 20_000; // user stops typing
    assert!(t.word(&mut e, AR, "اثممخ").is_some());
}

#[test]
fn undo_pause_words() {
    use crate::decision::UndoPause;
    let (mut e, mut t) = undone_engine(UndoPause::Words(2));
    assert!(t.word(&mut e, EN, "ugd;l").is_none());
    assert!(t.word(&mut e, AR, "اثممخ").is_none());
    assert!(t.word(&mut e, AR, "اثممخ").is_some()); // third word after the undo
}

#[test]
fn undo_pause_seconds() {
    use crate::decision::UndoPause;
    let (mut e, mut t) = undone_engine(UndoPause::Seconds(30));
    assert!(t.word(&mut e, EN, "ugd;l").is_none());
    t.now += 31_000;
    e.on_break();
    assert!(t.word(&mut e, EN, "ugd;l").is_some());
}

#[test]
fn paused_words_are_never_rewritten_later() {
    use crate::decision::UndoPause;
    let (mut e, mut t) = undone_engine(UndoPause::Words(1));
    assert!(t.word(&mut e, EN, "td").is_none()); // skipped during the pause (rejected)
    let c = t.word(&mut e, EN, "ugd;l").unwrap();
    assert_eq!(c.words.len(), 1); // "td" is not dragged into the rewrite
}
