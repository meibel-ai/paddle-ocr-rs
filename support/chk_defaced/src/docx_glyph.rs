//! DOCX semantic-replacement detector (variant A3) on the **embedded fonts directly** — deterministic,
//! NO rendering and NO OCR, and **font-agnostic** (works for any embedded font, not a specific corpus).
//!
//! Mechanism: a defacement font maps a character to a glyph whose **outline draws a different letter**
//! (the text extracts 'D' while the glyph drawn is 'M'). The attack copies a real glyph into the wrong
//! cmap slot, so across the document's embedded fonts the *same outline* becomes reachable from two
//! different letters. We deobfuscate and parse every embedded font, hash each glyph's outline, and flag
//! any outline reachable from more than one (non-homoglyph) letter: the extracted letter disagrees with
//! the drawn one. The "true" letter is the one that maps to that outline most consistently.
//!
//! Word embeds *full* fonts (not subsets), so a single outline is legitimately reached from hundreds of
//! codepoints the document never uses (Lisu letters shaped like Latin, Arabic presentation forms, roman
//! numerals…). To avoid that noise we restrict the candidate letters to those that actually occur in the
//! document's extracted text — the only characters a reader could be misled by — keeping full Unicode
//! coverage (accented Latin, non-Latin scripts) without the false positives of the full repertoire.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;

use crate::finding::{Category, Finding, OutlineScan, Severity};

// Compiled once (were re-compiled per function / per scan).
static FONTKEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"w:fontKey="(\{[0-9A-Fa-f-]+\})""#).unwrap());
static WT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)<w:t[ >](.*?)</w:t>").unwrap());

/// De-obfuscate the first 32 bytes of an OOXML embedded font with the `fontKey` GUID.
pub(crate) fn deobfuscate(odttf: &[u8], guid: &str, reversed: bool) -> Vec<u8> {
    let hexs: String = guid.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hexs.len() != 32 {
        return odttf.to_vec();
    }
    let mut key = [0u8; 16];
    for i in 0..16 {
        key[i] = u8::from_str_radix(&hexs[i * 2..i * 2 + 2], 16).unwrap_or(0);
    }
    let mut out = odttf.to_vec();
    let n = out.len().min(32);
    for i in 0..n {
        let kb = if reversed { key[15 - (i % 16)] } else { key[i % 16] };
        out[i] ^= kb;
    }
    out
}

use crate::font::glyph_outline_hash;
use crate::glyphmatch::{legitimately_identical, letter_latin as letter};

/// Per-outline tally: how many times each letter maps to this exact outline, across all embedded fonts.
#[derive(Default)]
struct Tally {
    by_letter: HashMap<char, usize>,
}

/// Collect the lowercased alphabetic characters that actually occur in the document's extracted text
/// (`w:t` runs of document.xml, headers/footers, notes). Word embeds *full* fonts, so an outline can be
/// legitimately reached from hundreds of codepoints the document never uses (Lisu letters shaped like
/// Latin, Arabic presentation forms, roman numerals…). We only treat a mapping as suspicious when its
/// extracted character is one the reader could actually encounter — i.e. present in this set.
fn document_letters(zip: &mut zip::ZipArchive<std::fs::File>, names: &[String]) -> HashSet<char> {
    let mut set = HashSet::new();
    for name in names {
        let textpart = name == "word/document.xml"
            || (name.starts_with("word/header") && name.ends_with(".xml"))
            || (name.starts_with("word/footer") && name.ends_with(".xml"))
            || name == "word/footnotes.xml"
            || name == "word/endnotes.xml";
        if !textpart {
            continue;
        }
        let mut s = String::new();
        if zip.by_name(name).map(|mut zf| zf.read_to_string(&mut s)).is_err() {
            continue;
        }
        for cap in WT_RE.captures_iter(&s) {
            for c in cap[1].chars() {
                if c.is_alphabetic() {
                    if let Some(l) = c.to_lowercase().next() {
                        set.insert(l);
                    }
                }
            }
        }
    }
    set
}

