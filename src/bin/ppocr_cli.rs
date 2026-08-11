//! ppocr-cli — interfaccia subprocess JSON per integrazione con pipeline esterne.
//!
//! ## Usage
//!
//! ```text
//! ppocr-cli <image_path> [--mode full|ori-layout] [--upright-out PATH]
//!           [--layout s|v3] [--preprocess off|normal|strong]
//! ppocr-cli --mode associate <input.json>
//! ppocr-cli --mode plan <input.json>
//! ```
//!
//! ### Mode `full` (default)
//! Pipeline Paddle completa: ① ori → rotate → ③ layout → PP-OCR det+rec.
//!
//! ### Mode `ori-layout`
//! Stadi ① + ⓪ + ③ (senza riconoscimento), allineati a README / `web/app.js`:
//!   ① DocOrientationClassifier (PP-LCNet)
//!   rotate → bitmap upright
//!   ⓪ preprocess (`--preprocess`, default `normal`)
//!   ③ PP-DocLayout-S (default) / V3
//! Output JSON con `layout_boxes` (label snake_case + score) e `words: []`.
//!
//! ### Mode `plan`
//! Input JSON (`page_width/height`, `layout_boxes`, opz. `words`) →
//! ③b `ocr_skip_regions`, bande `split_columns`, ②c `heading_regions`.
//!
//! ### Mode `associate`
//! Input JSON (`page_angle`, `layout_boxes`, `words` con `line_idx`) →
//! `associate_lines` (stesso algoritmo di wasm/Tauri) → words riordinate
//! con `layout_idx` popolato.
//!
//! ## Env vars
//!
//! | Variabile            | Default                                      |
//! |----------------------|----------------------------------------------|
//! | `PPOCR_LAYOUT_MODEL` | `$PPOCR_MODELS_DIR/layout/PP-DocLayout-S.onnx` |
//! | `PPOCR_LAYOUT_SPEC`  | `s` (`s`/`v3`)                               |
//! | `PPOCR_ORI_MODEL`    | `$PPOCR_MODELS_DIR/orientation/….onnx` o hub  |
//! | `PPOCR_MODELS_DIR`   | `models/paddleocr`                           |
//! | `PPOCR_TIER`         | `tiny` (solo mode `full`)                    |
//! | `PPOCR_NUM_THREADS`  | `4`                                          |

