use crate::types::{Key, KeyboardLayout};

/// Translates a key sequence through `layout`. `None` if any key has no text.
pub fn translate_keys<'a>(
    keys: impl IntoIterator<Item = &'a Key>,
    layout: &dyn KeyboardLayout,
) -> Option<String> {
    let mut out = String::new();
    for k in keys {
        out.push_str(&layout.translate(*k)?);
    }
    Some(out)
}
