//! Document **provenance metadata** — who/what produced the file — so a consumer can *evaluate the
//! source*, not just the tampering. Extracted deterministically from the container:
//! - PDF: the `/Info` dictionary (`Title`/`Author`/`Creator`/`Producer`/dates/keywords) + header version;
//! - DOCX: `docProps/core.xml` (Dublin Core: title/creator/lastModifiedBy/dates/revision) and
//!   `docProps/app.xml` (`Application`/`Company`/`AppVersion`).
//!
//! Forensic value: an unusual `Producer`/`Application` (or a mismatch between the authoring tool and the
//! font/defacement findings) is itself a signal. Absent fields are simply omitted from the report.

use std::io::Read;

use lopdf::{Document, Object};
use serde::{Deserialize, Serialize};

/// Provenance metadata common to PDF and DOCX (fields absent in the source are `None`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DocumentMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The declared author (PDF `/Author`, DOCX `dc:creator`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Who last saved it (DOCX `cp:lastModifiedBy`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_modified_by: Option<String>,
    /// The authoring application (PDF `/Creator`, DOCX `Application`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub creator_tool: Option<String>,
    /// The library/tool that actually wrote the file (PDF `/Producer`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub producer: Option<String>,
    /// Organisation (DOCX `Company`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub company: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keywords: Option<String>,
    /// Save/revision count (DOCX `cp:revision`) — a "1" on a supposedly-negotiated contract is suspicious.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Application version (DOCX `AppVersion`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    /// Container format version (PDF header, e.g. `1.7`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format_version: Option<String>,
}

impl DocumentMetadata {
    /// `true` when no field could be extracted (nothing to report).
    pub fn is_empty(&self) -> bool {
        *self == DocumentMetadata::default()
    }
}

/// Decode a PDF text string: UTF-16BE when it carries a BOM, otherwise treated as Latin-1/PDFDocEncoding
/// (adequate for the mostly-ASCII author/producer fields). Trims NULs and surrounding whitespace.
fn decode_pdf_text(bytes: &[u8]) -> String {
    let s = if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| b as char).collect()
    };
    s.trim_matches(|c: char| c == '\0' || c.is_whitespace()).to_string()
}

/// A PDF date `D:YYYYMMDDHHmmSS...` → a readable `YYYY-MM-DD HH:MM:SS` (best-effort; returns the raw
/// remainder if it does not match the expected shape).
fn tidy_pdf_date(raw: &str) -> String {
    let d = raw.strip_prefix("D:").unwrap_or(raw);
    let digits: String = d.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        let g = |a: usize, b: usize| &digits[a..b.min(digits.len())];
        let date = format!("{}-{}-{}", g(0, 4), g(4, 6), g(6, 8));
        if digits.len() >= 14 {
            return format!("{date} {}:{}:{}", g(8, 10), g(10, 12), g(12, 14));
        }
        return date;
    }
    raw.to_string()
}

fn opt(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

/// Extract provenance metadata from a loaded PDF (`/Info` dictionary + header version).
pub fn pdf_metadata(doc: &Document) -> DocumentMetadata {
    let mut m = DocumentMetadata { format_version: opt(doc.version.clone()), ..Default::default() };

    let info = doc
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|o| match o {
            Object::Reference(id) => doc.get_object(*id).ok(),
            other => Some(other),
        })
        .and_then(|o| o.as_dict().ok());
    let Some(info) = info else { return m };

    let get = |key: &[u8]| -> Option<String> {
        match info.get(key) {
            Ok(Object::String(bytes, _)) => opt(decode_pdf_text(bytes)),
            _ => None,
        }
    };
    m.title = get(b"Title");
    m.author = get(b"Author");
    m.creator_tool = get(b"Creator");
    m.producer = get(b"Producer");
    m.keywords = get(b"Keywords");
    m.created = get(b"CreationDate").map(|d| tidy_pdf_date(&d));
    m.modified = get(b"ModDate").map(|d| tidy_pdf_date(&d));
    m
}