use crate::pipeline::layout::{
    self as pipe_layout, HeadingOptions, LayoutModelSpec, OcrSkipOptions,
};
use crate::pipeline::preprocess::{preprocess_rgba_level, PreprocessLevel};
use crate::pipeline::BoundingBox as PipelineBBox;
use ppocr_rs::{
    DocOrientation, DocOrientationClassifier, LayoutAnalyzer, LayoutBox, ModelHub, OcrLite,
    OcrOptions, Point, PpOcrVersion, PpStructureModel, SemanticClass,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

// ─── Strutture JSON ───────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
struct CliOutput {
    page_angle: u32,
    page_width: u32,
    page_height: u32,
    layout_boxes: Vec<CliLayoutBox>,
    words: Vec<CliWord>,
}

#[derive(Serialize, Deserialize, Clone)]
struct CliLayoutBox {
    class: String,
    semantic: String,
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    reading_order: i32,
    /// Confidenza layout (0–1). Necessaria a ③b/②c; default 1.0 per JSON vecchi.
    #[serde(default = "default_layout_score")]
    score: f32,
}

fn default_layout_score() -> f32 {
    1.0
}

#[derive(Serialize, Deserialize, Clone)]
struct CliWord {
    text: String,
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    confidence: f32,
    layout_idx: i32,
    line_idx: i32,
}

#[derive(Serialize, Deserialize)]
struct PlanOutput {
    skip: Vec<CliLayoutBox>,
    skip_rejected: Vec<(String, f32, String)>,
    bands: Vec<CliBand>,
    columns: usize,
    headings: Vec<CliLayoutBox>,
    heading_rejected: Vec<(String, f32, String)>,
    heading_upscale: f32,
    heading_psm: u32,
}

#[derive(Serialize, Deserialize, Clone)]
struct CliBand {
    left: i32,
    right: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Full,
    OriLayout,
    Associate,
    Plan,
}

// ─── Entry point ─────────────────────────────────────────────────────────────

fn main() {
    if let Err(e) = run() {
        eprintln!("[ppocr-cli] {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args.iter().any(|a| a == "--help" || a == "-h") {
        print_help();
        std::process::exit(0);
    }

    let mode = parse_mode(&args);
    match mode {
        Mode::Associate => run_associate(&args),
        Mode::OriLayout => run_ori_layout(&args),
        Mode::Plan => run_plan(&args),
        Mode::Full => run_full(&args),
    }
}

fn parse_mode(args: &[String]) -> Mode {
    let mut mode = Mode::Full;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--mode" {
            if let Some(v) = args.get(i + 1) {
                mode = match v.as_str() {
                    "full" | "paddle" => Mode::Full,
                    "ori-layout" | "layout" => Mode::OriLayout,
                    "associate" => Mode::Associate,
                    "plan" => Mode::Plan,
                    _ => mode,
                };
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    mode
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].as_str())
}

fn positional_path(args: &[String]) -> Option<&str> {
    let mut i = 1;
    while i < args.len() {
        if args[i].starts_with("--") {
            // flag con valore
            if matches!(
                args[i].as_str(),
                "--mode"
                    | "--upright-out"
                    | "--layout"
                    | "--layout-spec"
                    | "--preprocess"
            ) {
                i += 2;
                continue;
            }
            i += 1;
            continue;
        }
        return Some(args[i].as_str());
    }
    None
}

// ─── Mode: full (Paddle OCR) ─────────────────────────────────────────────────

fn run_full(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let img_path = positional_path(args).ok_or("uso: ppocr-cli <image> [--mode full]")?;
    let img = image::open(img_path)
        .map_err(|e| format!("apertura immagine {img_path:?}: {e}"))?
        .to_rgb8();

    let tier = match std::env::var("PPOCR_TIER").as_deref() {
        Ok("small") => PpOcrVersion::V6Small,
        Ok("medium") => PpOcrVersion::V6Medium,
        _ => PpOcrVersion::V6Tiny,
    };
    let num_threads: usize = std::env::var("PPOCR_NUM_THREADS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);

    let hub = ModelHub::with_default_cache()?;
    let ocr_p = hub.ensure(tier)?;
    let ori_path = ori_model_path(&hub)?;
    let (lay_path, lay_spec) = layout_model_and_spec(args, "v3")?;
    if !lay_path.exists() {
        return Err(format!(
            "modello layout non trovato: {} (set PPOCR_LAYOUT_MODEL o PPOCR_MODELS_DIR)",
            lay_path.display()
        )
        .into());
    }

    let ori_clf = DocOrientationClassifier::from_path(&ori_path)?;
    let (orient, _conf) = ori_clf.classify(&img)?;
    let page_angle = orient.degrees();
    let upright = rotate_to_upright(img, orient);
    let (out_w, out_h) = upright.dimensions();

    let mut ocr = OcrLite::new();
    ocr.init_models_no_angle(
        ocr_p.det_onnx.to_str().unwrap(),
        ocr_p.rec_onnx.to_str().unwrap(),
        ocr_p.dict_txt.to_str().unwrap(),
        num_threads,
    )?;

    let mut layout = LayoutAnalyzer::from_path_with_spec(&lay_path, lay_spec)?;
    let opts = OcrOptions {
        return_word_box: true,
        use_doc_orientation: false,
        ..OcrOptions::default()
    };
    let result = ocr.detect_with_layout(
        &upright, &mut layout, 10, 960, 0.6, 0.3, 1.6, false, false, opts,
    )?;

    let layout_boxes = cli_layout_boxes(&result.layout_boxes);
    let mut words: Vec<CliWord> = Vec::new();
    for (line_idx, blk) in result.blocks.iter().enumerate() {
        let layout_idx = blk.layout_index.map(|i| i as i32).unwrap_or(-1);
        let line_idx = line_idx as i32;
        if blk.block.words.is_empty() {
            let (x1, y1, x2, y2) = aabb_points(&blk.block.box_points);
            words.push(CliWord {
                text: blk.block.text.clone(),
                x1,
                y1,
                x2,
                y2,
                confidence: blk.block.text_score,
                layout_idx,
                line_idx,
            });
        } else {
            for w in &blk.block.words {
                let (x1, y1, x2, y2) = aabb_points(&w.box_points);
                words.push(CliWord {
                    text: w.text.clone(),
                    x1,
                    y1,
                    x2,
                    y2,
                    confidence: w.score,
                    layout_idx,
                    line_idx,
                });
            }
        }
    }

    print_json(&CliOutput {
        page_angle,
        page_width: out_w,
        page_height: out_h,
        layout_boxes,
        words,
    })
}

// ─── Mode: ori-layout (README ①+⓪+③) ─────────────────────────────────────────

fn run_ori_layout(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let img_path = positional_path(args).ok_or(
        "uso: ppocr-cli <image> --mode ori-layout [--upright-out PATH] \
         [--layout s|v3] [--preprocess off|normal|strong]",
    )?;
    let img = image::open(img_path)
        .map_err(|e| format!("apertura immagine {img_path:?}: {e}"))?
        .to_rgb8();

    let hub = ModelHub::with_default_cache()?;
    let ori_path = ori_model_path(&hub)?;
    let (lay_path, lay_spec) = layout_model_and_spec(args, "s")?;
    if !lay_path.exists() {
        return Err(format!(
            "modello layout non trovato: {} (set PPOCR_LAYOUT_MODEL)",
            lay_path.display()
        )
        .into());
    }

    let ori_clf = DocOrientationClassifier::from_path(&ori_path)?;
    let (orient, _conf) = ori_clf.classify(&img)?;
    let page_angle = orient.degrees();
    let mut upright = rotate_to_upright(img, orient);

    // ⓪ pulizia sul bitmap raddrizzato (stesso ordine di web/app.js).
    let level = preprocess_level_from_args(args);
    if level != PreprocessLevel::Off {
        upright = apply_preprocess(upright, level)?;
    }

    let (out_w, out_h) = upright.dimensions();

    if let Some(out) = flag_value(args, "--upright-out") {
        upright
            .save(out)
            .map_err(|e| format!("scrittura upright {out}: {e}"))?;
    }

    let mut layout = LayoutAnalyzer::from_path_with_spec(&lay_path, lay_spec)?;
    // Layout sull'immagine upright (eventualmente pulita).
    let boxes = layout.analyze(&upright)?;
    let layout_boxes = cli_layout_boxes(&boxes);

    print_json(&CliOutput {
        page_angle,
        page_width: out_w,
        page_height: out_h,
        layout_boxes,
        words: Vec::new(),
    })
}

// ─── Mode: plan (③b skip + bande + ②c headings) ─────────────────────────────

fn run_plan(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let path = positional_path(args).ok_or(
        "uso: ppocr-cli --mode plan <input.json>  \
         (JSON con page_width/height + layout_boxes; words opzionali)",
    )?;
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("lettura {path}: {e}"))?;
    let input: CliOutput = serde_json::from_str(&raw)
        .map_err(|e| format!("JSON non valido: {e}"))?;

    let spec = layout_spec_from_args(args, "s");
    let page_w = input.page_width;
    let page_h = input.page_height;

    let shared_boxes = cli_to_shared_layout(&input.layout_boxes);

    let skip_opts = OcrSkipOptions::for_spec(&spec);
    let skip_plan = pipe_layout::ocr_skip_regions(&shared_boxes, page_w, page_h, &skip_opts);

    // Bande dalle PAROLE (come app.js), non dalle righe fuse.
    let word_boxes: Vec<PipelineBBox> = input
        .words
        .iter()
        .filter(|w| !w.text.trim().is_empty())
        .map(|w| PipelineBBox::from_lrtb(w.x1 as i32, w.y1 as i32, w.x2 as i32, w.y2 as i32))
        .collect();
    let bands_raw = pipe_layout::split_columns(&word_boxes);
    let columns = pipe_layout::detect_columns(&word_boxes);
    let bands: Vec<CliBand> = bands_raw
        .iter()
        .map(|b| CliBand {
            left: b.left,
            right: b.right,
        })
        .collect();

    // Line boxes per heading_regions: AABB per line_idx.
    let mut lines_map: BTreeMap<i32, Vec<&CliWord>> = BTreeMap::new();
    for w in &input.words {
        if w.text.trim().is_empty() {
            continue;
        }
        lines_map.entry(w.line_idx).or_default().push(w);
    }
    let line_bboxes: Vec<PipelineBBox> = lines_map
        .values()
        .map(|ws| {
            let x1 = ws.iter().map(|w| w.x1).min().unwrap_or(0);
            let y1 = ws.iter().map(|w| w.y1).min().unwrap_or(0);
            let x2 = ws.iter().map(|w| w.x2).max().unwrap_or(0);
            let y2 = ws.iter().map(|w| w.y2).max().unwrap_or(0);
            PipelineBBox::from_lrtb(x1 as i32, y1 as i32, x2 as i32, y2 as i32)
        })
        .collect();

    let head_opts = HeadingOptions::for_spec(&spec);
    let head_plan =
        pipe_layout::heading_regions(&shared_boxes, &line_bboxes, page_w, page_h, &head_opts);

    let out = PlanOutput {
        skip: shared_to_cli(&skip_plan.skip),
        skip_rejected: skip_plan.rejected,
        bands,
        columns,
        headings: shared_to_cli(&head_plan.regions),
        heading_rejected: head_plan.rejected,
        heading_upscale: head_opts.upscale,
        heading_psm: head_opts.psm,
    };
    println!("{}", serde_json::to_string(&out)?);
    Ok(())
}

// ─── Mode: associate (pipeline.md ④) ─────────────────────────────────────────

fn run_associate(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let path = positional_path(args).ok_or(
        "uso: ppocr-cli --mode associate <input.json>  \
         (JSON con layout_boxes + words con line_idx)",
    )?;
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("lettura {path}: {e}"))?;
    let mut input: CliOutput = serde_json::from_str(&raw)
        .map_err(|e| format!("JSON non valido: {e}"))?;

    // Raggruppa parole per line_idx → bbox di riga (come fa wasm con lines).
    let mut lines_map: BTreeMap<i32, Vec<usize>> = BTreeMap::new();
    for (i, w) in input.words.iter().enumerate() {
        if w.text.trim().is_empty() {
            continue;
        }
        lines_map.entry(w.line_idx).or_default().push(i);
    }

    let line_ids: Vec<i32> = lines_map.keys().copied().collect();
    let line_bboxes: Vec<PipelineBBox> = line_ids
        .iter()
        .map(|lid| {
            let idxs = &lines_map[lid];
            let x1 = idxs.iter().map(|&i| input.words[i].x1).min().unwrap_or(0);
            let y1 = idxs.iter().map(|&i| input.words[i].y1).min().unwrap_or(0);
            let x2 = idxs.iter().map(|&i| input.words[i].x2).max().unwrap_or(0);
            let y2 = idxs.iter().map(|&i| input.words[i].y2).max().unwrap_or(0);
            PipelineBBox::from_lrtb(x1 as i32, y1 as i32, x2 as i32, y2 as i32)
        })
        .collect();

    let shared_boxes = cli_to_shared_layout(&input.layout_boxes);

    let assoc = pipe_layout::associate_lines(&line_bboxes, &shared_boxes);

    // Propaga layout_idx e riordina le parole secondo assoc.order.
    let mut out_words: Vec<CliWord> = Vec::with_capacity(input.words.len());
    for &line_pos in &assoc.order {
        let lid = line_ids[line_pos];
        let layout_idx = assoc.line_to_box[line_pos]
            .map(|i| i as i32)
            .unwrap_or(-1);
        let mut idxs = lines_map[&lid].clone();
        idxs.sort_by_key(|&i| (input.words[i].x1, input.words[i].y1));
        for i in idxs {
            let mut w = input.words[i].clone();
            w.layout_idx = layout_idx;
            // Rinumerazione stabile post-ordine di lettura.
            w.line_idx = line_pos as i32;
            out_words.push(w);
        }
    }

    input.words = out_words;
    print_json(&input)
}

