//! Cheap checks on the *typed* token that veto any correction.

/// `true` if the typed token should never be touched.
pub fn skip_typed(typed: &str) -> bool {
    if !typed.chars().any(|c| c.is_alphabetic()) {
        return true; // digits / symbols only
    }
    if typed.contains("://") || typed.starts_with("www.") {
        return true;
    }
    if typed.contains('@') && typed.contains('.') {
        return true; // e-mail
    }
    if typed.contains('\\') || typed.contains('/') {
        return true; // path
    }
    if typed.starts_with("--") {
        return true; // CLI flag
    }
    if has_camel_case(typed) || has_snake_case(typed) {
        return true;
    }
    if typed.len() >= 8
        && typed.chars().all(|c| c.is_ascii_hexdigit())
        && typed.chars().any(|c| c.is_ascii_digit())
    {
        return true; // hash-like
    }
    false
}

fn has_camel_case(s: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    c.windows(2)
        .any(|w| w[0].is_ascii_lowercase() && w[1].is_ascii_uppercase())
}

fn has_snake_case(s: &str) -> bool {
    let c: Vec<char> = s.chars().collect();
    c.windows(3)
        .any(|w| w[0].is_alphanumeric() && w[1] == '_' && w[2].is_alphanumeric())
}
