//! `pdf2md` — PDF to Markdown, plus a window onto each stage of the pipeline:
//! how pages are classified and routed, what the native branch reads, what a
//! page draws besides text, and what the layout model makes of it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use pdf_extractor_2_md::assemble;
use pdf_extractor_2_md::detect::{self, DocumentReport, ScanStrategy};
use pdf_extractor_2_md::integrity::{self, ChkDefaced, IntegrityCheck};
use pdf_extractor_2_md::markdown::{self, Images, Options, Writer, FIGURE_DPI};
#[cfg(any(feature = "tesseract", feature = "ppocr"))]
use pdf_extractor_2_md::policy;
use pdf_extractor_2_md::native;
use pdf_extractor_2_md::region::Region;
use pdf_extractor_2_md::structure::Typography;

const USAGE: &str = "\
usage: pdf2md <input.pdf> [-o out.md] [--images embed|files|skip]
       OCR delle pagine scansionate: [--engine v6|tesseract] [--model small|medium|tiny] [--fallback]
       pdf2md <input.pdf> --report [--sample N]   pages, routing and integrity
       pdf2md <input.pdf> --lines PAGE            lines the native branch reads (0 = all)
       pdf2md <input.pdf> --structure             outline, metadata, rules, images
       pdf2md <input.pdf> --layout PAGE           regions and reading order (feature `layout`)";

/// What the command was asked to do.
enum Task {
    Convert { output: Option<PathBuf>, images: Images },
    Report(ScanStrategy),
    Lines(usize),
    Structure,
    #[cfg(feature = "layout")]
    Layout(usize),
    #[cfg(any(feature = "tesseract", feature = "ppocr"))]
    OcrImage(PathBuf),
    /// A file listing one PNG per line: each is read with a single engine —
    /// initialisation costs seconds and must not be paid per page — and the
    /// Markdown lands beside it as `<name>.tess.md`, with a timing line on
    /// stdout per page.
    #[cfg(any(feature = "tesseract", feature = "ppocr"))]
    OcrBatch(PathBuf),
}

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let Some(input) = args.next() else {
        eprintln!("{USAGE}");
        return ExitCode::FAILURE;
    };
    let path = PathBuf::from(&input);

    let mut task = Task::Convert { output: None, images: Images::Embed };
    let mut strategy = ScanStrategy::Full;
    // The OCR selection: which engine, which PP-OCRv6 tier, and whether the
    // word-level Tesseract fallback checks the uncertain words. The three are
    // orthogonal — the tier decides how well a page is read, the fallback
    // decides how its doubtful words are verified. All unused, and so left
    // out, in a build without an OCR branch — where they are still declared,
    // so the dispatch below reads the same in every configuration.
    #[allow(unused_mut, unused_assignments)]
    let (mut engine, mut requested_engine, mut model, mut fallback) =
        (String::from("tesseract"), None::<String>, None::<String>, false);
    let _ = (&engine, &model);
    while let Some(flag) = args.next() {
        match flag.as_str() {
            "--structure" => task = Task::Structure,
            "--report" => task = Task::Report(strategy),
            // Orthogonal to the engine: whichever PP-OCRv6 tier reads the
            // page, the fallback is what checks its uncertain words.
            #[cfg(all(feature = "ppocr", feature = "tesseract"))]
            "--fallback" => fallback = true,
            _ => {
                let Some(argument) = args.next() else {
                    eprintln!("{flag} needs a value\n{USAGE}");
                    return ExitCode::FAILURE;
                };
                match (flag.as_str(), argument) {
                    ("-o", out) => {
                        if let Task::Convert { output, .. } = &mut task {
                            *output = Some(PathBuf::from(out));
                        }
                    }
                    ("--images", mode) => match images_mode(&mode, &path) {
                        Some(images) => {
                            if let Task::Convert { images: slot, .. } = &mut task {
                                *slot = images;
                            }
                        }
                        None => {
                            eprintln!("--images wants embed, files or skip\n{USAGE}");
                            return ExitCode::FAILURE;
                        }
                    },
                    ("--sample", n) => match n.parse() {
                        Ok(n) => {
                            strategy = ScanStrategy::Sample(n);
                            task = Task::Report(strategy);
                        }
                        Err(_) => {
                            eprintln!("--sample wants a page count\n{USAGE}");
                            return ExitCode::FAILURE;
                        }
                    },
                    ("--lines", n) => match n.parse() {
                        Ok(n) => task = Task::Lines(n),
                        Err(_) => return usage_error(),
                    },
                    #[cfg(feature = "layout")]
                    ("--layout", n) => match n.parse() {
                        Ok(n) => task = Task::Layout(n),
                        Err(_) => return usage_error(),
                    },
                    #[cfg(any(feature = "tesseract", feature = "ppocr"))]
                    ("--ocr-png", image) => task = Task::OcrImage(PathBuf::from(image)),
                    #[cfg(any(feature = "tesseract", feature = "ppocr"))]
                    ("--ocr-batch", list) => task = Task::OcrBatch(PathBuf::from(list)),
                    #[cfg(any(feature = "tesseract", feature = "ppocr"))]
                    ("--engine", chosen) => {
                        requested_engine = Some(chosen.clone());
                        engine = chosen;
                    }
                    #[cfg(feature = "ppocr")]
                    ("--model", tier) => model = Some(tier),
                    _ => return usage_error(),
                }
            }
        }
    }

    // `--engine v6 --model small` and the older `--engine v6-small` name the
    // same thing; the resolved name is what the rest of the program sees.
    #[cfg(any(feature = "tesseract", feature = "ppocr"))]
    let engine = resolve_engine(&engine, model.as_deref());
    #[cfg(any(feature = "tesseract", feature = "ppocr"))]
    let requested_engine = requested_engine.map(|_| engine.clone());

    match task {
        Task::Convert { output, images } => {
            convert(&path, output, images, requested_engine.as_deref(), fallback)
        }
        Task::Report(strategy) => report(&path, strategy),
        Task::Lines(page) => print_lines(&path, page),
        Task::Structure => print_structure(&path),
        #[cfg(feature = "layout")]
        Task::Layout(page) => print_layout(&path, page),
        #[cfg(any(feature = "tesseract", feature = "ppocr"))]
        Task::OcrImage(image) => ocr_image(&image, &engine, fallback),
        #[cfg(any(feature = "tesseract", feature = "ppocr"))]
        Task::OcrBatch(list) => ocr_batch(&list, &engine, fallback),
    }
}