// ─── Helpers ─────────────────────────────────────────────────────────────────

fn print_json(out: &CliOutput) -> Result<(), Box<dyn std::error::Error>> {
    println!("{}", serde_json::to_string(out)?);
    Ok(())
}

fn semantic_name(s: SemanticClass) -> &'static str {
    match s {
        SemanticClass::Text => "text",
        SemanticClass::Title => "title",
        SemanticClass::List => "list",
        SemanticClass::Figure => "figure",
        SemanticClass::Table => "table",
        SemanticClass::Header => "header",
        SemanticClass::Footer => "footer",
        SemanticClass::Equation => "equation",
    }
}

fn cli_layout_boxes(boxes: &[LayoutBox]) -> Vec<CliLayoutBox> {
    boxes
        .iter()
        .map(|lb| CliLayoutBox {
            class: lb.class.name().to_string(),
            semantic: semantic_name(lb.class.semantic()).to_string(),
            x1: lb.xmin(),
            y1: lb.ymin(),
            x2: lb.xmax(),
            y2: lb.ymax(),
            reading_order: lb.reading_order,
            score: lb.score,
        })
        .collect()
}

fn shared_to_cli(boxes: &[pipe_layout::LayoutBox]) -> Vec<CliLayoutBox> {
    boxes
        .iter()
        .map(|b| CliLayoutBox {
            class: b.label.clone(),
            semantic: b.label.clone(),
            x1: b.bbox.left.max(0) as u32,
            y1: b.bbox.top.max(0) as u32,
            x2: b.bbox.right.max(0) as u32,
            y2: b.bbox.bottom.max(0) as u32,
            reading_order: b.reading_order,
            score: b.score,
        })
        .collect()
}

