//! `pdf2md` — the pipeline's command line, so far a window onto each stage:
//! how pages are classified and routed, what the native branch reads, what a
//! page draws besides text, and what the layout model makes of it.
//!
//! Markdown itself comes with the rest of Phase 3.

use std::path::Path;
use std::process::ExitCode;

use pdf_extractor_2_md::detect::{self, DocumentReport, ScanStrategy};
use pdf_extractor_2_md::integrity::{self, ChkDefaced, IntegrityCheck};
use pdf_extractor_2_md::native;

const USAGE: &str = "usage: pdf2md <input.pdf> [--sample N] [--lines PAGE] [--structure]\n\
                     (--lines 0 = every page; --structure = outline, metadata, rules, images)";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let mut strategy = ScanStrategy::Full;
    let mut lines_of_page = None;
    let mut structure = false;
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--structure" => structure = true,
            #[cfg(feature = "layout")]
            "--layout" => {
                let Some(value) = args.next().and_then(|v| v.parse::<usize>().ok()) else {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                };
                return print_layout(Path::new(&input), value);
            }
            "--sample" | "--lines" => {
                let Some(value) = args.next().and_then(|v| v.parse::<usize>().ok()) else {
                    eprintln!("{USAGE}");
                    return ExitCode::FAILURE;
                };
                match flag.as_str() {
                    "--sample" => strategy = ScanStrategy::Sample(value),
                    _ => lines_of_page = Some(value),
                }
            }
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }

    let path = Path::new(&input);
    if structure {
        return print_structure(path);
    }
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

/// The regions the layout model finds on a page, in the page's own points.
#[cfg(feature = "layout")]
fn print_layout(path: &Path, number: usize) -> ExitCode {
    use pdf_extractor_2_md::layout::{self, LayoutModel, LAYOUT_DPI};
    use pdf_extractor_2_md::{assemble, region::Region};

    // ort opens ONNX Runtime by this variable. Defaulting it to the runtime
    // that matches this build saves the caller from pointing it at the wrong
    // architecture, which fails with an error that explains nothing.
    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        std::env::set_var("ORT_DYLIB_PATH", native::onnxruntime_path());
    }
    let model_path = std::env::var("PDF2MD_LAYOUT_MODEL")
        .unwrap_or_else(|_| "models/paddleocr/layout/PP-DocLayoutV3.onnx".to_string());
    let mut model = match LayoutModel::open(&model_path) {
        Ok(model) => model,
        Err(error) => {
            eprintln!("cannot load {model_path}: {error}");
            eprintln!("ORT_DYLIB_PATH must point at native/<arch>/onnxruntime.dll");
            return ExitCode::FAILURE;
        }
    };
    let Ok(pdfium) = native::bind_pdfium() else {
        eprintln!("pdfium unavailable from {}", native::native_dir().display());
        return ExitCode::FAILURE;
    };
    let document = match pdfium.load_pdf_from_file(path, None) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("cannot open {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };
    let Some(page) = document.pages().iter().nth(number.saturating_sub(1)) else {
        eprintln!("page {number} is out of range");
        return ExitCode::FAILURE;
    };

    let raster = match native::render_page(&page, LAYOUT_DPI) {
        Ok(raster) => raster,
        Err(error) => {
            eprintln!("cannot render page {number}: {error}");
            return ExitCode::FAILURE;
        }
    };
    let regions = match model.regions(&raster) {
        Ok(regions) => regions,
        Err(error) => {
            eprintln!("layout failed on page {number}: {error}");
            return ExitCode::FAILURE;
        }
    };
    // The model works in raster pixels; everything downstream works in the
    // page's own points, so the regions are brought back here and once only.
    let height = page.height().value;
    let scale = LAYOUT_DPI / 72.0;
    let regions: Vec<_> = regions
        .into_iter()
        .map(|region| Region { bbox: layout::to_points(region.bbox, scale, height), ..region })
        .collect();

    let lines = native::text::page_lines(&page).unwrap_or_default();
    println!(
        "page {number}: {} regions, {} lines ({}x{} px)",
        regions.len(),
        lines.len(),
        raster.width(),
        raster.height(),
    );
    for block in assemble::assemble(lines, &regions) {
        let text = block.text().replace('\n', " ");
        let shown: String = text.chars().take(96).collect();
        println!(
            "  {:<10}{} [{:>6.1},{:>6.1} {:>6.1}x{:>6.1}] {shown}{}",
            block.kind.as_str(),
            if block.recovered { "*" } else { " " },
            block.bbox.left,
            block.bbox.bottom,
            block.bbox.width(),
            block.bbox.height(),
            if text.chars().count() > 96 { "…" } else { "" },
        );
    }
    ExitCode::SUCCESS
}

/// What the document says about itself, and what each page draws besides text.
fn print_structure(path: &Path) -> ExitCode {
    let Ok(pdfium) = native::bind_pdfium() else {
        eprintln!("pdfium unavailable from {}", native::native_dir().display());
        return ExitCode::FAILURE;
    };
    let document = match pdfium.load_pdf_from_file(path, None) {
        Ok(document) => document,
        Err(error) => {
            eprintln!("cannot open {}: {error}", path.display());
            return ExitCode::FAILURE;
        }
    };

    let metadata = native::outline::metadata(&document);
    println!("metadata: {} field(s)", metadata.len());
    for (name, value) in &metadata {
        println!("  {name}: {value}");
    }

    let bookmarks = native::outline::bookmarks(&document);
    println!("outline: {} bookmark(s)", bookmarks.len());
    for bookmark in bookmarks.iter().take(20) {
        let page = bookmark.page.map_or_else(|| "?".to_string(), |page| page.to_string());
        println!("  {}p.{page:<5} {}", "  ".repeat(bookmark.level), bookmark.title);
    }
    if bookmarks.len() > 20 {
        println!("  … {} more", bookmarks.len() - 20);
    }

    for (index, page) in document.pages().iter().enumerate() {
        let rules = native::objects::rules(&page);
        let images = native::objects::images(&page);
        let annotations = native::objects::annotations(&page);
        if rules.is_empty() && images.is_empty() && annotations.is_empty() {
            continue;
        }
        let horizontal = rules.iter().filter(|rule| rule.horizontal).count();
        println!(
            "  page {:>4}: {:>4} rules ({horizontal} h / {} v), {} image(s), {} annotation(s)",
            index + 1,
            rules.len(),
            rules.len() - horizontal,
            images.len(),
            annotations.len(),
        );
    }
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