/// The engine name the rest of the program uses, from `--engine` and
/// `--model`.
///
/// `--engine v6 --model small` is the way to say it now; `v6-small` remains a
/// valid name because measurements and reports already carry it, and a name
/// that used to work must keep working.
#[cfg(any(feature = "tesseract", feature = "ppocr"))]
fn resolve_engine(engine: &str, model: Option<&str>) -> String {
    let tier = model.unwrap_or("small");
    match engine {
        "v6" | "ppocr" | "ppocrv6" => format!("v6-{tier}"),
        // A tier named on `--engine` wins over `--model`: it is the more
        // specific statement of the two.
        other => other.to_string(),
    }
}

/// Resolution scanned pages are rasterised at for OCR. 200 DPI: measured in
/// `old_project/edito-ocr-v6` (`pipeline.py:497-522`), a 599 dpi scan read at
/// 200 gives the same 337 words as at 599 and costs 70 % less; below 150 a
/// line starts to go missing.
#[cfg(any(feature = "tesseract", feature = "ppocr"))]
const OCR_DPI: f32 = 200.0;

/// The OCR engine the `--engine` flag names: `tesseract`, or `v6-medium`,
/// `v6-small`, `v6-tiny` for the PP-OCRv6 tiers. Each is behind its feature;
/// asking for one that is not compiled in is an error, not a silent fallback.
#[cfg(any(feature = "tesseract", feature = "ppocr"))]
enum OcrEngine {
    #[cfg(feature = "tesseract")]
    Tesseract(pdf_extractor_2_md::ocr::TesseractEngine),
    #[cfg(feature = "ppocr")]
    Paddle(pdf_extractor_2_md::ocr::PaddleEngine),
    /// PP-OCRv6 small with the word-level Tesseract fallback (the arbiter).
    #[cfg(all(feature = "ppocr", feature = "tesseract"))]
    Arbitrated(pdf_extractor_2_md::ocr::ArbitratedPaddle, pdf_extractor_2_md::arbiter::Outcome),
}