fn cli_to_shared_layout(boxes: &[CliLayoutBox]) -> Vec<pipe_layout::LayoutBox> {
    boxes
        .iter()
        .enumerate()
        .map(|(i, b)| pipe_layout::LayoutBox {
            bbox: PipelineBBox::from_lrtb(b.x1 as i32, b.y1 as i32, b.x2 as i32, b.y2 as i32),
            class_id: i as u32,
            label: b.class.clone(),
            score: b.score,
            reading_order: b.reading_order,
        })
        .collect()
}

fn preprocess_level_from_args(args: &[String]) -> PreprocessLevel {
    let key = flag_value(args, "--preprocess")
        .map(|s| s.to_string())
        .or_else(|| std::env::var("OCR_PREPROCESS").ok())
        .unwrap_or_else(|| "normal".into());
    PreprocessLevel::parse(&key).unwrap_or(PreprocessLevel::Normal)
}

fn apply_preprocess(
    rgb: image::RgbImage,
    level: PreprocessLevel,
) -> Result<image::RgbImage, Box<dyn std::error::Error>> {
    let (w, h) = rgb.dimensions();
    let rgba = image::DynamicImage::ImageRgb8(rgb).to_rgba8();
    let mut buf = rgba.into_raw();
    let stats = preprocess_rgba_level(&mut buf, w as usize, h as usize, level);
    eprintln!(
        "[ppocr-cli] ⓪ preprocess={}: black_px={} noise_blobs={} rule_bands={}",
        level.as_str(),
        stats.black_pixels_cleared,
        stats.noise_blobs_removed,
        stats.rule_bands_found
    );
    let rgba = image::RgbaImage::from_raw(w, h, buf)
        .ok_or("buffer preprocess RGBA non valido")?;
    Ok(image::DynamicImage::ImageRgba8(rgba).to_rgb8())
}

