//! `pdf2md` — Phase 1 CLI: classify a document page by page and say which
//! pages need OCR, and why.
//!
//! Markdown comes in Phase 3; what this prints is the routing decision the
//! rest of the pipeline will act on.

use std::path::Path;
use std::process::ExitCode;

use pdf_extractor_2_md::detect::{self, DocumentReport, ScanStrategy};
use pdf_extractor_2_md::integrity::{self, ChkDefaced, IntegrityCheck};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("usage: pdf2md <input.pdf> [--sample N]");
        return ExitCode::FAILURE;
    };
    let strategy = match (args.next().as_deref(), args.next()) {
        (Some("--sample"), Some(n)) => match n.parse() {
            Ok(n) => ScanStrategy::Sample(n),
            Err(_) => {
                eprintln!("--sample wants a page count, got {n:?}");
                return ExitCode::FAILURE;
            }
        },
        (None, _) => ScanStrategy::Full,
        (Some(other), _) => {
            eprintln!("unknown option {other:?}");
            return ExitCode::FAILURE;
        }
    };

    let path = Path::new(&input);
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