#[cfg(any(feature = "tesseract", feature = "ppocr"))]
impl OcrEngine {
    /// Open the named engine. `fallback` wraps a PP-OCRv6 tier in the
    /// word-level Tesseract arbitration; it means nothing for Tesseract
    /// itself, which is already the arbiter.
    fn open(name: &str, fallback: bool) -> Result<Self, String> {
        // The historical `v6-small+tess` spelling still names the arbitrated
        // engine, so older commands and report suffixes keep working.
        let (name, fallback) = match name.strip_suffix("+tess") {
            Some(base) => (base, true),
            None => (name, fallback),
        };
        match name {
            #[cfg(feature = "tesseract")]
            "tesseract" => pdf_extractor_2_md::ocr::TesseractEngine::new(
                "ita+eng",
                PathBuf::from("models/tesseract/tessdata"),
            )
            .map(OcrEngine::Tesseract)
            .map_err(|error| format!("{error:?}")),
            #[cfg(feature = "ppocr")]
            "v6-medium" | "v6-small" | "v6-tiny" => {
                use pdf_extractor_2_md::ocr::{PaddleEngine, PaddleTier};
                if std::env::var_os("ORT_DYLIB_PATH").is_none() {
                    std::env::set_var("ORT_DYLIB_PATH", native::onnxruntime_path());
                }
                let tier = match name {
                    "v6-small" => PaddleTier::Small,
                    "v6-tiny" => PaddleTier::Tiny,
                    _ => PaddleTier::Medium,
                };
                let paddle = PaddleEngine::new(Path::new("models/paddleocr"), tier)
                    .map_err(|error| format!("{error:?}"))?;
                if !fallback {
                    return Ok(OcrEngine::Paddle(paddle));
                }
                // The arbitration wraps whichever tier was chosen: the tier
                // decides how well the page is read, the fallback decides how
                // the uncertain words are checked.
                #[cfg(feature = "tesseract")]
                {
                    use pdf_extractor_2_md::ocr::ArbitratedPaddle;
                    Ok(OcrEngine::Arbitrated(
                        ArbitratedPaddle::new(
                            paddle,
                            Path::new("models/wordlists"),
                            PathBuf::from("models/tesseract/tessdata"),
                        ),
                        Default::default(),
                    ))
                }
                #[cfg(not(feature = "tesseract"))]
                Err("--fallback richiede la feature `tesseract`".to_string())
            }
            other => Err(format!(
                "motore sconosciuto {other:?} (questa build conosce: tesseract, v6-small, v6-medium, v6-tiny; \
                 con --fallback la validazione parola per parola via Tesseract)"
            )),
        }
    }

    /// The first engine of the policy that actually opens, with the reason
    /// each refusal gave — a document read by the parachute must say so.
    fn open_by_policy(
        requested: Option<&str>,
        fallback: bool,
    ) -> Result<(Self, String), Vec<String>> {
        let mut refusals = Vec::new();
        for name in policy::candidates(requested) {
            match Self::open(&name, fallback) {
                Ok(engine) => return Ok((engine, name)),
                Err(error) => refusals.push(format!("{name}: {error}")),
            }
        }
        Err(refusals)
    }

    fn read_page(
        &mut self,
        page: &image::RgbImage,
    ) -> Result<Vec<pdf_extractor_2_md::native::Line>, String> {
        match self {
            #[cfg(feature = "tesseract")]
            OcrEngine::Tesseract(engine) => {
                engine.read_page(page).map_err(|error| format!("{error:?}"))
            }
            #[cfg(feature = "ppocr")]
            OcrEngine::Paddle(engine) => {
                engine.read_page(page).map_err(|error| format!("{error:?}"))
            }
            #[cfg(all(feature = "ppocr", feature = "tesseract"))]
            OcrEngine::Arbitrated(engine, totals) => {
                let (lines, outcome) = engine.read_page(page).map_err(|error| format!("{error:?}"))?;
                totals.corrections.extend(outcome.corrections);
                totals.flagged_numbers += outcome.flagged_numbers;
                totals.declined += outcome.declined;
                totals.declined_samples.extend(outcome.declined_samples);
                totals.declined_samples.truncate(40);
                totals.language = outcome.language;
                Ok(lines)
            }
        }
    }

