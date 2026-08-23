//! `chk_defaced` — verifies the coherence of the fonts used to render documents (PDF/DOCX/HTML) and
//! builds a JSON index of the system fonts' cmaps.
//!
//! Background and motivation: "What you see is not what your AI reads"
//! <https://dariofinardi.it/what-you-see-is-not-what-your-ai-reads-c3fed388d3bc>.
//!
//! This is the v1: a registry builder + deterministic font-coherence checks (no OCR/browser). The
//! authoritative, false-positive-free verdict (comparing the outline drawn for a codepoint against the
//! canonical outline for that codepoint) is the next phase — see the README.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};

use chk_defaced::finding::{Report, Severity};
use chk_defaced::registry::FontRegistry;
use chk_defaced::scan;

#[derive(Parser)]
#[command(name = "chk_defaced", version, about = "Detect 'defaced' documents and index the system fonts' cmaps")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Scan a fonts directory (default: the system fonts) and emit a JSON index of the cmaps.
    BuildRegistry {
        /// Fonts directory to index (default: the system fonts directory).
        #[arg(long)]
        fonts_dir: Option<PathBuf>,
        /// Output JSON file.
        #[arg(short, long, default_value = "fonts-index.json")]
        out: PathBuf,
        /// Omit the full cmaps (identity + hashes only): a much smaller index.
        #[arg(long)]
        slim: bool,
        /// Pretty-printed JSON (larger, human-readable).
        #[arg(long)]
        pretty: bool,
    },
    /// Verify the font coherence of one or more documents (.pdf/.docx/.html).
    Scan {
        /// Files to analyze.
        paths: Vec<PathBuf>,
        /// Reference JSON font index (from `build-registry`) used to spot modified fonts.
        #[arg(long)]
        registry: Option<PathBuf>,
        /// Output format: human | json.
        #[arg(long, default_value = "human")]
        format_out: String,
        /// Escalate to specimen-OCR on documents where the deterministic pass finds no semantic
        /// replacement — catches custom fonts with no honest anchor. Needs the `ocr-specimen` build
        /// and a tessdata directory; degrades gracefully (warns and continues) if OCR can't initialize.
        #[arg(long)]
        ocr: bool,
        /// OCR language(s) for `--ocr` (Tesseract `+`-joined form).
        #[arg(long, default_value = "ita+eng")]
        ocr_lang: String,
    },
}

fn default_fonts_dir() -> PathBuf {
    if let Ok(windir) = std::env::var("WINDIR") {
        return PathBuf::from(windir).join("Fonts");
    }
    // Reasonable fallbacks on other platforms.
    for p in [r"C:\Windows\Fonts", "/usr/share/fonts", "/System/Library/Fonts"] {
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            return pb;
        }
    }
    PathBuf::from(r"C:\Windows\Fonts")
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::BuildRegistry { fonts_dir, out, slim, pretty } => {
            let dir = fonts_dir.unwrap_or_else(default_fonts_dir);
            eprintln!("Indexing fonts in {} …", dir.display());
            let reg = FontRegistry::build_from_dir(&dir, !slim)?;
            reg.save_json(&out, pretty)?;
            eprintln!(
                "Indexed {} font faces ({} unparsable files skipped) → {}",
                reg.count,
                reg.skipped.len(),
                out.display()
            );
            Ok(ExitCode::SUCCESS)
        }
        Cmd::Scan { paths, registry, format_out, ocr, ocr_lang } => {
            if paths.is_empty() {
                anyhow::bail!("no files to analyze");
            }
            let reg = match registry {
                Some(p) => Some(FontRegistry::load_json(&p)?),
                None => None,
            };

            // Optional OCR escalation engine (one per run). Only with the `ocr-specimen` build.
            #[cfg(feature = "ocr-specimen")]
            let ocr_engine = if ocr {
                use chk_defaced::ocr::{OcrHint, TesseractOcr};
                match TesseractOcr::new(None, &ocr_lang, OcrHint::SingleLine) {
                    Ok(e) => Some(e),
                    Err(e) => {
                        eprintln!("[warn] --ocr disabled: could not initialize Tesseract ({e:#})");
                        None
                    }
                }
            } else {
                None
            };
            #[cfg(not(any(
                feature = "ocr-specimen",
                feature = "ocr-atlas",
                all(feature = "render-wry", feature = "ocr-tesseract")
            )))]
            if ocr {
                eprintln!(
                    "[warn] --ocr ignored: rebuild with `--features ocr-specimen` (or ocr-atlas, or render-ocr for DOCX)"
                );
            }
            let _ = &ocr_lang; // used only with the OCR features

            let scan_one = |path: &std::path::Path| -> Result<Report> {
                #[cfg(feature = "ocr-specimen")]
                if let Some(engine) = ocr_engine.as_ref() {
                    return scan::scan_path_with_ocr(path, reg.as_ref(), engine);
                }
                scan::scan_path(path, reg.as_ref())
            };

            let mut reports = Vec::new();
            for path in &paths {
                match scan_one(path) {
                    Ok(r) => reports.push(r),
                    Err(e) => eprintln!("[error] {}: {e:#}", path.display()),
                }
            }

            // OCR ground truth for the phrase comparison (PDF only, `--ocr`, `ocr-atlas` build):
            // renders the pages and fills each phrase's `ocr` field. Needs the pdfium library
            // (PDFIUM_DIR) and tessdata; degrades gracefully with a warning.
            #[cfg(feature = "ocr-atlas")]
            if ocr {
                use chk_defaced::ocr::{OcrHint, TesseractOcr};
                let pdfium_dir = std::env::var("PDFIUM_DIR").unwrap_or_else(|_| ".".to_string());
                match TesseractOcr::new(None, &ocr_lang, OcrHint::Block) {
                    Ok(eng) => {
                        for r in reports.iter_mut().filter(|r| r.format == "pdf") {
                            let p = std::path::PathBuf::from(&r.file);
                            if let Err(e) =
                                chk_defaced::atlas::verify_with_render(r, &p, std::path::Path::new(&pdfium_dir), &eng)
                            {
                                eprintln!("[warn] OCR verify for {}: {e:#}", r.file);
                            }
                        }
                    }
                    Err(e) => eprintln!("[warn] --ocr text disabled: {e:#}"),
                }
            }

            // DOCX render-OCR verdict (`render-ocr` build): render the document in the in-process webview
            // (embedded fonts applied) and OCR it to confirm/refute the collision signal — the only way to
            // catch the localized/positional A3 attack on DOCX. Needs tessdata; degrades with a warning.
            #[cfg(all(feature = "render-wry", feature = "ocr-tesseract"))]
            if ocr {
                use chk_defaced::ocr::{OcrHint, TesseractOcr};
                match TesseractOcr::new(None, &ocr_lang, OcrHint::Block) {
                    Ok(eng) => {
                        for r in reports.iter_mut().filter(|r| r.format == "docx") {
                            let p = std::path::PathBuf::from(&r.file);
                            if let Err(e) = chk_defaced::render::verify_docx_with_render(r, &p, &eng) {
                                eprintln!("[warn] DOCX render-OCR verify for {}: {e:#}", r.file);
                            }
                        }
                    }
                    Err(e) => eprintln!("[warn] --ocr DOCX render disabled: {e:#}"),
                }
            }

            // Refresh the explicit document-level assessment after any OCR escalation raised severities.
            for r in reports.iter_mut() {
                r.finalize();
            }

            if format_out == "json" {
                println!("{}", serde_json::to_string_pretty(&reports)?);
            } else {
                for r in &reports {
                    print_human(r);
                }
            }
            // CI-friendly exit code: non-zero if any finding is High or worse.
            let worst = reports.iter().filter_map(|r| r.max_severity()).max();
            Ok(if matches!(worst, Some(s) if s >= Severity::High) {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            })
        }
    }
}

