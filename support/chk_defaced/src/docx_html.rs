//! Render-input generator: turns a DOCX into a **self-contained HTML** page that, when opened in any
//! browser/webview, reproduces *what a human sees* — the embedded (possibly tampered) fonts applied to
//! the run text. This is the renderer-agnostic prerequisite for the render-OCR fallback on DOCX: a
//! faithful engine (WebView2/WKWebView/WebKitGTK) applies each `@font-face` cmap and draws the glyphs the
//! font actually carries, so a screenshot + OCR recovers the *rendered* text to compare with the
//! *extracted* `w:t` text. Converters that re-shape clean glyphs (Skia/Typst DOCX→PDF) launder the
//! attack; a browser does not.
//!
//! The page is fully inline (fonts as base64 `@font-face`, no external refs) so it is safe under a strict
//! CSP and works air-gapped. No rendering happens here — only HTML synthesis.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};

use crate::docx_glyph::deobfuscate;

/// One embedded face declared in `word/fontTable.xml`: the family it belongs to, its weight/style slot,
/// the relationship id pointing at the `.odttf`, and the `fontKey` GUID to de-obfuscate it.
struct EmbeddedFace {
    family: String,
    bold: bool,
    italic: bool,
    rid: String,
    font_key: String,
}

/// A run of text with the resolved font family, weight/style, colour and size — so the render is
/// faithful to what a human sees (needed to reproduce hidden text: white-on-white, sub-visible size).
struct Run {
    text: String,
    family: String,
    bold: bool,
    italic: bool,
    color: Option<String>, // `w:color w:val` hex (RRGGBB), absent = default
    size_pt: Option<f32>,  // `w:sz` in points (half-points / 2)
}

/// Build a self-contained HTML page reproducing the DOCX's rendered text with its embedded fonts.
pub fn docx_to_html(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("opening DOCX {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("DOCX is not a valid ZIP")?;
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();

    let faces = parse_font_table(&mut zip);
    let rels = parse_font_rels(&mut zip);

    // Emit one @font-face per embedded face we can de-obfuscate and base64-encode.
    let mut font_faces = String::new();
    for face in &faces {
        let Some(target) = rels.get(&face.rid) else { continue };
        let part = normalize_part("word", target);
        let mut raw = Vec::new();
        if zip.by_name(&part).map(|mut zf| zf.read_to_end(&mut raw)).is_err() {
            continue;
        }
        // De-obfuscate with the declared fontKey; accept whichever byte order yields a parseable SFNT.
        let de = first_parseable(&raw, &face.font_key);
        let Some(de) = de else { continue };
        let b64 = base64_encode(&de);
        font_faces.push_str(&format!(
            "@font-face{{font-family:'{fam}';font-weight:{w};font-style:{s};src:url(data:font/ttf;base64,{b64}) format('truetype');}}\n",
            fam = css_escape(&face.family),
            w = if face.bold { "bold" } else { "normal" },
            s = if face.italic { "italic" } else { "normal" },
        ));
    }

    // Document body: runs grouped into paragraphs, each span carrying its resolved family/weight/style.
    let paragraphs = parse_document_runs(&mut zip, &names);
    let mut body = String::new();
    for para in &paragraphs {
        body.push_str("<p>");
        for run in para {
            if run.text.is_empty() {
                continue;
            }
            let color = run.color.as_ref().map(|c| format!("color:#{c};")).unwrap_or_default();
            let size = run.size_pt.map(|p| format!("font-size:{p}pt;")).unwrap_or_default();
            body.push_str(&format!(
                "<span style=\"font-family:'{fam}';font-weight:{w};font-style:{s};{color}{size}\">{txt}</span>",
                fam = css_escape(&run.family),
                w = if run.bold { "bold" } else { "normal" },
                s = if run.italic { "italic" } else { "normal" },
                txt = html_escape(&run.text),
            ));
        }
        body.push_str("</p>\n");
    }

    Ok(format!(
        "<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n<style>\n{font_faces}\
         body{{font-size:18px;line-height:1.5;color:#000;background:#fff;margin:24px;}}\n\
         p{{margin:0 0 0.4em 0;white-space:pre-wrap;}}\n</style></head>\n<body>\n{body}</body></html>\n"
    ))
}

/// The body run text (`word/document.xml`) as plain text — the exact counterpart of what
/// [`docx_to_html`] renders (both exclude headers/footers/notes), so the render-OCR comparison is
/// apples-to-apples. Runs are concatenated; paragraphs are newline-separated.
pub fn docx_body_text(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path).with_context(|| format!("opening DOCX {}", path.display()))?;
    let mut zip = zip::ZipArchive::new(file).context("DOCX is not a valid ZIP")?;
    let names: Vec<String> = zip.file_names().map(|s| s.to_string()).collect();
    let paragraphs = parse_document_runs(&mut zip, &names);
    let mut out = String::new();
    for para in &paragraphs {
        for run in para {
            out.push_str(&run.text);
        }
        out.push('\n');
    }
    Ok(out)
}