fn rotate_to_upright(img: image::RgbImage, orient: DocOrientation) -> image::RgbImage {
    match orient {
        DocOrientation::Deg0 => img,
        DocOrientation::Deg90 => image::imageops::rotate270(&img),
        DocOrientation::Deg180 => image::imageops::rotate180(&img),
        DocOrientation::Deg270 => image::imageops::rotate90(&img),
    }
}

fn aabb_points(pts: &[Point]) -> (u32, u32, u32, u32) {
    let x1 = pts.iter().map(|p| p.x).min().unwrap_or(0);
    let y1 = pts.iter().map(|p| p.y).min().unwrap_or(0);
    let x2 = pts.iter().map(|p| p.x).max().unwrap_or(0);
    let y2 = pts.iter().map(|p| p.y).max().unwrap_or(0);
    (x1, y1, x2, y2)
}

fn models_dir() -> PathBuf {
    PathBuf::from(
        std::env::var("PPOCR_MODELS_DIR").unwrap_or_else(|_| "models/paddleocr".into()),
    )
}

fn ori_model_path(hub: &ModelHub) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Ok(p) = std::env::var("PPOCR_ORI_MODEL") {
        return Ok(PathBuf::from(p));
    }
    let local = models_dir().join("orientation/PP-LCNet_x1_0_doc_ori.onnx");
    if local.exists() {
        return Ok(local);
    }
    let paths = hub.ensure_single(PpStructureModel::DocOrientation)?;
    Ok(paths.onnx)
}