fn print_human(r: &Report) {
    println!("\n=== {} [{}] — {} font(s) examined ===", r.file, r.format, r.fonts_examined);

    // Explicit document-level verdict.
    if let Some(a) = r.assessment {
        let status = if a.defaced && a.hidden_text {
            "KO — DEFACED + HIDDEN TEXT"
        } else if a.defaced {
            "KO — DEFACED (font tampering)"
        } else if a.hidden_text {
            "KO — HIDDEN TEXT (invisible/cloaked)"
        } else if a.ok {
            "OK — no High-severity anomaly"
        } else {
            "OK"
        };
        println!("  Verdict: {status}");
    }

    // Provenance metadata — to evaluate the source.
    if let Some(m) = &r.metadata {
        let mut parts: Vec<String> = Vec::new();
        let mut add = |label: &str, v: &Option<String>| {
            if let Some(v) = v {
                parts.push(format!("{label}={v}"));
            }
        };
        add("author", &m.author);
        add("last_modified_by", &m.last_modified_by);
        add("tool", &m.creator_tool);
        add("producer", &m.producer);
        add("company", &m.company);
        add("created", &m.created);
        add("modified", &m.modified);
        add("revision", &m.revision);
        add("format", &m.format_version);
        if !parts.is_empty() {
            println!("  Source: {}", parts.join("  ·  "));
        }
    }

    if r.findings.is_empty() {
        println!("  no anomalies detected ✓");
        return;
    }
    let mut findings: Vec<_> = r.findings.iter().collect();
    findings.sort_by(|a, b| b.severity.cmp(&a.severity));
    for f in findings {
        println!(
            "  [{:?}] {} — {} ({}) conf={:.2}",
            f.severity, f.rule, f.message, f.location, f.confidence
        );
    }
    if let Some(v) = r.verdict {
        let note = match v {
            chk_defaced::finding::Verdict::Confirmed => "OCR confirms a real rendered/extracted divergence",
            chk_defaced::finding::Verdict::Refuted => "OCR refutes it — rendered text matches extracted (rig present but not used)",
            chk_defaced::finding::Verdict::Unconfirmed => "not verified (pass a --registry, or run --ocr on a PDF, to confirm or refute)",
        };
        println!("  Verdict: {v:?} — {note}");
    }
    if !r.phrases.is_empty() {
        println!("  Affected phrases (extracted → presumed{}):", if r.phrases.iter().any(|p| p.ocr.is_some()) { " → ocr" } else { "" });
        for p in r.phrases.iter().take(20) {
            if let Some(pg) = p.page {
                println!("    [page {pg}]");
            }
            println!("    read:     {}", p.extracted);
            println!("    presumed: {}", p.presumed);
            if let Some(ocr) = &p.ocr {
                println!("    ocr:      {ocr}");
            }
            println!();
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}