/// Resolve a relationship target (relative to `word/_rels/`) against the `word` base. Targets are
/// usually `fonts/fontN.odttf`; a leading `/` means package-absolute.
fn normalize_part(base: &str, target: &str) -> String {
    if let Some(stripped) = target.strip_prefix('/') {
        stripped.to_string()
    } else {
        format!("{base}/{target}")
    }
}

/// Parse `word/fontTable.xml` into the embedded faces, capturing the family and each embed slot's
/// `r:id` + `w:fontKey` + weight/style.
fn parse_font_table(zip: &mut zip::ZipArchive<std::fs::File>) -> Vec<EmbeddedFace> {
    use quick_xml::events::Event;
    let mut out = Vec::new();
    let mut s = String::new();
    if zip.by_name("word/fontTable.xml").map(|mut zf| zf.read_to_string(&mut s)).is_err() {
        return out;
    }
    let mut reader = quick_xml::Reader::from_str(&s);
    let mut current = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                match name.as_ref() {
                    b"w:font" => {
                        current = attr(&e, b"w:name").unwrap_or_default();
                    }
                    b"w:embedRegular" | b"w:embedBold" | b"w:embedItalic" | b"w:embedBoldItalic" => {
                        let rid = attr(&e, b"r:id").unwrap_or_default();
                        let font_key = attr(&e, b"w:fontKey").unwrap_or_default();
                        if !current.is_empty() && !rid.is_empty() {
                            out.push(EmbeddedFace {
                                family: current.clone(),
                                bold: matches!(name.as_ref(), b"w:embedBold" | b"w:embedBoldItalic"),
                                italic: matches!(name.as_ref(), b"w:embedItalic" | b"w:embedBoldItalic"),
                                rid,
                                font_key,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// Parse `word/_rels/fontTable.xml.rels` into `r:id → target` (the `.odttf` part path).
fn parse_font_rels(zip: &mut zip::ZipArchive<std::fs::File>) -> std::collections::HashMap<String, String> {
    use quick_xml::events::Event;
    let mut map = std::collections::HashMap::new();
    let mut s = String::new();
    if zip.by_name("word/_rels/fontTable.xml.rels").map(|mut zf| zf.read_to_string(&mut s)).is_err() {
        return map;
    }
    let mut reader = quick_xml::Reader::from_str(&s);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                if e.name().as_ref() == b"Relationship" {
                    if let (Some(id), Some(target)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                        map.insert(id, target);
                    }
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    map
}

/// Parse `word/document.xml` runs into paragraphs, resolving each run's font family (`w:rFonts w:ascii`)
/// and bold/italic (`w:b` / `w:i`). Headers/footers are not included (the visible body suffices for the
/// render-OCR comparison; the extracted text already covers them separately).
fn parse_document_runs(zip: &mut zip::ZipArchive<std::fs::File>, _names: &[String]) -> Vec<Vec<Run>> {
    use quick_xml::events::Event;
    let mut paragraphs: Vec<Vec<Run>> = Vec::new();
    let mut s = String::new();
    if zip.by_name("word/document.xml").map(|mut zf| zf.read_to_string(&mut s)).is_err() {
        return paragraphs;
    }
    let mut reader = quick_xml::Reader::from_str(&s);

    let mut para: Vec<Run> = Vec::new();
    let mut family = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut color: Option<String> = None;
    let mut size_pt: Option<f32> = None;
    let mut in_rpr = false;
    let mut in_t = false;
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.name().as_ref() {
                b"w:p" => {
                    para = Vec::new();
                }
                b"w:r" => {
                    family.clear();
                    bold = false;
                    italic = false;
                    color = None;
                    size_pt = None;
                    text.clear();
                }
                b"w:rPr" => in_rpr = true,
                b"w:t" => {
                    in_t = true;
                    text.clear();
                }
                _ => {}
            },
            Ok(Event::Empty(e)) => match e.name().as_ref() {
                b"w:rFonts" => {
                    if let Some(f) = attr(&e, b"w:ascii").or_else(|| attr(&e, b"w:hAnsi")) {
                        family = f;
                    }
                }
                b"w:b" if in_rpr => bold = !attr_off(&e),
                b"w:i" if in_rpr => italic = !attr_off(&e),
                b"w:color" if in_rpr => {
                    color = attr(&e, b"w:val").filter(|v| v.len() == 6 && !v.eq_ignore_ascii_case("auto"));
                }
                b"w:sz" if in_rpr => {
                    size_pt = attr(&e, b"w:val").and_then(|v| v.parse::<f32>().ok()).map(|hp| hp / 2.0);
                }
                _ => {}
            },
            Ok(Event::Text(e)) => {
                if in_t {
                    if let Ok(t) = e.unescape() {
                        text.push_str(&t);
                    }
                }
            }
            Ok(Event::End(e)) => match e.name().as_ref() {
                b"w:rPr" => in_rpr = false,
                b"w:t" => in_t = false,
                b"w:r" => {
                    if !text.is_empty() {
                        para.push(Run {
                            text: std::mem::take(&mut text),
                            family: family.clone(),
                            bold,
                            italic,
                            color: color.clone(),
                            size_pt,
                        });
                    }
                }
                b"w:p" => paragraphs.push(std::mem::take(&mut para)),
                _ => {}
            },
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    paragraphs
}

/// A toggle element like `<w:b/>` is ON; `<w:b w:val="false"/>` (or `0`/`off`) is OFF.
fn attr_off(e: &quick_xml::events::BytesStart) -> bool {
    matches!(attr(e, b"w:val").as_deref(), Some("false") | Some("0") | Some("off"))
}

/// Read an XML attribute value by qualified name.
fn attr(e: &quick_xml::events::BytesStart, key: &[u8]) -> Option<String> {
    e.attributes().with_checks(false).flatten().find(|a| a.key.as_ref() == key).and_then(|a| {
        a.unescape_value().ok().map(|v| v.into_owned())
    })
}

/// Pick the first byte order whose de-obfuscation yields a parseable SFNT font.
fn first_parseable(raw: &[u8], font_key: &str) -> Option<Vec<u8>> {
    for reversed in [true, false] {
        let de = deobfuscate(raw, font_key, reversed);
        if ttf_parser::Face::parse(&de, 0).is_ok() {
            return Some(de);
        }
    }
    None
}

/// Minimal, dependency-free standard base64 encoder (for the inline `@font-face` data URLs).
fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(T[(n >> 18 & 63) as usize] as char);
        out.push(T[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// Escape text for an HTML text node.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// Escape a font-family name for use inside a single-quoted CSS string.
fn css_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn escapes_html_and_css() {
        assert_eq!(html_escape("a<b>&c"), "a&lt;b&gt;&amp;c");
        assert_eq!(css_escape("O'Hara\\x"), "O\\'Hara\\\\x");
    }
}