fn layout_spec_from_args(args: &[String], default: &str) -> LayoutModelSpec {
    let key = flag_value(args, "--layout")
        .or_else(|| flag_value(args, "--layout-spec"))
        .map(|s| s.to_string())
        .or_else(|| std::env::var("PPOCR_LAYOUT_SPEC").ok())
        .unwrap_or_else(|| default.to_string());
    match key.to_ascii_lowercase().as_str() {
        "v3" | "pp-doclayoutv3" => LayoutModelSpec::pp_doclayout_v3(),
        "m" | "pp-doclayout-m" => LayoutModelSpec::pp_doclayout_m(),
        _ => LayoutModelSpec::pp_doclayout_s(),
    }
}

fn layout_model_and_spec(
    args: &[String],
    default_spec: &str,
) -> Result<(PathBuf, LayoutModelSpec), Box<dyn std::error::Error>> {
    let spec = layout_spec_from_args(args, default_spec);
    if let Ok(p) = std::env::var("PPOCR_LAYOUT_MODEL") {
        return Ok((PathBuf::from(p), spec));
    }
    let base = models_dir().join("layout");
    let name = match spec.id {
        "PP-DocLayoutV3" => "PP-DocLayoutV3.onnx",
        "PP-DocLayout-M" => "PP-DocLayout-M.onnx",
        _ => "PP-DocLayout-S.onnx",
    };
    let path = base.join(name);
    // Fallback V3 int8 se il fp32 non c'è (repo lo rimuove).
    if !path.exists() && spec.id == "PP-DocLayoutV3" {
        let int8 = base.join("PP-DocLayoutV3.int8.onnx");
        if int8.exists() {
            return Ok((int8, spec));
        }
    }
    Ok((path, spec))
}

fn print_help() {
    eprintln!(
        "ppocr-cli <image> [--mode full|ori-layout] [--upright-out PATH] \
[--layout s|m|v3] [--preprocess off|normal|strong]
ppocr-cli --mode associate <input.json>
ppocr-cli --mode plan <input.json>

Modes:
  full         ① ori + ③ layout + PP-OCR det/rec (default layout v3)
  ori-layout   ① ori + ⓪ preprocess + ③ layout  (README; default layout s)
  associate    ④ associate_lines su JSON con layout_boxes + words
  plan         ③b skip + bande + ②c headings da JSON

Env:
  PPOCR_MODELS_DIR    default models/paddleocr
  PPOCR_LAYOUT_MODEL  path ONNX layout
  PPOCR_LAYOUT_SPEC   s|m|v3
  PPOCR_ORI_MODEL     path ONNX orientation
  PPOCR_TIER          tiny|small|medium (solo full)
  PPOCR_NUM_THREADS   default 4
  OCR_PREPROCESS      off|normal|strong (default normal, solo ori-layout)"
    );
}