    /// What arbitration did across the run, for the batch report.
    fn arbitration(&self) -> Option<&pdf_extractor_2_md::arbiter::Outcome> {
        match self {
            #[cfg(all(feature = "ppocr", feature = "tesseract"))]
            OcrEngine::Arbitrated(_, totals) => Some(totals),
            #[allow(unreachable_patterns)]
            _ => None,
        }
    }
}

#[cfg(any(feature = "tesseract", feature = "ppocr"))]
fn ocr_batch(list: &Path, engine_name: &str, fallback: bool) -> ExitCode {
    use pdf_extractor_2_md::structure::Typography;
    use std::time::Instant;

    let Ok(listing) = std::fs::read_to_string(list) else {
        eprintln!("cannot read {}", list.display());
        return ExitCode::FAILURE;
    };
    let mut engine = match OcrEngine::open(engine_name, fallback) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("engine unavailable: {error}");
            return ExitCode::FAILURE;
        }
    };
    for entry in listing.lines().map(str::trim).filter(|entry| !entry.is_empty()) {
        let started = Instant::now();
        let Ok(page) = image::open(entry).map(|image| image.into_rgb8()) else {
            println!("{entry}	ERROR open");
            continue;
        };
        let Ok(lines) = engine.read_page(&page) else {
            println!("{entry}	ERROR ocr");
            continue;
        };
        let typography = Typography::of(std::slice::from_ref(&lines), &[]);
        let mut writer =
            Writer::new(Options { images: Images::Skip, keep_furniture: true }, typography);
        let blocks = assemble::assemble(lines, &[]);
        let markdown = writer.page(&blocks, None);
        let suffix = if fallback { format!("{engine_name}+tess") } else { engine_name.to_string() };
        let out = format!("{entry}.{suffix}.md");
        if std::fs::write(&out, markdown).is_err() {
            println!("{entry}	ERROR write");
            continue;
        }
        println!("{entry}	{:.2}", started.elapsed().as_secs_f64());
    }
    if let Some(totals) = engine.arbitration() {
        eprintln!(
            "arbitration: {} correction(s), {} number(s) flagged, {} declined, language {:?}",
            totals.corrections.len(),
            totals.flagged_numbers,
            totals.declined,
            totals.language,
        );
        for (from, to) in &totals.corrections {
            eprintln!("  {from} -> {to}");
        }
        for (paddle, tesseract) in &totals.declined_samples {
            eprintln!("  declined: {paddle} | tess: {tesseract}");
        }
    }
    ExitCode::SUCCESS
}

/// The OCR pipeline on one raster page: the chosen engine reads it, then the
/// same column detection, assembly and Markdown writer as the native branch.
/// `<input>` is ignored in this mode; the image is the input.
#[cfg(any(feature = "tesseract", feature = "ppocr"))]
fn ocr_image(image_path: &Path, engine_name: &str, fallback: bool) -> ExitCode {
    use pdf_extractor_2_md::structure::Typography;

    let mut engine = match OcrEngine::open(engine_name, fallback) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("engine unavailable: {error}");
            return ExitCode::FAILURE;
        }
    };
    let page = match image::open(image_path) {
        Ok(image) => image.into_rgb8(),
        Err(error) => {
            eprintln!("cannot open {}: {error}", image_path.display());
            return ExitCode::FAILURE;
        }
    };
    let lines = match engine.read_page(&page) {
        Ok(lines) => lines,
        Err(error) => {
            eprintln!("ocr failed on {}: {error}", image_path.display());
            return ExitCode::FAILURE;
        }
    };
    let typography = Typography::of(std::slice::from_ref(&lines), &[]);
    let mut writer = Writer::new(
        Options { images: Images::Skip, keep_furniture: true },
        typography,
    );
    let blocks = assemble::assemble(lines, &[]);
    print!("{}", writer.page(&blocks, None));
    ExitCode::SUCCESS
}

