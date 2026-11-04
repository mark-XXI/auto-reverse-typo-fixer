//! Keyboard layouts backed by the OS: no hand-written tables, every installed layout works.

use crate::sys::*;
use arf_core::layouts::LayoutRegistry;
use arf_core::{Key, KeyboardLayout, Lang};

pub struct WinLayout {
    hkl: HKL,
    lang: Lang,
}

/// Primary language of an HKL (low word is the LANGID; low 10 bits the primary language).
pub fn lang_of_hkl(hkl: HKL) -> Option<Lang> {
    match (hkl as usize & 0xFFFF) & 0x3FF {
        0x01 => Some(Lang::Arabic),
        0x09 => Some(Lang::English),
        _ => None,
    }
}

impl WinLayout {
    pub fn new(hkl: HKL) -> Option<Self> {
        lang_of_hkl(hkl).map(|lang| WinLayout { hkl, lang })
    }
}

impl KeyboardLayout for WinLayout {
    fn lang(&self) -> Lang {
        self.lang
    }

    fn translate(&self, key: Key) -> Option<String> {
        unsafe {
            let vk = MapVirtualKeyExW(key.scan as u32, MAPVK_VSC_TO_VK_EX, self.hkl);
            if vk == 0 {
                return None;
            }
            let mut state = [0u8; 256];
            if key.shift {
                state[VK_SHIFT as usize] = 0x80;
            }
            if key.caps {
                state[VK_CAPITAL as usize] = 0x01;
            }
            let mut buf = [0u16; 8];
            // Flag 4: do not change the keyboard state (dead keys stay untouched).
            let n = ToUnicodeEx(
                vk,
                key.scan as u32,
                state.as_ptr(),
                buf.as_mut_ptr(),
                8,
                4,
                self.hkl,
            );
            if n <= 0 {
                return None; // 0: no text, <0: dead key
            }
            String::from_utf16(&buf[..n as usize]).ok()
        }
    }
}

pub fn installed_layouts() -> Vec<HKL> {
    unsafe {
        let n = GetKeyboardLayoutList(0, std::ptr::null_mut());
        if n <= 0 {
            return Vec::new();
        }
        let mut list = vec![0 as HKL; n as usize];
        let got = GetKeyboardLayoutList(n, list.as_mut_ptr());
        list.truncate(got.max(0) as usize);
        list
    }
}

/// Registers every installed Arabic/English layout (the first of each language is the target).
pub fn register_installed(reg: &mut LayoutRegistry) {
    for hkl in installed_layouts() {
        register(reg, hkl);
    }
}

pub fn register(reg: &mut LayoutRegistry, hkl: HKL) -> bool {
    if reg.contains(hkl as u64) {
        return true;
    }
    match WinLayout::new(hkl) {
        Some(l) => {
            reg.add(hkl as u64, Box::new(l));
            true
        }
        None => false,
    }
}
