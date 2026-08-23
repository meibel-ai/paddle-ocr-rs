//! DOCX backend (a ZIP of OOXML): obfuscated embedded fonts (`word/fonts/*.odttf`, de-obfuscated with
//! the `fontKey` GUID), Unicode hygiene of the `w:t` run text, and hidden text (`w:vanish`).
//! "Structural + font first" strategy: no rendering in v1.

use std::io::Read;
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use regex::Regex;

use crate::finding::{Category, Finding, Report, Severity};
use crate::registry::FontRegistry;
use crate::{font, scan, unicode};

static FONTKEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"w:fontKey="(\{[0-9A-Fa-f-]+\})""#).unwrap());

/// De-obfuscate the first 32 bytes of an OOXML embedded font with the `fontKey` GUID.
/// `reversed`: byte order of the key (the spec uses reverse order; we try both).
fn deobfuscate(odttf: &[u8], guid: &str, reversed: bool) -> Vec<u8> {
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

/// Extract `w:t` text and count `w:vanish` (hidden text) from an OOXML part.
fn extract_text_and_vanish(xml: &str, text: &mut String, vanish: &mut usize) {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut in_t = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if e.name().as_ref() == b"w:t" {
                    in_t = true;
                }
            }
            Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"w:vanish" {
                    *vanish += 1;
                }
            }
            Ok(Event::End(e)) => {
                if e.name().as_ref() == b"w:t" {
                    in_t = false;
                }
            }
            Ok(Event::Text(e)) => {
                if in_t {
                    if let Ok(t) = e.unescape() {
                        text.push_str(&t);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
}

pub fn scan(path: &Path, registry: Option<&FontRegistry>) -> Result<Report> {
    let file = std::fs::File::open(path).with_context(|| format!("opening DOCX {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("DOCX is not a valid ZIP")?;
    let mut report = Report::new(&path.display().to_string(), "docx");

    let meta = crate::metadata::docx_metadata(&mut zip);
    if !meta.is_empty() {
        report.metadata = Some(meta);
    }

    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();

    // 1. Text + hidden text from document.xml, headers/footers, notes. `raw_xml` keeps the OOXML for the
    // run-property (invisible/cloaked text) scan.
    let mut text = String::new();
    let mut raw_xml = String::new();
    let mut vanish = 0usize;
    for name in &names {
        let is_textpart = name == "word/document.xml"
            || (name.starts_with("word/header") && name.ends_with(".xml"))
            || (name.starts_with("word/footer") && name.ends_with(".xml"))
            || name == "word/footnotes.xml"
            || name == "word/endnotes.xml";
        if is_textpart {
            if let Ok(mut zf) = zip.by_name(name) {
                let mut s = String::new();
                if zf.read_to_string(&mut s).is_ok() {
                    extract_text_and_vanish(&s, &mut text, &mut vanish);
                    raw_xml.push_str(&s);
                }
            }
        }
    }
    unicode::scan_text(&text, "document text", &mut report.findings);
    // Invisible / cloaked text (prompt-injection vector): near-white font colour, sub-visible size.
    report.findings.extend(crate::visibility::docx_visibility_scan(&raw_xml));
    if vanish > 0 {
        report.push(Finding::new(
            "DOCX.HIDDEN_VANISH",
            Severity::Medium,
            Category::HiddenContent,
            "document runs",
            format!("{vanish} run(s) with w:vanish (hidden text: present in the extract but not visible)"),
            0.8,
        ));
    }

    // 2. fontKeys from the font table.
    let mut keys: Vec<String> = Vec::new();
    if let Ok(mut zf) = zip.by_name("word/fontTable.xml") {
        let mut s = String::new();
        if zf.read_to_string(&mut s).is_ok() {
            for cap in FONTKEY_RE.captures_iter(&s) {
                keys.push(cap[1].to_string());
            }
        }
    }

    // 3. Obfuscated embedded fonts → de-obfuscate, parse, judge.
    let odttf_names: Vec<String> = names.iter().filter(|n| n.to_ascii_lowercase().ends_with(".odttf")).cloned().collect();
    let mut examined = 0usize;
    for name in &odttf_names {
        let mut raw = Vec::new();
        if let Ok(mut zf) = zip.by_name(name) {
            if zf.read_to_end(&mut raw).is_err() {
                continue;
            }
        } else {
            continue;
        }
        let loc = format!("embedded font {name}");
        let mut judged = false;
        'keys: for guid in &keys {
            for reversed in [true, false] {
                let de = deobfuscate(&raw, guid, reversed);
                let faces = font::parse_data(&de);
                if !faces.is_empty() {
                    for parsed in &faces {
                        scan::judge_embedded_font(parsed, &loc, registry, &mut report.findings);
                    }
                    examined += faces.len();
                    judged = true;
                    break 'keys;
                }
            }
        }
        if !judged {
            report.push(Finding::new(
                "DOCX.FONT_UNREADABLE",
                Severity::Low,
                Category::FontIntegrity,
                &loc,
                "obfuscated embedded font could not be de-obfuscated/parsed with the discovered fontKeys",
                0.4,
            ));
        }
    }

    // Deterministic semantic-replacement (variant A3) check on the embedded outlines: an outline that
    // draws one letter but is reached from a different extracted character. No OCR, no rendering.
    if let Ok(outline) = crate::docx_glyph::docx_outline_scan(path) {
        let mut subs = outline.substitutions;
        if !outline.findings.is_empty() {
            // Collision found: unconfirmed until verified (no render-level OCR for DOCX).
            report.verdict = Some(crate::finding::Verdict::Unconfirmed);
            // Step 1 — deterministic canonical check (registry): confirms the tamper + correct direction.
            if let Some(reg) = registry {
                if let Ok(csubs) = crate::docx_glyph::docx_canonical_substitutions(path, reg) {
                    if !csubs.is_empty() {
                        report.verdict = Some(crate::finding::Verdict::Confirmed);
                        let list = csubs.iter().take(6).map(|(e, d)| format!("'{e}'→'{d}'")).collect::<Vec<_>>().join(", ");
                        report.push(Finding::new(
                            "DOCX.CANONICAL_TAMPER_CONFIRMED",
                            Severity::High,
                            Category::FontIntegrity,
                            "embedded font",
                            format!("registry outline check: {} embedded glyph(s) draw a different letter than the extracted character ({list})", csubs.len()),
                            0.95,
                        ));
                        subs = csubs;
                    }
                }
            }
        }
        if !subs.is_empty() {
            report.phrases = crate::finding::phrase_diffs(&text, &subs);
        }
        report.findings.extend(outline.findings);
    }

    report.fonts_examined = examined;
    Ok(report)
}
