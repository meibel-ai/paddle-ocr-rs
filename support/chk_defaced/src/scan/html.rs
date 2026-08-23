//! HTML+CSS backend (static pass, no browser in v1): enumerates `@font-face` declarations and tries
//! to verify locally-referenced fonts, Unicode hygiene of the text, and CSS cloaking heuristics.
//! NB: the authoritative ground truth (rendered DOM, downloaded webfonts, OCR) needs a headless
//! browser (a later phase); here we only have lower-confidence heuristics.

use std::path::Path;

use anyhow::{Context, Result};
use regex::Regex;
use scraper::{Html, Selector};

use crate::finding::{Category, Finding, Report, Severity};
use crate::registry::FontRegistry;
use crate::{font, scan, unicode};

pub fn scan(path: &Path, registry: Option<&FontRegistry>) -> Result<Report> {
    let src = std::fs::read_to_string(path).with_context(|| format!("reading HTML {}", path.display()))?;
    let mut report = Report::new(&path.display().to_string(), "html");
    let doc = Html::parse_document(&src);

    // 1. Visible/extracted text → Unicode hygiene.
    let text: String = doc.root_element().text().collect::<Vec<_>>().join(" ");
    unicode::scan_text(&text, "DOM text", &mut report.findings);

    // 2. Gather all inline CSS (<style>) + element inline styles.
    let style_sel = Selector::parse("style").unwrap();
    let mut css = String::new();
    for el in doc.select(&style_sel) {
        css.push_str(&el.text().collect::<String>());
        css.push('\n');
    }
    let styled_sel = Selector::parse("[style]").unwrap();
    let mut inline_styles = String::new();
    for el in doc.select(&styled_sel) {
        if let Some(s) = el.value().attr("style") {
            inline_styles.push_str(s);
            inline_styles.push('\n');
        }
    }
    let all_css = format!("{css}\n{inline_styles}");

    // 3. Cloaking heuristics (variant D).
    let hide_patterns = [
        (r"display\s*:\s*none", "display:none"),
        (r"visibility\s*:\s*hidden", "visibility:hidden"),
        (r"opacity\s*:\s*0(?:\.0+)?\b", "opacity:0"),
        (r"font-size\s*:\s*0(?:px|pt|em)?\b", "font-size:0"),
        (r"text-indent\s*:\s*-\s*9{3,}", "negative text-indent"),
        (r"clip\s*:\s*rect\(\s*0", "clip:rect(0…)"),
        (r"left\s*:\s*-\s*9{3,}", "off-canvas (negative left)"),
    ];
    for (pat, label) in hide_patterns {
        let re = Regex::new(pat).unwrap();
        let n = re.find_iter(&all_css).count();
        if n > 0 {
            report.push(Finding::new(
                "HTML.HIDDEN_STYLE",
                Severity::Medium,
                Category::HiddenContent,
                "CSS",
                format!("{n} cloaking rule(s) '{label}': text potentially present in the extract but not visible"),
                0.6,
            ));
        }
    }

    // 4. ::before/::after { content: "…" } → text injected outside the DOM (variant E).
    let content_re = Regex::new(r#"content\s*:\s*(["'])"#).unwrap();
    let n_content = content_re.find_iter(&all_css).count();
    if n_content > 0 {
        report.push(Finding::new(
            "HTML.CSS_CONTENT_INJECTION",
            Severity::Low,
            Category::HiddenContent,
            "CSS",
            format!("{n_content} `content:` declaration(s) (text injected via CSS, absent from the extracted DOM)"),
            0.4,
        ));
    }

    // 5. @font-face → enumerate, and verify LOCAL referenced fonts (relative urls, not http).
    let face_re = Regex::new(r"(?is)@font-face\s*\{([^}]*)\}").unwrap();
    let url_re = Regex::new(r#"url\(\s*['"]?([^'")]+)['"]?\s*\)"#).unwrap();
    let base_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut faces = 0usize;
    let mut examined = 0usize;
    for face in face_re.captures_iter(&all_css) {
        faces += 1;
        let body = &face[1];
        for url in url_re.captures_iter(body) {
            let target = url[1].trim();
            if target.starts_with("http://") || target.starts_with("https://") || target.starts_with("//") {
                report.push(Finding::new(
                    "HTML.REMOTE_WEBFONT",
                    Severity::Info,
                    Category::FontIntegrity,
                    "@font-face",
                    format!("remote webfont not verifiable offline: {target}"),
                    0.3,
                ));
                continue;
            }
            if target.starts_with("data:") {
                report.push(Finding::new(
                    "HTML.DATAURI_WEBFONT",
                    Severity::Info,
                    Category::FontIntegrity,
                    "@font-face",
                    "inline webfont (data: URI) — binary verification is not implemented in v1",
                    0.3,
                ));
                continue;
            }
            // local relative file
            let candidate = base_dir.join(target);
            if candidate.is_file() {
                if let Ok(parsed) = font::parse_file(&candidate) {
                    for p in &parsed {
                        let loc = format!("@font-face {target}");
                        scan::judge_embedded_font(p, &loc, registry, &mut report.findings);
                        examined += 1;
                    }
                }
            }
        }
    }
    if faces > 0 {
        report.push(Finding::new(
            "HTML.FONTFACE_COUNT",
            Severity::Info,
            Category::Structural,
            "CSS",
            format!("{faces} @font-face declaration(s) (static pass; full verification needs the headless browser)"),
            0.2,
        ));
    }

    report.fonts_examined = examined;
    Ok(report)
}
