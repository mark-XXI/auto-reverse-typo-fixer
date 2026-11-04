//! Data-only configuration: a strict subset of TOML (flat `key = value` lines).
//! Unknown keys and out-of-range values are rejected. No paths or commands are accepted.

use crate::decision::{Config, Sensitivity, UndoPause};
use crate::types::Lang;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub win: bool,
    /// Windows virtual-key code of the main key.
    pub vk: u32,
}

impl Hotkey {
    pub fn parse(s: &str) -> Result<Hotkey, String> {
        let mut h = Hotkey {
            ctrl: false,
            alt: false,
            shift: false,
            win: false,
            vk: 0,
        };
        for part in s.split('+').map(|p| p.trim().to_lowercase()) {
            match part.as_str() {
                "ctrl" | "control" => h.ctrl = true,
                "alt" => h.alt = true,
                "shift" => h.shift = true,
                "win" | "super" => h.win = true,
                key => {
                    if h.vk != 0 {
                        return Err(format!("hotkey '{s}' has more than one main key"));
                    }
                    h.vk =
                        key_to_vk(key).ok_or_else(|| format!("unknown key '{key}' in hotkey"))?;
                }
            }
        }
        if h.vk == 0 {
            return Err(format!("hotkey '{s}' has no main key"));
        }
        Ok(h)
    }
}

impl Hotkey {
    /// Human-readable form, e.g. "Ctrl+Alt+Z".
    pub fn display(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl".into());
        }
        if self.alt {
            parts.push("Alt".into());
        }
        if self.shift {
            parts.push("Shift".into());
        }
        if self.win {
            parts.push("Win".into());
        }
        parts.push(match self.vk {
            0x41..=0x5A | 0x30..=0x39 => (self.vk as u8 as char).to_string(),
            0x70..=0x7B => format!("F{}", self.vk - 0x6F),
            0x08 => "Backspace".into(),
            0x13 => "Pause".into(),
            0x20 => "Space".into(),
            0x2D => "Insert".into(),
            0x2E => "Delete".into(),
            v => format!("0x{v:02X}"),
        });
        parts.join("+")
    }
}

fn key_to_vk(k: &str) -> Option<u32> {
    let b = k.as_bytes();
    if b.len() == 1 && b[0].is_ascii_lowercase() {
        return Some((b[0] - b'a') as u32 + 0x41);
    }
    if b.len() == 1 && b[0].is_ascii_digit() {
        return Some(b[0] as u32);
    }
    if let Some(n) = k.strip_prefix('f').and_then(|n| n.parse::<u32>().ok()) {
        if (1..=12).contains(&n) {
            return Some(0x70 + n - 1);
        }
    }
    match k {
        "backspace" => Some(0x08),
        "pause" => Some(0x13),
        "space" => Some(0x20),
        "insert" => Some(0x2D),
        "delete" => Some(0x2E),
        _ => None,
    }
}

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub engine: Config,
    pub enabled: bool,
    /// Lower-case process names where correction is disabled.
    pub blacklist: Vec<String>,
    pub undo_hotkey: Hotkey,
    /// Show the "corrected" marker.
    pub overlay: bool,
    /// Wait this long after injecting before asking the OS to switch the layout.
    pub switch_delay_ms: u64,
    /// Inject with small pauses between steps (global default).
    pub paced_injection: bool,
    /// Processes that always use paced injection.
    pub paced_apps: Vec<String>,
}

pub const DEFAULT_BLACKLIST: &[&str] = &[
    "cmd.exe",
    "powershell.exe",
    "pwsh.exe",
    "windowsterminal.exe",
    "conhost.exe",
    "code.exe",
    "devenv.exe",
    "idea64.exe",
    "pycharm64.exe",
    "clion64.exe",
    "webstorm64.exe",
    "keepass.exe",
    "keepassxc.exe",
    "bitwarden.exe",
    "1password.exe",
    "mstsc.exe",
    "vmware.exe",
    "virtualbox.exe",
    "notepad.exe",
];

impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            engine: Config::default(),
            enabled: true,
            blacklist: DEFAULT_BLACKLIST.iter().map(|s| s.to_string()).collect(),
            undo_hotkey: Hotkey::parse("ctrl+alt+z").expect("default hotkey"),
            overlay: true,
            switch_delay_ms: 25,
            paced_injection: false,
            paced_apps: vec!["notepad.exe".to_string()],
        }
    }
}

fn parse_str(v: &str) -> Result<String, String> {
    v.strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .map(str::to_string)
        .ok_or_else(|| format!("expected a quoted string, got {v}"))
}

fn parse_list(v: &str) -> Result<Vec<String>, String> {
    let inner = v
        .strip_prefix('[')
        .and_then(|r| r.strip_suffix(']'))
        .ok_or_else(|| format!("expected a list, got {v}"))?;
    inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_str)
        .collect()
}

fn parse_num<T: std::str::FromStr + PartialOrd>(v: &str, lo: T, hi: T) -> Result<T, String> {
    let n: T = v.parse().map_err(|_| format!("invalid number '{v}'"))?;
    if n < lo || n > hi {
        return Err(format!("value '{v}' out of range"));
    }
    Ok(n)
}

fn parse_undo_pause(s: &str) -> Result<UndoPause, String> {
    match s {
        "none" => Ok(UndoPause::None),
        "run" => Ok(UndoPause::Run),
        _ => {
            if let Some(n) = s.strip_prefix("words:") {
                Ok(UndoPause::Words(parse_num(n, 1u32, 1000)?))
            } else if let Some(n) = s.strip_prefix("seconds:") {
                Ok(UndoPause::Seconds(parse_num(n, 1u32, 3600)?))
            } else {
                Err(format!(
                    "expected none | run | words:N | seconds:N, got '{s}'"
                ))
            }
        }
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_str = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_str = !in_str,
            '#' if !in_str => return &line[..i],
            _ => {}
        }
    }
    line
}