fn usage_error() -> ExitCode {
    eprintln!("{USAGE}");
    ExitCode::FAILURE
}

fn images_mode(mode: &str, input: &Path) -> Option<Images> {
    match mode {
        "embed" => Some(Images::Embed),
        "skip" => Some(Images::Skip),
        "files" => Some(Images::Files {
            dir: input.parent().unwrap_or(Path::new(".")).to_path_buf(),
            prefix: slug(&input.file_stem().unwrap_or_default().to_string_lossy()),
        }),
        _ => None,
    }
}

/// Why a page cannot be read natively, if it cannot.
fn ocr_reason(
    routing: &DocumentReport,
    number: u32,
) -> Option<pdf_extractor_2_md::detect::OcrReason> {
    routing.pages.iter().find(|page| page.number == number).and_then(|page| page.ocr)
}

/// Figure regions for the images no region covers.
///
/// The layout model places what it recognises; an image it passes over — a
/// letterhead, a stamp, a logo in a margin — would otherwise leave the
/// Markdown without a word being said about it.
fn images_outside(page: &pdfium_render::prelude::PdfPage, regions: &[Region]) -> Vec<Region> {
    use pdf_extractor_2_md::region::RegionKind;

    /// Side below which a placed image is a spacer, a rule or a bullet dot
    /// rather than a figure worth carrying into the Markdown.
    const MIN_SIDE: f32 = 16.0;

    native::objects::images(page)
        .into_iter()
        .filter(|placement| {
            placement.bbox.width() >= MIN_SIDE && placement.bbox.height() >= MIN_SIDE
        })
        .filter(|placement| {
            regions.iter().all(|region| placement.bbox.share_inside(&region.bbox) < 0.5)
        })
        .map(|placement| Region::plain(placement.bbox, RegionKind::Figure))
        .collect()
}

/// A file name safe to write into a Markdown link: a space in a destination
/// ends it, so `italia grafica-001.png` would link to `italia`.
fn slug(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-').to_string();
    let mut out = String::with_capacity(trimmed.len());
    let mut previous_dash = false;
    for c in trimmed.chars() {
        if c == '-' && previous_dash {
            continue;
        }
        previous_dash = c == '-';
        out.push(c);
    }
    if out.is_empty() { "figure".to_string() } else { out }
}

