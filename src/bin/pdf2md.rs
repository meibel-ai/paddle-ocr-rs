//! `pdf2md` — Phase 1 CLI: classify a document page by page and say which
//! pages need OCR, and why.
//!
//! Markdown comes in Phase 3; what this prints is the routing decision the
//! rest of the pipeline will act on.

use std::path::Path;
use std::process::ExitCode;

use pdf_extractor_2_md::detect::{self, DocumentReport, ScanStrategy};
use pdf_extractor_2_md::integrity::{self, ChkDefaced, IntegrityCheck};
use pdf_extractor_2_md::native;

const USAGE: &str = "usage: pdf2md <input.pdf> [--sample N] [--lines PAGE]  (--lines 0 = every page)";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let mut strategy = ScanStrategy::Full;
    let mut lines_of_page = None;
    while let Some(flag) = args.next() {
        let value = args.next().and_then(|v| v.parse::<usize>().ok());
        match (flag.as_str(), value) {
            ("--sample", Some(n)) => strategy = ScanStrategy::Sample(n),
            ("--lines", Some(n)) => lines_of_page = Some(n),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }

    let path = Path::new(&input);
    if let Some(number) = lines_of_page {
        return print_lines(path, number);
    }
    let document = match lopdf::Document::load(path) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("cannot open {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };

    let mut report = detect::scan_document(&document, strategy);
    println!(
        "{}: {} — {} of {} pages scanned",
        path.display(),
        report.kind().as_str(),
        report.pages.len(),
        report.total_pages,
    );

    match ChkDefaced::default().inspect(&document, &path.display().to_string()) {
        Ok(found) => {
            let routed = integrity::route_defaced_pages(&found, &mut report);
            print_integrity(&found, routed);
        }
        // An integrity check that cannot run must not stop the extraction: it
        // is a warning, not the deliverable.
        Err(error) => eprintln!("integrity check unavailable: {error}"),
    }

    print_pages(&report);
    ExitCode::SUCCESS
}

/// Dump the lines pdfium yields for one page, with their boxes — how the
/// native branch actually reads a multi-column page or a table.
fn print_lines(path: &Path, number: usize) -> ExitCode {
    let pdfium = match native::bind_pdfium() {
        Ok(pdfium) => pdfium,
        Err(error) => {
            eprintln!("pdfium unavailable from {}: {error}", native::native_dir().display());
            return ExitCode::FAILURE;
        }
    };
    let document = match pdfium.load_pdf_from_file(path, None) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("cannot open {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    // Page 0 means the whole document, which is what a sweep over a corpus
    // wants; any other number is that one page.
    let pages: Vec<_> = document.pages().iter().collect();
    let selected: Vec<usize> = match number {
        0 => (0..pages.len()).collect(),
        n if n <= pages.len() => vec![n - 1],
        _ => {
            eprintln!("page {number} is out of range ({} pages)", pages.len());
            return ExitCode::FAILURE;
        }
    };
    for index in selected {
        let Ok(lines) = native::text::page_lines(&pages[index]) else {
            eprintln!("cannot read page {}", index + 1);
            continue;
        };
        println!("page {}: {} lines", index + 1, lines.len());
        for line in &lines {
            println!(
                "  [{:>6.1},{:>6.1} {:>6.1}x{:>4.1}] {:>4.1}pt{}{} {}",
                line.bbox.left,
                line.bbox.bottom,
                line.bbox.width(),
                line.bbox.height(),
                line.size(),
                if line.words.first().is_some_and(|w| w.style.bold) { " b" } else { "  " },
                if line.is_invisible() { " inv" } else { "    " },
                line.text(),
            );
        }
    }
    ExitCode::SUCCESS
}

fn print_pages(report: &DocumentReport) {
    for page in &report.pages {
        let signals = &page.signals;
        let ocr = match page.ocr {
            Some(reason) => format!("  OCR: {}", reason.as_str()),
            None if page.has_text_layer() => "  (searchable scan)".to_string(),
            None => String::new(),
        };
        println!(
            "  page {:>4}  {:<8} {:>6} visible + {:>6} invisible bytes, \
             {:>3} images covering {:>3.0}%, {:>5} paths{}",
            page.number,
            page.kind.as_str(),
            signals.visible_bytes,
            signals.invisible_bytes,
            signals.image_count,
            signals.image_coverage * 100.0,
            signals.path_ops,
            ocr,
        );
    }
    let needing_ocr = report.pages_needing_ocr().count();
    println!("  {needing_ocr} page(s) need OCR");
}

fn print_integrity(report: &chk_defaced::finding::Report, routed: usize) {
    let Some(assessment) = report.assessment else { return };
    if assessment.ok && report.findings.is_empty() {
        return;
    }
    println!("integrity: {} font(s) examined", report.fonts_examined);
    for finding in &report.findings {
        println!(
            "  [{:?}] {} — {} ({})",
            finding.severity, finding.rule, finding.message, finding.location
        );
    }
    if assessment.defaced {
        println!("  defaced fonts: {routed} page(s) routed to OCR");
    }
    if assessment.hidden_text {
        println!("  hidden text present — it will be carried into the output marked, not dropped");
    }
}