/// Text content of the first XML element whose (possibly prefixed) local name is `local`.
fn xml_text(xml: &str, local: &str) -> Option<String> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut capture = false;
    let mut out = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let n = name.as_ref();
                let localname = n.rsplit(|&b| b == b':').next().unwrap_or(n);
                if localname == local.as_bytes() {
                    capture = true;
                    out.clear();
                }
            }
            Ok(Event::Text(e)) if capture => {
                if let Ok(t) = e.unescape() {
                    out.push_str(&t);
                }
            }
            Ok(Event::End(_)) if capture => break,
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    opt(out.trim().to_string())
}

/// Extract provenance metadata from a DOCX (`docProps/core.xml` + `docProps/app.xml`).
pub fn docx_metadata(zip: &mut zip::ZipArchive<std::fs::File>) -> DocumentMetadata {
    let mut m = DocumentMetadata::default();
    let read = |zip: &mut zip::ZipArchive<std::fs::File>, name: &str| -> Option<String> {
        let mut s = String::new();
        zip.by_name(name).ok()?.read_to_string(&mut s).ok()?;
        Some(s)
    };

    if let Some(core) = read(zip, "docProps/core.xml") {
        m.title = xml_text(&core, "title");
        m.author = xml_text(&core, "creator");
        m.last_modified_by = xml_text(&core, "lastModifiedBy");
        m.created = xml_text(&core, "created");
        m.modified = xml_text(&core, "modified");
        m.keywords = xml_text(&core, "keywords");
        m.revision = xml_text(&core, "revision");
    }
    if let Some(app) = read(zip, "docProps/app.xml") {
        m.creator_tool = xml_text(&app, "Application");
        m.company = xml_text(&app, "Company");
        m.app_version = xml_text(&app, "AppVersion");
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_string_decoding_and_dates() {
        assert_eq!(decode_pdf_text(b"Adobe Acrobat\0"), "Adobe Acrobat");
        // UTF-16BE "Hi"
        assert_eq!(decode_pdf_text(&[0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69]), "Hi");
        assert_eq!(tidy_pdf_date("D:20240115103000Z"), "2024-01-15 10:30:00");
        assert_eq!(tidy_pdf_date("D:20240115"), "2024-01-15");
    }

    #[test]
    fn docx_core_and_app_parsing() {
        let core = r#"<?xml version="1.0"?>
        <cp:coreProperties xmlns:cp="x" xmlns:dc="y" xmlns:cp2="z">
          <dc:title>Contract</dc:title>
          <dc:creator>Jane Doe</dc:creator>
          <cp:lastModifiedBy>Mallory</cp:lastModifiedBy>
          <cp:revision>1</cp:revision>
        </cp:coreProperties>"#;
        assert_eq!(xml_text(core, "creator").as_deref(), Some("Jane Doe"));
        assert_eq!(xml_text(core, "lastModifiedBy").as_deref(), Some("Mallory"));
        assert_eq!(xml_text(core, "revision").as_deref(), Some("1"));
        assert_eq!(xml_text(core, "title").as_deref(), Some("Contract"));

        let app = r#"<Properties><Application>Microsoft Office Word</Application><Company>ACME</Company></Properties>"#;
        assert_eq!(xml_text(app, "Application").as_deref(), Some("Microsoft Office Word"));
        assert_eq!(xml_text(app, "Company").as_deref(), Some("ACME"));
    }

    /// End-to-end through `scan_path` on real files: metadata is populated and the explicit assessment
    /// is present. Prints the extracted source info for eyeballing. Skips absent fixtures.
    #[test]
    fn real_docs_carry_metadata_and_assessment() {
        use std::path::PathBuf;
        let base = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..");
        for rel in [
            "corpus/clean/us/education/arxiv_1706.03762_attention.pdf",
            "corpus/clean/misc/docx/calibre_demo.docx",
            "defaced-test/replaced.docx",
        ] {
            let p = base.join(rel);
            if !p.exists() {
                eprintln!("skip: {rel} absent");
                continue;
            }
            let r = crate::scan::scan_path(&p, None).expect("scan");
            eprintln!("\n{rel}\n  assessment = {:?}\n  metadata   = {:?}", r.assessment, r.metadata);
            assert!(r.assessment.is_some(), "{rel}: assessment must be computed");
        }
    }
}