/// The conversion itself: route the pages, read the ones the native branch
/// owns, and write what came out.
fn convert(
    path: &Path,
    output: Option<PathBuf>,
    images: Images,
    engine: Option<&str>,
    fallback: bool,
) -> ExitCode {
    let Ok(document) = lopdf::Document::load(path) else {
        eprintln!("cannot open {}", path.display());
        return ExitCode::FAILURE;
    };
    let mut routing = detect::scan_document(&document, ScanStrategy::Full);
    if let Ok(found) = ChkDefaced::default().inspect(&document, &path.display().to_string()) {
        integrity::route_defaced_pages(&found, &mut routing);
    }

    let Ok(pdfium) = native::bind_pdfium() else {
        eprintln!("pdfium unavailable from {}", native::native_dir().display());
        return ExitCode::FAILURE;
    };
    let Ok(rendered) = pdfium.load_pdf_from_file(path, None) else {
        eprintln!("pdfium cannot open {}", path.display());
        return ExitCode::FAILURE;
    };

    // The Files mode writes beside the Markdown, not beside the PDF.
    let images = match (images, &output) {
        (Images::Files { prefix, .. }, Some(out)) => Images::Files {
            dir: out.parent().unwrap_or(Path::new(".")).to_path_buf(),
            prefix,
        },
        (images, _) => images,
    };
    let mut layout = open_layout_model();

    // Every page's text is read first, because the type size that marks a
    // heading is a property of the whole document and cannot be known from one
    // page. Text is cheap — about 17 ms a page — so this pass reads text only
    // and leaves rendering, which costs thirty times as much, to the second.
    // A page bound for OCR contributes an empty entry, keeping the two passes
    // in step by page number.
    let lines_by_page: Vec<Vec<pdf_extractor_2_md::native::Line>> = rendered
        .pages()
        .iter()
        .enumerate()
        .map(|(index, page)| match ocr_reason(&routing, index as u32 + 1) {
            Some(_) => Vec::new(),
            None => native::text::page_lines(&page).unwrap_or_default(),
        })
        .collect();
    let typography = Typography::of(&lines_by_page, &native::outline::bookmarks(&rendered));

    let wants_figures = !matches!(images, Images::Skip);
    let mut writer = Writer::new(Options { images, keep_furniture: false }, typography);
    let mut pages = Vec::new();
    // The OCR engine is opened only if a page actually needs it: loading a
    // recogniser costs seconds, and most documents never route a page.
    let mut ocr = OcrBranch::new(engine, fallback, routing.pages_needing_ocr().count());

    for (index, page) in rendered.pages().iter().enumerate() {
        let number = index as u32 + 1;
        // A page the router sent to OCR is read from its pixels instead.
        if let Some(reason) = ocr_reason(&routing, number) {
            pages.push(ocr_page(&page, number, reason, &mut ocr, &mut writer));
            continue;
        }
        let lines = lines_by_page[index].clone();
        if lines.is_empty() {
            continue;
        }
        // Rendering is paid for only when something will read the raster: the
        // layout model, or a figure that has to be cropped out of it.
        let raster = (layout_is_open(&layout) || wants_figures)
            .then(|| native::render_page(&page, FIGURE_DPI).ok())
            .flatten();
        let mut regions = regions_of(&mut layout, raster.as_ref());
        regions.extend(images_outside(&page, &regions));
        let blocks = assemble::assemble(lines, &regions);
        pages.push(writer.page(&blocks, raster.as_ref()));
    }

    let out = markdown::document(&pages);
    match output {
        Some(file) => match std::fs::write(&file, &out) {
            Ok(()) => println!("{}: {} byte in {}", path.display(), out.len(), file.display()),
            Err(error) => {
                eprintln!("cannot write {}: {error}", file.display());
                return ExitCode::FAILURE;
            }
        },
        None => print!("{out}"),
    }
    ExitCode::SUCCESS
}

// Without the layout model there is no model to open, and regions come from
// geometry alone: every line is an orphan, so clustering and the XY cut do
// the work.
#[cfg(not(feature = "layout"))]
fn open_layout_model() {}

#[cfg(not(feature = "layout"))]
fn layout_is_open(_: &()) -> bool {
    false
}

#[cfg(feature = "layout")]
fn layout_is_open(model: &Option<pdf_extractor_2_md::layout::LayoutModel>) -> bool {
    model.is_some()
}

#[cfg(not(feature = "layout"))]
fn regions_of(_: &mut (), _: Option<&native::Raster>) -> Vec<Region> {
    Vec::new()
}

#[cfg(feature = "layout")]
fn open_layout_model() -> Option<pdf_extractor_2_md::layout::LayoutModel> {
    use pdf_extractor_2_md::layout::LayoutModel;

    if std::env::var_os("ORT_DYLIB_PATH").is_none() {
        std::env::set_var("ORT_DYLIB_PATH", native::onnxruntime_path());
    }
    let model = std::env::var("PDF2MD_LAYOUT_MODEL")
        .unwrap_or_else(|_| "models/paddleocr/layout/PP-DocLayoutV3.onnx".to_string());
    match LayoutModel::open(&model) {
        Ok(model) => Some(model),
        Err(error) => {
            // Losing the model costs structure, not the document: geometry
            // still orders the page.
            eprintln!("layout model unavailable ({error}); falling back to geometry");
            None
        }
    }
}

