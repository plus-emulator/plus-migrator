//! Text repair for Arcturus data.
//!
//! Many Arcturus tables are latin1. CMSes connected over latin1 commonly wrote
//! UTF-8 bytes into those columns, so after the server converts latin1 to
//! utf8mb4 for us, "é" arrives as "Ã©". `repair` undoes that: if a string's
//! characters all map back to single cp1252 bytes, and those bytes are valid
//! UTF-8 containing at least one multi-byte sequence, the decoded text is what
//! the user originally typed. Genuine latin1 text ("Café" stored as 0xE9) is
//! not valid UTF-8 and passes through untouched.

/// cp1252 code points for bytes 0x80..=0x9F (MariaDB's "latin1" is cp1252).
const CP1252_HIGH: [(char, u8); 27] = [
    ('€', 0x80), ('‚', 0x82), ('ƒ', 0x83), ('„', 0x84), ('…', 0x85), ('†', 0x86), ('‡', 0x87),
    ('ˆ', 0x88), ('‰', 0x89), ('Š', 0x8A), ('‹', 0x8B), ('Œ', 0x8C), ('Ž', 0x8E), ('‘', 0x91),
    ('’', 0x92), ('“', 0x93), ('”', 0x94), ('•', 0x95), ('–', 0x96), ('—', 0x97), ('˜', 0x98),
    ('™', 0x99), ('š', 0x9A), ('›', 0x9B), ('œ', 0x9C), ('ž', 0x9E), ('Ÿ', 0x9F),
];

fn cp1252_byte(c: char) -> Option<u8> {
    let n = c as u32;
    if n < 0x80 || (0xA0..=0xFF).contains(&n) {
        return Some(n as u8);
    }
    // MariaDB maps the five undefined cp1252 bytes straight to U+0081 etc.
    if matches!(n, 0x81 | 0x8D | 0x8F | 0x90 | 0x9D) {
        return Some(n as u8);
    }
    CP1252_HIGH.iter().find(|(ch, _)| *ch == c).map(|(_, b)| *b)
}

pub fn repair(s: &str) -> std::borrow::Cow<'_, str> {
    if s.is_ascii() {
        return s.into();
    }
    let mut bytes = Vec::with_capacity(s.len());
    for c in s.chars() {
        match cp1252_byte(c) {
            Some(b) => bytes.push(b),
            None => return s.into(),
        }
    }
    match String::from_utf8(bytes) {
        Ok(decoded) if decoded != s => decoded.into(),
        _ => s.into(),
    }
}

/// Truncates to at most `max` characters.
pub fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}
