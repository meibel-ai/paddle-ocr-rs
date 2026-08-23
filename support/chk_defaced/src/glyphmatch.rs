//! Shared glyph-matching primitives for the defacement detectors ([`crate::pdf_glyph`],
//! [`crate::docx_glyph`]) and the specimen-OCR escalation ([`crate::specimen`]).
//!
//! Centralizes the **Latin-letter filter** and the **"legitimately identical"** predicate (the
//! homoglyph / cross-script / NFKD / ASCII-base filters that keep the semantic-replacement checks
//! false-positive-free). Previously these lived in three near-identical copies; a filter fix in one
//! could silently miss the others. Outline hashing itself lives in [`crate::font::glyph_outline_hash`]
//! so the registry builder and the checkers produce directly comparable hashes.

use unicode_normalization::UnicodeNormalization;
use unicode_script::{Script, UnicodeScript};

/// A **Latin-script** alphabetic character, normalized to lowercase. v1.0 scopes the outline
/// cross-reference to Latin (the safety target) — basic plus accented/extended letters (European
/// languages), not just ASCII; other scripts are not analyzed, so they raise neither findings nor
/// false positives.
pub fn letter_latin(cp: u32) -> Option<char> {
    let c = char::from_u32(cp)?;
    if c.is_alphabetic() && c.script() == Script::Latin {
        c.to_lowercase().next()
    } else {
        None
    }
}

/// The TR39 confusable skeleton of a single character (cross-script homoglyphs collapse to one).
pub fn skel(c: char) -> String {
    unicode_security::confusable_detection::skeleton(&c.to_string()).collect()
}

/// Two letters legitimately share an identical glyph (so a collision is *not* tampering) when they are
/// the same letter, TR39 confusables, from **different scripts** (Latin 'b' / Greek 'β' / Cyrillic 'в'
/// — cross-script glyph-sharing, not the within-script A3 swap), **compatibility-equivalent** (NFKD:
/// Arabic presentation forms, ligatures), fold to the same **ASCII base** (ð / đ / ɖ → "d"), or the
/// common lowercase-'l' / 'i' pair. Callers pass already-lowercased letters (see [`letter_latin`]).
pub fn legitimately_identical(a: char, b: char) -> bool {
    if a == b {
        return true;
    }
    // Cross-script glyph-sharing is legitimate; the A3 attack swaps letters within one script.
    if a.script() != b.script() {
        return true;
    }
    // Compatibility-equivalent encodings (Arabic presentation forms, ligatures) are the same letter.
    if a.to_string().nfkd().eq(b.to_string().nfkd()) {
        return true;
    }
    // Same ASCII base (ð / đ / ɖ → "d", ø → "o"): confusable variants of one base letter, not a swap.
    if let (Some(x), Some(y)) = (deunicode::deunicode_char(a), deunicode::deunicode_char(b)) {
        if !x.is_empty() && x == y {
            return true;
        }
    }
    let mut p = [a, b];
    p.sort_unstable();
    if matches!((p[0], p[1]), ('i', 'l')) {
        return true;
    }
    skel(a) == skel(b)
}

/// Case-insensitive variant used by the specimen path, where an OCR read and the document's claimed
/// character can differ in case (e.g. OCR reads 'M' for a glyph the document extracts as 'm').
pub fn legitimately_identical_ci(a: char, b: char) -> bool {
    let la = a.to_lowercase().next().unwrap_or(a);
    let lb = b.to_lowercase().next().unwrap_or(b);
    la == lb || legitimately_identical(la, lb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_filter() {
        assert_eq!(letter_latin('A' as u32), Some('a'));
        assert_eq!(letter_latin('é' as u32), Some('é'));
        assert_eq!(letter_latin('Α' as u32), None); // Greek
        assert_eq!(letter_latin('1' as u32), None); // digit
    }

    #[test]
    fn homoglyphs_and_real_swaps() {
        assert!(legitimately_identical('a', 'a'));
        assert!(legitimately_identical('a', '\u{0430}')); // Latin a vs Cyrillic а (cross-script)
        assert!(legitimately_identical('b', '\u{03B2}')); // Latin b vs Greek β
        assert!(legitimately_identical('\u{00F0}', '\u{0111}')); // ð vs đ — same ASCII base
        assert!(legitimately_identical('i', 'l'));
        assert!(!legitimately_identical('m', 'd')); // genuine same-script swap
        assert!(!legitimately_identical('r', 'n'));
    }

    #[test]
    fn case_insensitive_variant() {
        assert!(legitimately_identical_ci('M', 'm'));
        assert!(!legitimately_identical_ci('M', 'D'));
    }
}