#[cfg(feature = "layout")]
fn regions_of(
    model: &mut Option<pdf_extractor_2_md::layout::LayoutModel>,
    raster: Option<&native::Raster>,
) -> Vec<Region> {
    use pdf_extractor_2_md::layout;

    // The page is rendered once and both the model and the figure crops read
    // that same raster: rendering is the most expensive thing on the page, and
    // the layout pass and the figures want the same resolution anyway.
    let (Some(model), Some(raster)) = (model.as_mut(), raster) else { return Vec::new() };
    match model.regions(&raster.image) {
        Ok(regions) => regions
            .into_iter()
            .map(|region| Region {
                bbox: layout::to_points(region.bbox, raster.dpi / 72.0, raster.page_height),
                ..region
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// The regions the layout model finds on a page, in the page's own points.
#[cfg(feature = "layout")]
fn print_layout(path: &Path, number: usize) -> ExitCode {
    use pdf_extractor_2_md::layout::LAYOUT_DPI;

    let mut model = open_layout_model();
    if model.is_none() {
        return ExitCode::FAILURE;
    }
    let Ok(pdfium) = native::bind_pdfium() else {
        eprintln!("pdfium unavailable from {}", native::native_dir().display());
        return ExitCode::FAILURE;
    };
    let Ok(document) = pdfium.load_pdf_from_file(path, None) else {
        eprintln!("cannot open {}", path.display());
        return ExitCode::FAILURE;
    };
    let Some(page) = document.pages().iter().nth(number.saturating_sub(1)) else {
        eprintln!("page {number} is out of range");
        return ExitCode::FAILURE;
    };

    let raster = native::render_page(&page, LAYOUT_DPI).ok();
    let regions = regions_of(&mut model, raster.as_ref());
    let lines = native::text::page_lines(&page).unwrap_or_default();
    println!("page {number}: {} regions, {} lines at {LAYOUT_DPI} DPI", regions.len(), lines.len());
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
    let Ok(document) = pdfium.load_pdf_from_file(path, None) else {
        eprintln!("cannot open {}", path.display());
        return ExitCode::FAILURE;
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

/// Dump the lines pdfium yields, with their boxes — how the native branch
/// actually reads a multi-column page or a table.
fn print_lines(path: &Path, number: usize) -> ExitCode {
    let Ok(pdfium) = native::bind_pdfium() else {
        eprintln!("pdfium unavailable from {}", native::native_dir().display());
        return ExitCode::FAILURE;
    };
    let Ok(document) = pdfium.load_pdf_from_file(path, None) else {
        eprintln!("cannot open {}", path.display());
        return ExitCode::FAILURE;
    };
    let pages: Vec<_> = document.pages().iter().collect();
    // Page 0 means the whole document, which is what a sweep over a corpus
    // wants; any other number is that one page.
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

/// Pages, routing and integrity.
fn report(path: &Path, strategy: ScanStrategy) -> ExitCode {
    let Ok(document) = lopdf::Document::load(path) else {
        eprintln!("cannot open {}", path.display());
        return ExitCode::FAILURE;
    };
    let mut found = detect::scan_document(&document, strategy);
    println!(
        "{}: {} — {} of {} pages scanned",
        path.display(),
        found.kind().as_str(),
        found.pages.len(),
        found.total_pages,
    );
    match ChkDefaced::default().inspect(&document, &path.display().to_string()) {
        Ok(integrity_report) => {
            let routed = integrity::route_defaced_pages(&integrity_report, &mut found);
            print_integrity(&integrity_report, routed);
        }
        // An integrity check that cannot run must not stop the extraction: it
        // is a warning, not the deliverable.
        Err(error) => eprintln!("integrity check unavailable: {error}"),
    }
    print_pages(&found);
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
    println!("  {} page(s) need OCR", report.pages_needing_ocr().count());
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


/// The OCR side of a conversion: opened on first use, so a document with no
/// scanned page never pays for a recogniser.
#[cfg(any(feature = "tesseract", feature = "ppocr"))]
struct OcrBranch {
    requested: Option<String>,
    fallback: bool,
    needed: usize,
    engine: Option<(OcrEngine, String)>,
    failed: bool,
}

#[cfg(any(feature = "tesseract", feature = "ppocr"))]
impl OcrBranch {
    fn new(requested: Option<&str>, fallback: bool, needed: usize) -> Self {
        OcrBranch {
            requested: requested.map(str::to_string),
            fallback,
            needed,
            engine: None,
            failed: false,
        }
    }

    /// The engine, opening it the first time it is asked for.
    fn engine(&mut self) -> Option<(&mut OcrEngine, &str)> {
        if self.engine.is_none() && !self.failed && self.needed > 0 {
            match OcrEngine::open_by_policy(self.requested.as_deref(), self.fallback) {
                Ok((engine, name)) => {
                    let how = if self.fallback { " + fallback Tesseract" } else { "" };
                    eprintln!("ramo OCR: {name}{how} ({} pagina/e)", self.needed);
                    self.engine = Some((engine, name));
                }
                Err(refusals) => {
                    self.failed = true;
                    eprintln!("ramo OCR non disponibile: {}", refusals.join("; "));
                }
            }
        }
        self.engine.as_mut().map(|(engine, name)| (engine, name.as_str()))
    }
}

/// Read one routed page from its pixels and write its Markdown.
///
/// Nothing is ever silently dropped: a page the OCR branch cannot read keeps
/// the warning it used to get, naming the reason.
#[cfg(any(feature = "tesseract", feature = "ppocr"))]
fn ocr_page(
    page: &pdfium_render::prelude::PdfPage,
    number: u32,
    reason: pdf_extractor_2_md::detect::OcrReason,
    branch: &mut OcrBranch,
    writer: &mut Writer,
) -> String {
    let unread = |detail: &str| {
        format!("> ⚠ pagina {number}: da leggere con OCR ({}) — {detail}\n\n", reason.as_str())
    };
    let Ok(raster) = native::render_page(page, OCR_DPI) else {
        return unread("rasterizzazione fallita");
    };
    let Some((engine, name)) = branch.engine() else {
        return unread("nessun motore disponibile");
    };
    let lines = match engine.read_page(&raster.image) {
        Ok(lines) if !lines.is_empty() => lines,
        Ok(_) => return unread("nessun testo riconosciuto"),
        Err(error) => return unread(&format!("lettura fallita: {error}")),
    };
    // The engine works in raster pixels; the rest of the pipeline in points.
    let scale = OCR_DPI / 72.0;
    let lines = lines
        .into_iter()
        .map(|line| scale_line(line, scale))
        .collect::<Vec<_>>();
    let blocks = assemble::assemble(lines, &[]);
    let name = name.to_string();
    format!("<!-- pagina {number}: OCR {name} -->\n\n{}", writer.page(&blocks, None))
}

/// Bring a line from raster pixels into the page's points.
#[cfg(any(feature = "tesseract", feature = "ppocr"))]
fn scale_line(
    line: pdf_extractor_2_md::native::Line,
    scale: f32,
) -> pdf_extractor_2_md::native::Line {
    use pdf_extractor_2_md::geometry::Rect;
    let shrink = |bbox: Rect| {
        Rect::new(bbox.left / scale, bbox.bottom / scale, bbox.right / scale, bbox.top / scale)
    };
    pdf_extractor_2_md::native::Line {
        bbox: shrink(line.bbox),
        words: line
            .words
            .into_iter()
            .map(|word| pdf_extractor_2_md::native::Word {
                bbox: shrink(word.bbox),
                style: pdf_extractor_2_md::native::Style {
                    size: word.style.size / scale,
                    ..word.style
                },
                ..word
            })
            .collect(),
    }
}

/// Without an OCR feature the branch does not exist and a routed page keeps
/// its warning, which is what the document should say.
#[cfg(not(any(feature = "tesseract", feature = "ppocr")))]
struct OcrBranch;

#[cfg(not(any(feature = "tesseract", feature = "ppocr")))]
impl OcrBranch {
    fn new(_requested: Option<&str>, _fallback: bool, _needed: usize) -> Self {
        OcrBranch
    }
}

#[cfg(not(any(feature = "tesseract", feature = "ppocr")))]
fn ocr_page(
    _page: &pdfium_render::prelude::PdfPage,
    number: u32,
    reason: pdf_extractor_2_md::detect::OcrReason,
    _branch: &mut OcrBranch,
    _writer: &mut Writer,
) -> String {
    format!(
        "> ⚠ pagina {number}: da leggere con OCR ({}) — build senza ramo OCR\n\n",
        reason.as_str()
    )
}
