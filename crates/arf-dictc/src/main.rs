//! Dictionary tools.
//!
//!   arf-dictc build <ar|en> <list.txt> <out.fst> [--top N] [--short-top M] [--min-len L]
//!   arf-dictc eval  <ar_list.txt> <en_list.txt>  [--top N] [--short-top M]
//!
//! List format: one entry per line, `word` or `word count` (frequency-sorted, most frequent first).
//! `--top N`        keep the N most frequent accepted words
//! `--short-top M`  words of up to 3 letters must also rank within the first M (limits accidental matches)
//! `--min-len L`    drop words shorter than L letters (default 2)

use arf_core::dict::{FstDictionary, WordSet};
use arf_core::layouts::{arabic_101_fixture, us_qwerty, LayoutRegistry};
use arf_core::normalize::{letter_count, normalize};
use arf_core::validator::Validator;
use arf_core::{Config, Engine, Lang};
use std::{env, fs, process::ExitCode};

struct Opts {
    top: usize,
    short_top: usize,
    min_len: usize,
}

fn parse_opts(args: &[String]) -> Result<Opts, String> {
    let mut o = Opts {
        top: usize::MAX,
        short_top: usize::MAX,
        min_len: 2,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut num = |name: &str| -> Result<usize, String> {
            it.next()
                .and_then(|v| v.parse().ok())
                .ok_or_else(|| format!("{name} needs a number"))
        };
        match a.as_str() {
            "--top" => o.top = num("--top")?,
            "--short-top" => o.short_top = num("--short-top")?,
            "--min-len" => o.min_len = num("--min-len")?,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(o)
}

/// Accepted words in frequency order with their counts (normalized, letters only, de-duplicated).
fn read_list(lang: Lang, text: &str, min_len: usize) -> Vec<(String, u64)> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(raw) = parts.next() else { continue };
        let count: u64 = parts.next().and_then(|c| c.parse().ok()).unwrap_or(1);
        let w = normalize(lang, raw);
        if letter_count(&w) < min_len || !w.chars().all(|c| c.is_alphabetic()) || has_triple(&w) {
            continue; // too short, not letters only, or noise such as "ههههه" / "aaah"
        }
        if seen.insert(w.clone()) {
            out.push((w, count));
        }
    }
    out
}

fn has_triple(w: &str) -> bool {
    let c: Vec<char> = w.chars().collect();
    c.windows(3).any(|x| x[0] == x[1] && x[1] == x[2])
}

fn select(words: &[(String, u64)], o: &Opts) -> Vec<String> {
    words
        .iter()
        .take(o.top)
        .enumerate()
        .filter(|(rank, (w, _))| w.chars().count() > 3 || *rank < o.short_top)
        .map(|(_, (w, _))| w.clone())
        .collect()
}

fn build(args: &[String]) -> Result<(), String> {
    let [lang, input, output, rest @ ..] = args else {
        return Err("usage: arf-dictc build <ar|en> <list.txt> <out.fst> [options]".into());
    };
    let lang = Lang::from_code(lang).ok_or("language must be 'ar' or 'en'")?;
    let o = parse_opts(rest)?;
    let text = fs::read_to_string(input).map_err(|e| format!("cannot read {input}: {e}"))?;
    let words = select(&read_list(lang, &text, o.min_len), &o);
    let bytes = FstDictionary::build_bytes(lang, &words.join("\n")).map_err(|e| e.to_string())?;
    fs::write(output, &bytes).map_err(|e| format!("cannot write {output}: {e}"))?;
    println!("{output}: {} words, {} bytes", words.len(), bytes.len());
    Ok(())
}

