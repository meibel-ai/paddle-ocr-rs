//! PDF backend: walks the object graph with `lopdf`, inspects font dictionaries, `ToUnicode` maps,
//! `/Encoding /Differences` and embedded font programs (`FontFile2/3`). Deterministic coherence
//! checks (PUA, modified cmap vs registry, forged identity, multiple subsets = variant B).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use lopdf::{Dictionary, Document, Object};
use regex::Regex;

use crate::finding::{Category, Finding, Report, Severity};
use crate::registry::FontRegistry;
use crate::{font, scan};

// ToUnicode CMap regexes — compiled once (were re-compiled per embedded font).
static BFCHAR_BLK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)beginbfchar(.*?)endbfchar").unwrap());
static BFRANGE_BLK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)beginbfrange(.*?)endbfrange").unwrap());
static PAIR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>").unwrap());
static TRIPLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>").unwrap());
static ARR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>\s*\[([^\]]*)\]").unwrap());
static TOK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([0-9A-Fa-f]+)>").unwrap());

fn deref<'a>(doc: &'a Document, o: &'a Object) -> Option<&'a Object> {
    match o {
        Object::Reference(id) => doc.get_object(*id).ok(),
        x => Some(x),
    }
}
fn dget<'a>(doc: &'a Document, d: &'a Dictionary, k: &[u8]) -> Option<&'a Object> {
    d.get(k).ok().and_then(|o| deref(doc, o))
}
fn as_dict(o: &Object) -> Option<&Dictionary> {
    match o {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}
fn name_of(o: &Object) -> Option<String> {
    match o {
        Object::Name(b) => Some(String::from_utf8_lossy(b).into_owned()),
        _ => None,
    }
}

fn font_descriptor<'a>(doc: &'a Document, fontdict: &'a Dictionary) -> Option<&'a Dictionary> {
    if let Some(fd) = dget(doc, fontdict, b"FontDescriptor").and_then(as_dict) {
        return Some(fd);
    }
    // Type0 → DescendantFonts → FontDescriptor
    if let Some(Object::Array(arr)) = dget(doc, fontdict, b"DescendantFonts") {
        if let Some(first) = arr.first().and_then(|o| deref(doc, o)).and_then(as_dict) {
            return dget(doc, first, b"FontDescriptor").and_then(as_dict);
        }
    }
    None
}

fn embedded_font_bytes(doc: &Document, fontdict: &Dictionary) -> Option<Vec<u8>> {
    let fd = font_descriptor(doc, fontdict)?;
    for key in [b"FontFile2".as_ref(), b"FontFile3".as_ref(), b"FontFile".as_ref()] {
        if let Some(Object::Stream(s)) = dget(doc, fd, key) {
            if let Ok(bytes) = s.decompressed_content() {
                return Some(bytes);
            }
        }
    }
    None
}

/// A ToUnicode destination is "unreadable" if it is in the Private Use Area or is an invisible /
/// zero-width codepoint (BOM, ZWSP/ZWNJ/ZWJ, word joiner, soft hyphen): in both cases the extracted
/// text is garbled while the glyphs render normally.
fn is_unreadable_cp(cp: u32) -> bool {
    font::is_pua(cp) || matches!(cp, 0xFEFF | 0x00AD | 0x200B..=0x200D | 0x2060..=0x2064)
}

/// Count how many UTF-16BE units of a hex destination string are "unreadable" (PUA or invisible).
fn pua_units(hex: &str) -> usize {
    let mut n = 0;
    for chunk in hex.as_bytes().chunks(4) {
        if chunk.len() == 4 {
            if let Ok(cp) = u32::from_str_radix(std::str::from_utf8(chunk).unwrap_or(""), 16) {
                if is_unreadable_cp(cp) {
                    n += 1;
                }
            }
        }
    }
    n
}

/// Count `ToUnicode` Unicode destinations that fall in the PUA. Handles both `bfchar` (`<src> <dst>`)
/// and `bfrange` (`<lo> <hi> <dst>` or `<lo> <hi> [<d0> <d1> …]`) blocks — the destination is the
/// trustworthy token (sources are the font's internal codes).
fn tounicode_pua_count(doc: &Document, fontdict: &Dictionary) -> usize {
    let Some(Object::Stream(s)) = dget(doc, fontdict, b"ToUnicode") else {
        return 0;
    };
    let Ok(content) = s.decompressed_content() else {
        return 0;
    };
    let text = String::from_utf8_lossy(&content);
    let mut pua = 0usize;

    // bfchar: <src> <dst> → the destination is the 2nd token.
    for blk in BFCHAR_BLK.captures_iter(&text) {
        for c in PAIR.captures_iter(&blk[1]) {
            pua += pua_units(&c[2]);
        }
    }

    // bfrange: <lo> <hi> <dst-start> → the destination is the 3rd token (the range maps consecutively,
    // so if dst-start is PUA the whole range is). Also handle the array form `[<d0> <d1> …]`.
    for blk in BFRANGE_BLK.captures_iter(&text) {
        let body = &blk[1];
        for c in TRIPLE.captures_iter(body) {
            pua += pua_units(&c[3]);
        }
        // Array destinations: each entry is a destination codepoint.
        for c in ARR.captures_iter(body) {
            for t in TOK.captures_iter(&c[3]) {
                pua += pua_units(&t[1]);
            }
        }
    }

    pua
}

/// Math/symbol font families that legitimately map glyphs (delimiters, operators, dingbats) to the PUA,
/// because those glyphs have no standard Unicode — so a high PUA-in-ToUnicode count is benign, not a
/// garble attack. Covers Computer Modern / AMS math (CMEX, CMSY, CMMI, MSAM, MSBM) and symbol fonts.
fn is_math_or_symbol_font(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["cmex", "cmsy", "cmmi", "msam", "msbm", "stmary", "wasy", "symbol", "wingding", "webding", "dingbat"]
        .iter()
        .any(|s| n.contains(s))
}

/// Extract the subset tag from a BaseFont like `ABCDEF+Arial` → ("ABCDEF", "Arial").
fn subset_parts(base: &str) -> (Option<&str>, &str) {
    if base.len() > 7 && base.as_bytes().get(6) == Some(&b'+') {
        (Some(&base[..6]), &base[7..])
    } else {
        (None, base)
    }
}

pub fn scan(path: &Path, registry: Option<&FontRegistry>) -> Result<Report> {
    let doc = Document::load(path).with_context(|| format!("opening PDF {}", path.display()))?;
    let mut report = Report::new(&path.display().to_string(), "pdf");
    let meta = crate::metadata::pdf_metadata(&doc);
    if !meta.is_empty() {
        report.metadata = Some(meta);
    }
    let mut subset_families: HashMap<String, HashSet<String>> = HashMap::new();
    let mut examined = 0usize;

    for obj in doc.objects.values() {
        let Some(d) = as_dict(obj) else { continue };
        if dget(&doc, d, b"Type").and_then(name_of).as_deref() != Some("Font") {
            continue;
        }
        let base = dget(&doc, d, b"BaseFont").and_then(name_of).unwrap_or_else(|| "(no BaseFont)".into());
        let loc = format!("font {base}");
        examined += 1;

        let (tag, family) = subset_parts(&base);
        if let Some(tag) = tag {
            subset_families.entry(family.to_string()).or_default().insert(tag.to_string());
        }

        // ToUnicode → PUA / invisible codepoints (variant A1/A2: extracted text garbled).
        // Threshold: legitimate fonts (e.g. Computer Modern / LaTeX) may map a *handful* of glyphs that
        // have no standard Unicode into the PUA — benign noise. A deliberate obfuscation garbles many
        // (the test corpus shows 1-2 for clean PDFs vs 20-150+ for attacks), so we require >= 4.
        const GARBLED_MIN: usize = 4;
        let garbled = tounicode_pua_count(&doc, d);
        if garbled >= GARBLED_MIN && !is_math_or_symbol_font(family) {
            report.push(Finding::new(
                "PDF.TOUNICODE_GARBLED",
                Severity::High,
                Category::FontIntegrity,
                &loc,
                format!("{garbled} ToUnicode mapping(s) to the Private Use Area or invisible/zero-width codepoints: the extracted text will be garbled while the glyphs render normally"),
                0.85,
            ));
        }

        // /Encoding /Differences: custom encoding (potential code redirect).
        if let Some(enc) = dget(&doc, d, b"Encoding").and_then(as_dict) {
            if enc.get(b"Differences").is_ok() {
                report.push(Finding::new(
                    "PDF.CUSTOM_DIFFERENCES",
                    Severity::Info,
                    Category::Structural,
                    &loc,
                    "encoding with /Differences: custom code remapping (legitimate, but it is the vector for glyph↔codepoint redirects)",
                    0.3,
                ));
            }
        }

        // Embedded font program → coherence (registry/PUA).
        if let Some(bytes) = embedded_font_bytes(&doc, d) {
            for parsed in font::parse_data(&bytes) {
                scan::judge_embedded_font(&parsed, &loc, registry, &mut report.findings);
            }
        }
    }

    // Variant B: one family split across many subset/font objects.
    for (family, tags) in &subset_families {
        if tags.len() >= 5 {
            report.push(Finding::new(
                "PDF.MANY_SUBSETS",
                Severity::Medium,
                Category::Structural,
                format!("family {family}"),
                format!("{} distinct subsets of the same family: possible per-page/per-run dynamic font subsetting (variant B)", tags.len()),
                0.6,
            ));
        }
    }

    // Deterministic semantic-replacement (variant A3) check on the embedded outlines: an outline that
    // draws one letter but is reached from a different extracted character. No OCR, no rendering.
    // Reuses the already-loaded `doc` (no second/third PDF parse).
    {
        let outline = crate::pdf_glyph::pdf_outline_scan_doc(&doc);
        let mut subs = outline.substitutions;
        // Gate on the substitution map, not on findings: a suppressed uniform-shift artifact returns an
        // Info note with NO substitutions, and must not set the Unconfirmed verdict or run the registry
        // check — the document is not defaced.
        if !subs.is_empty() {
            // Collision found: unconfirmed until verified.
            report.verdict = Some(crate::finding::Verdict::Unconfirmed);
            // Step 1 — deterministic canonical check (registry): if a registry-known font's glyph draws
            // a different canonical letter, that confirms the tamper and gives the correct direction.
            if let Some(reg) = registry {
                {
                    let csubs = crate::pdf_glyph::pdf_canonical_substitutions_doc(&doc, reg);
                    if !csubs.is_empty() {
                        report.verdict = Some(crate::finding::Verdict::Confirmed);
                        let list = csubs.iter().take(6).map(|(e, d)| format!("'{e}'→'{d}'")).collect::<Vec<_>>().join(", ");
                        report.push(Finding::new(
                            "PDF.CANONICAL_TAMPER_CONFIRMED",
                            Severity::High,
                            Category::FontIntegrity,
                            "embedded font",
                            format!("registry outline check: {} embedded glyph(s) draw a different letter than the extracted character ({list})", csubs.len()),
                            0.95,
                        ));
                        subs = csubs; // correct direction overrides the frequency heuristic
                    }
                }
            }
        }
        if !subs.is_empty() {
            // Affected sentences per page (extracted vs presumed-rendered), so each carries the page it
            // was found on — extract page-by-page rather than the whole document at once.
            let mut phrases = Vec::new();
            for pg in doc.get_pages().keys().copied() {
                if phrases.len() >= 100 {
                    break;
                }
                if let Ok(raw) = doc.extract_text(&[pg]) {
                    phrases.extend(crate::finding::phrase_diffs_paged(&raw, &subs, Some(pg)));
                }
            }
            phrases.truncate(100);
            report.phrases = phrases;
        }
        report.findings.extend(outline.findings);
    }

    // Invisible / cloaked text (prompt-injection vector): white-on-white, sub-visible size, invisible
    // render mode, off-page — orthogonal to font defacement. Reuses the already-loaded `doc`.
    report.findings.extend(crate::visibility::pdf_visibility_scan(&doc));

    report.fonts_examined = examined;
    Ok(report)
}