pub fn parse(text: &str) -> Result<AppConfig, String> {
    let mut cfg = AppConfig::default();
    for (n, raw) in text.lines().enumerate() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        let (k, v) = line
            .split_once('=')
            .ok_or_else(|| format!("line {}: expected key = value", n + 1))?;
        let (k, v) = (k.trim(), v.trim());
        let err = |e: String| format!("line {}: {k}: {e}", n + 1);
        match k {
            "enabled" => cfg.enabled = v.parse().map_err(|_| err("expected true/false".into()))?,
            "min_len" => cfg.engine.min_len = parse_num(v, 1usize, 10).map_err(err)?,
            "max_run_words" => cfg.engine.max_run_words = parse_num(v, 1usize, 50).map_err(err)?,
            "max_run_chars" => {
                cfg.engine.max_run_chars = parse_num(v, 10usize, 500).map_err(err)?
            }
            "idle_timeout_ms" => {
                cfg.engine.idle_timeout_ms = parse_num(v, 1_000u64, 120_000).map_err(err)?
            }
            "undo_window_ms" => {
                cfg.engine.undo_window_ms = parse_num(v, 500u64, 60_000).map_err(err)?
            }
            "tier_b_strict" => {
                cfg.engine.tier_b_strict = parse_num(v, 0.0f32, 10.0).map_err(err)?
            }
            "tier_b_relaxed" => {
                cfg.engine.tier_b_relaxed = parse_num(v, 0.0f32, 10.0).map_err(err)?
            }
            "sensitivity" => {
                cfg.engine.sensitivity = match parse_str(v).map_err(err)?.as_str() {
                    "conservative" => Sensitivity::Conservative,
                    "balanced" => Sensitivity::Balanced,
                    "aggressive" => Sensitivity::Aggressive,
                    o => return Err(err(format!("unknown sensitivity '{o}'"))),
                }
            }
            "correct_from" => {
                let langs = parse_list(v).map_err(err)?;
                cfg.engine.correct_from = langs
                    .iter()
                    .map(|c| {
                        Lang::from_code(c).ok_or_else(|| err(format!("unknown language '{c}'")))
                    })
                    .collect::<Result<_, _>>()?;
            }
            "blacklist" => {
                cfg.blacklist = parse_list(v)
                    .map_err(err)?
                    .into_iter()
                    .map(|s| s.to_lowercase())
                    .collect()
            }
            "overlay" => cfg.overlay = v.parse().map_err(|_| err("expected true/false".into()))?,
            "paced_injection" => {
                cfg.paced_injection = v.parse().map_err(|_| err("expected true/false".into()))?
            }
            "switch_delay_ms" => cfg.switch_delay_ms = parse_num(v, 0u64, 500).map_err(err)?,
            "paced_apps" => {
                cfg.paced_apps = parse_list(v)
                    .map_err(err)?
                    .into_iter()
                    .map(|s| s.to_lowercase())
                    .collect()
            }
            "after_undo" => {
                cfg.engine.undo_pause =
                    parse_undo_pause(&parse_str(v).map_err(err)?).map_err(err)?
            }
            "undo_hotkey" => {
                cfg.undo_hotkey = Hotkey::parse(&parse_str(v).map_err(err)?).map_err(err)?
            }
            other => return Err(format!("line {}: unknown key '{other}'", n + 1)),
        }
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_overrides() {
        let c = parse("# comment\nmin_len = 4\nsensitivity = \"balanced\" # inline\nundo_hotkey = \"ctrl+shift+f9\"\nblacklist = [\"Game.exe\"]\ncorrect_from = [\"ar\"]").unwrap();
        assert_eq!(c.engine.min_len, 4);
        assert_eq!(c.engine.sensitivity, Sensitivity::Balanced);
        assert_eq!(
            c.undo_hotkey,
            Hotkey {
                ctrl: true,
                alt: false,
                shift: true,
                win: false,
                vk: 0x78
            }
        );
        assert_eq!(c.blacklist, vec!["game.exe"]);
        assert_eq!(c.engine.correct_from, vec![Lang::Arabic]);
    }

    #[test]
    fn strict_validation() {
        assert!(parse("min_len = 99").is_err());
        assert!(parse("unknown = 1").is_err());
        assert!(parse("undo_hotkey = \"ctrl+alt\"").is_err());
        assert!(parse("sensitivity = \"wild\"").is_err());
        assert!(parse("correct_from = [\"fr\"]").is_err());
        assert!(parse("just text").is_err());
    }

    #[test]
    fn undo_policy_values() {
        let p = |v: &str| parse(&format!("after_undo = \"{v}\"")).map(|c| c.engine.undo_pause);
        assert_eq!(p("run"), Ok(UndoPause::Run));
        assert_eq!(p("none"), Ok(UndoPause::None));
        assert_eq!(p("words:5"), Ok(UndoPause::Words(5)));
        assert_eq!(p("seconds:30"), Ok(UndoPause::Seconds(30)));
        assert!(p("words:0").is_err());
        assert!(p("forever").is_err());
    }

    #[test]
    fn hotkey_display() {
        assert_eq!(Hotkey::parse("ctrl+alt+z").unwrap().display(), "Ctrl+Alt+Z");
        assert_eq!(Hotkey::parse("shift+f9").unwrap().display(), "Shift+F9");
    }

    #[test]
    fn notepad_is_blacklisted_by_default() {
        let cfg = AppConfig::default();
        assert!(cfg.blacklist.iter().any(|app| app == "notepad.exe"));
    }
    #[test]
    fn default_hotkey() {
        assert_eq!(AppConfig::default().undo_hotkey.vk, 0x5A);
    }
}