/// **Canonical tamper check** for a DOCX (deterministic, no OCR): de-obfuscate each embedded `.odttf`
/// font, and for those whose family is in `registry`, compare glyph outlines to the canonical ones,
/// returning `(extracted_char, true_drawn_char)` where a glyph draws a different letter. Empty when no
/// embedded font is registry-known (→ doubt).
pub fn docx_canonical_substitutions(path: &Path, registry: &crate::registry::FontRegistry) -> Result<Vec<(char, char)>> {
    let file = std::fs::File::open(path).with_context(|| format!("opening DOCX {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("DOCX is not a valid ZIP")?;
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();

    let mut keys: Vec<String> = Vec::new();
    if let Ok(mut zf) = zip.by_name("word/fontTable.xml") {
        let mut s = String::new();
        if zf.read_to_string(&mut s).is_ok() {
            for cap in FONTKEY_RE.captures_iter(&s) {
                keys.push(cap[1].to_string());
            }
        }
    }

    let odttf: Vec<String> =
        names.iter().filter(|n| n.to_ascii_lowercase().ends_with(".odttf")).cloned().collect();
    let mut out = Vec::new();
    for name in &odttf {
        let mut raw = Vec::new();
        if zip.by_name(name).map(|mut zf| zf.read_to_end(&mut raw)).is_err() {
            continue;
        }
        'keys: for guid in &keys {
            for reversed in [true, false] {
                let de = deobfuscate(&raw, guid, reversed);
                let faces = crate::font::parse_data(&de);
                if !faces.is_empty() {
                    for pf in &faces {
                        out.extend(registry.canonical_substitutions(pf));
                    }
                    break 'keys;
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Scan a DOCX's embedded fonts for semantic replacement (variant A3). Deterministic, no OCR.
pub fn docx_outline_scan(path: &Path) -> Result<OutlineScan> {
    let file = std::fs::File::open(path).with_context(|| format!("opening DOCX {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("DOCX is not a valid ZIP")?;
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();

    // Characters the document actually extracts — used to suppress by-design outline sharing in full fonts.
    let doc_letters = document_letters(&mut zip, &names);

    // fontKeys from the font table.
    let mut keys: Vec<String> = Vec::new();
    if let Ok(mut zf) = zip.by_name("word/fontTable.xml") {
        let mut s = String::new();
        if zf.read_to_string(&mut s).is_ok() {
            for cap in FONTKEY_RE.captures_iter(&s) {
                keys.push(cap[1].to_string());
            }
        }
    }

    // outline signature → tally of the letters that map to it.
    let mut sigs: HashMap<u64, Tally> = HashMap::new();
    let odttf: Vec<String> =
        names.iter().filter(|n| n.to_ascii_lowercase().ends_with(".odttf")).cloned().collect();

    for name in &odttf {
        let mut raw = Vec::new();
        if zip.by_name(name).map(|mut zf| zf.read_to_end(&mut raw)).is_err() {
            continue;
        }
        // try each fontKey (both byte orders) until the font parses
        let mut parsed = false;
        'keys: for guid in &keys {
            for reversed in [true, false] {
                let de = deobfuscate(&raw, guid, reversed);
                if let Ok(face) = ttf_parser::Face::parse(&de, 0) {
                    tally_face(&face, &doc_letters, &mut sigs);
                    parsed = true;
                    break 'keys;
                }
            }
        }
        let _ = parsed;
    }

    // A glyph (outline) reachable from >1 distinct letter → the extracted letter disagrees with the
    // drawn one. The most frequent letter is the true (drawn) one; the others are the lies. Aggregate
    // by (drawn, extracted) pair across all colliding glyphs.
    let mut lies: HashMap<(char, char), usize> = HashMap::new();
    for tally in sigs.values() {
        if tally.by_letter.len() < 2 {
            continue;
        }
        let mut letters: Vec<(char, usize)> = tally.by_letter.iter().map(|(c, n)| (*c, *n)).collect();
        letters.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let (truth, _) = letters[0];
        for (lie, n) in letters.iter().skip(1) {
            if legitimately_identical(truth, *lie) {
                continue;
            }
            *lies.entry((truth, *lie)).or_default() += *n;
        }
    }

    // Substitution map: for each extracted `lie`, the most frequent drawn `truth` is the correction.
    let mut best: HashMap<char, (char, usize)> = HashMap::new();
    for (&(truth, lie), &n) in &lies {
        let e = best.entry(lie).or_insert((truth, 0));
        if n > e.1 {
            *e = (truth, n);
        }
    }
    let mut substitutions: Vec<(char, char)> = best.into_iter().map(|(lie, (truth, _))| (lie, truth)).collect();
    substitutions.sort_unstable();

    let mut findings: Vec<Finding> = lies
        .into_iter()
        .map(|((truth, lie), n)| {
            Finding::new(
                "DOCX.GLYPH_SEMANTIC_REPLACEMENT",
                Severity::High,
                Category::FontIntegrity,
                "embedded font",
                format!(
                    "a glyph that draws '{truth}' is mapped from the extracted character '{lie}' ({n}×): the embedded font's cmap lies (semantic replacement / variant A3)"
                ),
                0.9,
            )
        })
        .collect();
    findings.sort_by(|a, b| a.message.cmp(&b.message));
    Ok(OutlineScan { findings, substitutions })
}

/// Per embedded font, the de-obfuscated font bytes plus the `(claimed_char, glyph_id)` pairs the
/// document extracts — input for the specimen-OCR escalation ([`crate::specimen`]). Same extraction and
/// document-text scoping as [`docx_outline_scan`], but it records the claims (so a custom font with no
/// honest anchor is still checkable by OCR). Restricted to glyphs whose extracted character actually
/// occurs in the document, which also bounds the OCR cost on Word's full (non-subset) embedded fonts.
pub fn docx_font_claims(path: &Path) -> Result<Vec<crate::FontClaims>> {
    use std::collections::HashSet;
    let file = std::fs::File::open(path).with_context(|| format!("opening DOCX {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("DOCX is not a valid ZIP")?;
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
    let doc_letters = document_letters(&mut zip, &names);

    let mut keys: Vec<String> = Vec::new();
    if let Ok(mut zf) = zip.by_name("word/fontTable.xml") {
        let mut s = String::new();
        if zf.read_to_string(&mut s).is_ok() {
            for cap in FONTKEY_RE.captures_iter(&s) {
                keys.push(cap[1].to_string());
            }
        }
    }

    let odttf: Vec<String> =
        names.iter().filter(|n| n.to_ascii_lowercase().ends_with(".odttf")).cloned().collect();
    let mut out: Vec<crate::FontClaims> = Vec::new();

    for name in &odttf {
        let mut raw = Vec::new();
        if zip.by_name(name).map(|mut zf| zf.read_to_end(&mut raw)).is_err() {
            continue;
        }
        'keys: for guid in &keys {
            for reversed in [true, false] {
                let de = deobfuscate(&raw, guid, reversed);
                let Ok(face) = ttf_parser::Face::parse(&de, 0) else { continue };
                let Some(cmap) = face.tables().cmap else { break 'keys };
                let mut claims: HashSet<(char, u16)> = HashSet::new();
                for sub in cmap.subtables {
                    if !sub.is_unicode() {
                        continue;
                    }
                    sub.codepoints(|cp| {
                        let Some(l) = letter(cp) else { return };
                        if !doc_letters.contains(&l) {
                            return;
                        }
                        if let (Some(c), Some(gid)) = (char::from_u32(cp), sub.glyph_index(cp)) {
                            claims.insert((c, gid.0));
                        }
                    });
                }
                if !claims.is_empty() {
                    out.push((de, claims.into_iter().collect()));
                }
                break 'keys;
            }
        }
    }
    Ok(out)
}

fn tally_face(face: &ttf_parser::Face, doc_letters: &HashSet<char>, sigs: &mut HashMap<u64, Tally>) {
    let Some(cmap) = face.tables().cmap else { return };
    for sub in cmap.subtables {
        if !sub.is_unicode() {
            continue;
        }
        sub.codepoints(|cp| {
            let Some(letter) = letter(cp) else { return };
            // Only characters the document actually extracts can mislead a reader; ignore the rest
            // (full fonts reuse one outline across many never-used codepoints).
            if !doc_letters.contains(&letter) {
                return;
            }
            if let Some(gid) = sub.glyph_index(cp) {
                if let Some(h) = glyph_outline_hash(face, gid.0) {
                    *sigs.entry(h).or_default().by_letter.entry(letter).or_default() += 1;
                }
            }
        });
    }
}