/// Measures, with the static fixture layouts, how the engine behaves on words that are NOT in
/// the dictionary (false positives when typed correctly) and on dictionary words typed in the
/// wrong layout (recall).
fn eval(args: &[String]) -> Result<(), String> {
    let [ar_path, en_path, rest @ ..] = args else {
        return Err("usage: arf-dictc eval <ar_list.txt> <en_list.txt> [options]".into());
    };
    let o = parse_opts(rest)?;
    let ar_all = read_list(
        Lang::Arabic,
        &fs::read_to_string(ar_path).map_err(|e| e.to_string())?,
        o.min_len,
    );
    let en_all = read_list(
        Lang::English,
        &fs::read_to_string(en_path).map_err(|e| e.to_string())?,
        o.min_len,
    );
    let ar_dict = select(&ar_all, &o);
    let en_dict = select(&en_all, &o);
    let total = |all: &[(String, u64)]| all.iter().map(|(_, c)| *c as f64).sum::<f64>();
    let covered = |all: &[(String, u64)], dict: &[String]| {
        let set: std::collections::HashSet<&String> = dict.iter().collect();
        all.iter()
            .filter(|(w, _)| set.contains(w))
            .map(|(_, c)| *c as f64)
            .sum::<f64>()
    };
    println!(
        "dictionary: ar {} words ({:.1}% of corpus tokens), en {} words ({:.1}%)",
        ar_dict.len(),
        100.0 * covered(&ar_all, &ar_dict) / total(&ar_all),
        en_dict.len(),
        100.0 * covered(&en_all, &en_dict) / total(&en_all)
    );

    let validator = Validator::new(
        Box::new(WordSet::from_text(Lang::Arabic, &ar_dict.join("\n"))),
        Box::new(WordSet::from_text(Lang::English, &en_dict.join("\n"))),
        false,
    );
    let mut reg = LayoutRegistry::new();
    reg.add(1, Box::new(us_qwerty()));
    reg.add(2, Box::new(arabic_101_fixture()));
    let mut engine = Engine::new(Config::default(), validator, reg);
    let (en, ar) = (us_qwerty(), arabic_101_fixture());

    // Held-out words: frequent in real text but NOT in the dictionary (3+ letters).
    let in_dict = |d: &[String]| d.iter().cloned().collect::<std::collections::HashSet<_>>();
    let (ar_set, en_set) = (in_dict(&ar_dict), in_dict(&en_dict));
    let held_out = |all: &[(String, u64)], set: &std::collections::HashSet<String>| {
        all.iter()
            .map(|(w, _)| w.clone())
            .filter(|w| !set.contains(w) && w.chars().count() >= 3)
            .take(5000)
            .collect::<Vec<_>>()
    };

    let mut t = 0u64;
    let mut run = |engine: &mut Engine, layout: u64, keys: &[arf_core::Key]| -> bool {
        engine.on_break();
        for k in keys {
            t += 80;
            engine.on_key(*k, layout, t);
        }
        t += 80;
        engine.on_space(t).is_some()
    };

    let mut report = |name: &str,
                      words: &[String],
                      typed_layout: u64,
                      typist: &dyn Fn(&str) -> Vec<arf_core::Key>,
                      engine: &mut Engine| {
        let (mut n, mut hits) = (0usize, 0usize);
        let mut by_len = [(0usize, 0usize); 4]; // 3,4,5,6+
        for w in words {
            let keys = typist(w);
            if keys.len() < 2 || keys.len() != w.chars().count() && !w.contains('ل') {
                continue;
            }
            let hit = run(engine, typed_layout, &keys);
            let b = (w.chars().count().clamp(3, 6)) - 3;
            by_len[b].0 += 1;
            n += 1;
            if hit {
                hits += 1;
                by_len[b].1 += 1;
            }
        }
        let pct = |a: usize, b: usize| {
            if b == 0 {
                0.0
            } else {
                100.0 * a as f64 / b as f64
            }
        };
        println!(
            "{name}: {hits}/{n} = {:.2}%   (len3 {:.2}%, len4 {:.2}%, len5 {:.2}%, len6+ {:.2}%)",
            pct(hits, n),
            pct(by_len[0].1, by_len[0].0),
            pct(by_len[1].1, by_len[1].0),
            pct(by_len[2].1, by_len[2].0),
            pct(by_len[3].1, by_len[3].0)
        );
    };

    println!("\nFALSE POSITIVES (correctly typed, not in dictionary; lower is better):");
    report(
        "  Arabic words typed in Arabic layout ",
        &held_out(&ar_all, &ar_set),
        2,
        &|w| ar.keys_for_text(w),
        &mut engine,
    );
    report(
        "  English words typed in English layout",
        &held_out(&en_all, &en_set),
        1,
        &|w| en.keys_for_text(w),
        &mut engine,
    );

    println!(
        "\nRECALL (dictionary words typed in the wrong layout, 3+ letters; higher is better):"
    );
    let ar3: Vec<String> = ar_dict
        .iter()
        .filter(|w| w.chars().count() >= 3)
        .take(5000)
        .cloned()
        .collect();
    let en3: Vec<String> = en_dict
        .iter()
        .filter(|w| w.chars().count() >= 3)
        .take(5000)
        .cloned()
        .collect();
    report(
        "  Arabic words typed with English layout",
        &ar3,
        1,
        &|w| ar.keys_for_text(w),
        &mut engine,
    );
    report(
        "  English words typed with Arabic layout",
        &en3,
        2,
        &|w| en.keys_for_text(w),
        &mut engine,
    );
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("build") => build(&args[1..]),
        Some("eval") => eval(&args[1..]),
        _ => Err("usage: arf-dictc build|eval ... (see source header)".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
