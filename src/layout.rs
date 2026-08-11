//! Layout analysis via PP-DocLayout (S o V3), spec-driven.
//!
//! Porting Rust del layout analyzer PaddleX / PaddleOCR. La parte pura
//! (spec dei modelli, decodifica output, NMS, XY-Cut, ordinamento) vive in
//! `crate::pipeline::layout` — implementazione unica condivisa col wasm; qui
//! restano solo il preprocess `image`→tensore e la sessione `ort`.
//!
//! ## Pipeline
//!
//! 1. **Preprocess** (dal `inference.yml` del modello — vedi
//!    [`crate::pipeline::layout::LayoutModelSpec`]): resize a **stretch**
//!    (`keep_ratio: false`, NIENTE letterbox) alla dimensione del modello
//!    (480×480 per S, 800×800 per V3), interpolazione bicubica, `/255`,
//!    mean/std ImageNet solo se il modello li dichiara (S sì, V3 no:
//!    `norm_type: none`). HWC→CHW + batch.
//! 2. **Inference**: input forniti in base a quelli che la sessione
//!    espone — `image` sempre; `scale_factor` `[1,2]` **per-asse**
//!    `[input_h/h, input_w/w]`; `im_shape` solo se il modello lo ha (V3).
//! 3. **Output**: `[N, 6]` (S) o `[N, 7]` (V3):
//!    `[class_id, score, xmin, ymin, xmax, ymax, (reading_order)]`.
//! 4. **Postprocess** (condiviso): confidence filter → NMS solo se il
//!    modello NON la fa nel grafo (S sì nel grafo, V3 no) → reading order
//!    dal modello o via XY-Cut → sort.
//!
//! Il modello rimappa internamente le bbox tramite `scale_factor`, quindi
//! le coordinate finali sono già in pixel sull'immagine di input.

use crate::ocr_error::OcrError;
use ndarray::{Array, Array2, Array4};
use crate::pipeline::layout::LayoutModelSpec;
use ort::{
    inputs,
    session::{builder::GraphOptimizationLevel, Session},
    value::Tensor,
};
use std::path::Path;

/// Dimensione input della variante V3 (default storico di questo crate).
/// La dimensione effettiva viene dalla [`LayoutModelSpec`] dell'analyzer.
pub const LAYOUT_INPUT_SIZE: u32 = 800;

/// 25 classi di PP-DocLayoutV3 (ordine del suo `label_list`). Per
/// PP-DocLayout-S (23 classi, ordine diverso) il mapping passa dal label:
/// [`LayoutClass::from_name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LayoutClass {
    Abstract        = 0,
    Algorithm       = 1,
    AsideText       = 2,
    Chart           = 3,
    Content         = 4,
    DisplayFormula  = 5,
    DocTitle        = 6,
    FigureTitle     = 7,
    Footer          = 8,
    FooterImage     = 9,
    Footnote        = 10,
    FormulaNumber   = 11,
    Header          = 12,
    HeaderImage     = 13,
    Image           = 14,
    InlineFormula   = 15,
    Number          = 16,
    ParagraphTitle  = 17,
    Reference       = 18,
    ReferenceContent = 19,
    Seal            = 20,
    Table           = 21,
    Text            = 22,
    VerticalText    = 23,
    VisionFootnote  = 24,
}

impl LayoutClass {
    /// Mapping id → classe valido SOLO per l'ordine del label_list V3.
    /// Per modelli diversi usare [`Self::from_name`] col loro label_list.
    pub fn from_id(id: usize) -> Option<Self> {
        Some(match id {
            0 => Self::Abstract,
            1 => Self::Algorithm,
            2 => Self::AsideText,
            3 => Self::Chart,
            4 => Self::Content,
            5 => Self::DisplayFormula,
            6 => Self::DocTitle,
            7 => Self::FigureTitle,
            8 => Self::Footer,
            9 => Self::FooterImage,
            10 => Self::Footnote,
            11 => Self::FormulaNumber,
            12 => Self::Header,
            13 => Self::HeaderImage,
            14 => Self::Image,
            15 => Self::InlineFormula,
            16 => Self::Number,
            17 => Self::ParagraphTitle,
            18 => Self::Reference,
            19 => Self::ReferenceContent,
            20 => Self::Seal,
            21 => Self::Table,
            22 => Self::Text,
            23 => Self::VerticalText,
            24 => Self::VisionFootnote,
            _ => return None,
        })
    }

    /// Mapping label (snake_case del `label_list` yml) → classe. Copre i
    /// label di entrambi i modelli: quelli solo-S senza corrispondente
    /// esatto vengono approssimati (`formula`→DisplayFormula,
    /// `table_title`/`chart_title`→FigureTitle).
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "abstract"          => Self::Abstract,
            "algorithm"         => Self::Algorithm,
            "aside_text"        => Self::AsideText,
            "chart"             => Self::Chart,
            "content"           => Self::Content,
            "display_formula" | "formula" => Self::DisplayFormula,
            "doc_title"         => Self::DocTitle,
            "figure_title" | "table_title" | "chart_title" => Self::FigureTitle,
            "footer"            => Self::Footer,
            "footer_image"      => Self::FooterImage,
            "footnote"          => Self::Footnote,
            "formula_number"    => Self::FormulaNumber,
            "header"            => Self::Header,
            "header_image"      => Self::HeaderImage,
            "image"             => Self::Image,
            "inline_formula"    => Self::InlineFormula,
            "number"            => Self::Number,
            "paragraph_title"   => Self::ParagraphTitle,
            "reference"         => Self::Reference,
            "reference_content" => Self::ReferenceContent,
            "seal"              => Self::Seal,
            "table"             => Self::Table,
            "text"              => Self::Text,
            "vertical_text"     => Self::VerticalText,
            "vision_footnote"   => Self::VisionFootnote,
            _ => return None,
        })
    }

    /// Label snake_case della classe (inverso di [`Self::from_name`]).
    pub fn name(self) -> &'static str {
        match self {
            Self::Abstract         => "abstract",
            Self::Algorithm        => "algorithm",
            Self::AsideText        => "aside_text",
            Self::Chart            => "chart",
            Self::Content          => "content",
            Self::DisplayFormula   => "display_formula",
            Self::DocTitle         => "doc_title",
            Self::FigureTitle      => "figure_title",
            Self::Footer           => "footer",
            Self::FooterImage      => "footer_image",
            Self::Footnote         => "footnote",
            Self::FormulaNumber    => "formula_number",
            Self::Header           => "header",
            Self::HeaderImage      => "header_image",
            Self::Image            => "image",
            Self::InlineFormula    => "inline_formula",
            Self::Number           => "number",
            Self::ParagraphTitle   => "paragraph_title",
            Self::Reference        => "reference",
            Self::ReferenceContent => "reference_content",
            Self::Seal             => "seal",
            Self::Table            => "table",
            Self::Text             => "text",
            Self::VerticalText     => "vertical_text",
            Self::VisionFootnote   => "vision_footnote",
        }
    }

    /// Mapping a categoria semantica semplificata coerente col Python
    /// (`CLASS_MAPPING` in `analyzer.py`). 8 categorie: text/title/list/
    /// figure/table/header/footer/equation.
    pub fn semantic(self) -> SemanticClass {
        use LayoutClass::*;
        match self {
            DocTitle | ParagraphTitle              => SemanticClass::Title,
            Header                                 => SemanticClass::Header,
            Footer                                 => SemanticClass::Footer,
            Reference                              => SemanticClass::List,
            Chart | FooterImage | HeaderImage |
            Image | Seal                           => SemanticClass::Figure,
            Table                                  => SemanticClass::Table,
            DisplayFormula | InlineFormula         => SemanticClass::Equation,
            _                                      => SemanticClass::Text,
        }
    }
}

/// Categoria semantica semplificata (8 classi).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SemanticClass {
    Text,
    Title,
    List,
    Figure,
    Table,
    Header,
    Footer,
    Equation,
}

/// Bounding box di una regione layout, coordinate pixel sull'immagine di
/// input ORIGINALE (rescaling fatto internamente dal modello).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LayoutBox {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub class: LayoutClass,
    pub score: f32,
    /// Reading order (basso = prima nella lettura). Dal modello se lo
    /// emette (V3), altrimenti assegnato via XY-Cut nel postprocess
    /// condiviso. `-1` solo se non assegnabile.
    pub reading_order: i32,
}

impl LayoutBox {
    pub fn xmin(&self) -> u32 { self.x }
    pub fn ymin(&self) -> u32 { self.y }
    pub fn xmax(&self) -> u32 { self.x + self.w }
    pub fn ymax(&self) -> u32 { self.y + self.h }

    /// True se `(px, py)` cade nel rettangolo (inclusivo su left/top,
    /// esclusivo su right/bottom).
    pub fn contains(&self, px: u32, py: u32) -> bool {
        px >= self.xmin() && px < self.xmax()
            && py >= self.ymin() && py < self.ymax()
    }

    /// Distanza euclidea dal centro del box a `(px, py)`.
    pub fn distance_to(&self, px: u32, py: u32) -> f32 {
        let cx = self.x as f32 + self.w as f32 / 2.0;
        let cy = self.y as f32 + self.h as f32 / 2.0;
        let dx = cx - px as f32;
        let dy = cy - py as f32;
        (dx * dx + dy * dy).sqrt()
    }

    fn to_shared(&self) -> crate::pipeline::layout::LayoutBox {
        crate::pipeline::layout::LayoutBox {
            bbox: crate::pipeline::BoundingBox::from_lrtb(
                self.x as i32, self.y as i32,
                (self.x + self.w) as i32, (self.y + self.h) as i32,
            ),
            class_id: self.class as u32,
            label: self.class.name().to_string(),
            score: self.score,
            reading_order: self.reading_order,
        }
    }
}

/// Analyzer wrapper: `ort::Session` + [`LayoutModelSpec`] del modello.
pub struct LayoutAnalyzer {
    pub session:        Session,
    pub spec:           LayoutModelSpec,
    pub conf_thresh:    f32,
    pub nms_iou_thresh: f32,
}

impl LayoutAnalyzer {
    /// Carica il modello da file con la spec **V3** (default storico di
    /// questo crate). Per PP-DocLayout-S usare [`Self::from_path_with_spec`].
    pub fn from_path(model_path: impl AsRef<Path>) -> Result<Self, OcrError> {
        Self::from_path_with_spec(model_path, LayoutModelSpec::pp_doclayout_v3())
    }

    /// Carica il modello da file con la spec indicata
    /// (`LayoutModelSpec::pp_doclayout_s()` / `pp_doclayout_v3()`).
    pub fn from_path_with_spec(
        model_path: impl AsRef<Path>,
        spec: LayoutModelSpec,
    ) -> Result<Self, OcrError> {
        let session = Session::builder()?
            .with_optimization_level(GraphOptimizationLevel::Level3)?
            .commit_from_file(model_path)?;
        Ok(Self::from_session_with_spec(session, spec))
    }

    /// Costruttore da Session pre-caricata, spec V3 (compat).
    pub fn from_session(session: Session) -> Self {
        Self::from_session_with_spec(session, LayoutModelSpec::pp_doclayout_v3())
    }

    pub fn from_session_with_spec(session: Session, spec: LayoutModelSpec) -> Self {
        Self {
            session,
            spec,
            conf_thresh:    0.50,
            nms_iou_thresh: 0.50,
        }
    }

    /// Esegue layout analysis. Ritorna le [`LayoutBox`] filtrate per
    /// confidence, con NMS (solo se non già nel grafo del modello),
    /// reading order (dal modello o XY-Cut) e ordinate per lettura.
    pub fn analyze(&mut self, image: &image::RgbImage) -> Result<Vec<LayoutBox>, OcrError> {
        // ── Step 1: preprocess (stretch + norm dalla spec) ──────────────
        let blob = preprocess(image, &self.spec);
        let sf = self.spec.scale_factor(image.width(), image.height());
        let scale_factor: Array2<f32> = ndarray::arr2(&[sf]);

        // ── Step 2: inference — input in base a cosa espone la sessione ─
        // S ha [image, scale_factor]; V3 ha [im_shape, image, scale_factor].
        let names: Vec<String> = self.session.inputs().iter().map(|i| i.name().to_string()).collect();
        if names.is_empty() {
            return Err(OcrError::ModelInput("layout: sessione senza input".into()));
        }
        let name_image = names.iter().find(|n| n.as_str() == "image").cloned()
            // Fallback posizionale: nell'ordine canonico Paddle l'input
            // immagine è il secondo quando c'è im_shape, altrimenti il primo.
            .unwrap_or_else(|| names[if names.len() >= 3 { 1 } else { 0 }].clone());
        let name_scale = names.iter().find(|n| n.as_str() == "scale_factor").cloned()
            .unwrap_or_else(|| names[names.len() - 1].clone());
        let name_im_shape = names.iter().find(|n| n.as_str() == "im_shape").cloned();

        let image_t = Tensor::from_array(blob)?;
        let sf_t    = Tensor::from_array(scale_factor)?;

        let outputs = match name_im_shape {
            Some(nis) => {
                let im_shape: Array2<f32> = ndarray::arr2(&[[
                    self.spec.input_h as f32, self.spec.input_w as f32,
                ]]);
                let im_shape_t = Tensor::from_array(im_shape)?;
                self.session.run(inputs![
                    nis        => im_shape_t,
                    name_image => image_t,
                    name_scale => sf_t,
                ])?
            }
            None => self.session.run(inputs![
                name_image => image_t,
                name_scale => sf_t,
            ])?,
        };

        // ── Step 3: parse output primario [N, 6|7] ──────────────────────
        let (_, primary) = outputs.iter().next()
            .ok_or_else(|| OcrError::ModelOutput("layout: nessun output".into()))?;
        let (shape_vec, raw_data) = crate::compat::tensor_extract_with_shape_f32(&primary)?;
        let n_cols = if shape_vec.len() > 1 { shape_vec[1] as usize } else { 0 };
        if n_cols < 6 {
            return Err(OcrError::ModelOutput(format!(
                "layout: output cols={n_cols} (atteso ≥6)",
            )));
        }

        // ── Step 4: postprocess condiviso (stesso path del wasm) ────────
        let shared = crate::pipeline::layout::postprocess_layout(
            &raw_data, n_cols, &self.spec,
            self.conf_thresh, self.nms_iou_thresh,
            image.width(), image.height(),
        );

        Ok(shared.into_iter().map(|b| LayoutBox {
            x: b.bbox.left.max(0) as u32,
            y: b.bbox.top.max(0) as u32,
            w: b.bbox.width() as u32,
            h: b.bbox.height() as u32,
            class: LayoutClass::from_name(&b.label).unwrap_or(LayoutClass::Text),
            score: b.score,
            reading_order: b.reading_order,
        }).collect())
    }
}

/// Preprocess dalla spec: resize a **stretch** (`keep_ratio: false`) alla
/// dimensione del modello, interpolazione bicubica (CatmullRom), `/255` se
/// `is_scale`, mean/std solo se il modello li dichiara. HWC→CHW + batch.
fn preprocess(image: &image::RgbImage, spec: &LayoutModelSpec) -> Array4<f32> {
    let resized = image::imageops::resize(
        image, spec.input_w, spec.input_h, image::imageops::FilterType::CatmullRom,
    );
    let (w, h) = (spec.input_w as usize, spec.input_h as usize);
    let mut blob: Array4<f32> = Array::zeros((1, 3, h, w));
    for y in 0..h {
        for x in 0..w {
            let pixel = resized.get_pixel(x as u32, y as u32);
            for c in 0..3 {
                let mut v = pixel[c] as f32;
                if spec.is_scale {
                    v /= 255.0;
                }
                if let (Some(mean), Some(std)) = (spec.mean, spec.std) {
                    v = (v - mean[c]) / std[c];
                }
                blob[[0, c, y, x]] = v;
            }
        }
    }
    blob
}

// ─── XY-Cut reading-order ─────────────────────────────────────────────────────

/// Ordina i layout-box in reading order con XY-Cut. Wrapper del
/// [`crate::pipeline::layout::xy_cut_order`] condiviso (implementazione unica
/// nativa/wasm). Ritorna gli indici in `boxes` nell'ordine di lettura.
pub fn xy_cut_order(boxes: &[LayoutBox]) -> Vec<usize> {
    let rects: Vec<crate::pipeline::BoundingBox> = boxes.iter()
        .map(|b| crate::pipeline::BoundingBox::from_lrtb(
            b.x as i32, b.y as i32, (b.x + b.w) as i32, (b.y + b.h) as i32,
        ))
        .collect();
    crate::pipeline::layout::xy_cut_order(&rects)
}

/// Conversione batch verso i [`crate::pipeline::layout::LayoutBox`] condivisi
/// (usata da `detect_with_layout` per l'associazione generica).
pub(crate) fn to_shared_boxes(boxes: &[LayoutBox]) -> Vec<crate::pipeline::layout::LayoutBox> {
    boxes.iter().map(|b| b.to_shared()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lb(x: u32, y: u32, w: u32, h: u32, class: LayoutClass, score: f32, ro: i32) -> LayoutBox {
        LayoutBox { x, y, w, h, class, score, reading_order: ro }
    }

    #[test]
    fn semantic_mapping() {
        assert_eq!(LayoutClass::DocTitle.semantic(),       SemanticClass::Title);
        assert_eq!(LayoutClass::ParagraphTitle.semantic(), SemanticClass::Title);
        assert_eq!(LayoutClass::Image.semantic(),          SemanticClass::Figure);
        assert_eq!(LayoutClass::Table.semantic(),          SemanticClass::Table);
        assert_eq!(LayoutClass::Footer.semantic(),         SemanticClass::Footer);
        assert_eq!(LayoutClass::DisplayFormula.semantic(), SemanticClass::Equation);
        assert_eq!(LayoutClass::Text.semantic(),           SemanticClass::Text);
        assert_eq!(LayoutClass::VerticalText.semantic(),   SemanticClass::Text);
    }

    #[test]
    fn from_name_roundtrip_and_s_only_labels() {
        // Roundtrip sui 25 nomi canonici V3.
        for id in 0..25 {
            let c = LayoutClass::from_id(id).unwrap();
            assert_eq!(LayoutClass::from_name(c.name()), Some(c));
        }
        // Label solo-S approssimati.
        assert_eq!(LayoutClass::from_name("formula"),     Some(LayoutClass::DisplayFormula));
        assert_eq!(LayoutClass::from_name("table_title"), Some(LayoutClass::FigureTitle));
        assert_eq!(LayoutClass::from_name("chart_title"), Some(LayoutClass::FigureTitle));
        assert_eq!(LayoutClass::from_name("boh"),         None);
    }

    #[test]
    fn s_labels_all_map_to_a_class() {
        for l in crate::pipeline::layout::PP_DOCLAYOUT_S_LABELS {
            assert!(LayoutClass::from_name(l).is_some(), "label S non mappato: {l}");
        }
    }

    #[test]
    fn layout_box_contains_and_distance() {
        let b = lb(100, 100, 200, 50, LayoutClass::Text, 0.9, 0);
        assert!(b.contains(150, 120));
        assert!(!b.contains(50, 120));
        assert!(!b.contains(350, 120));
        assert!(b.distance_to(200, 125) < 0.01);
    }

    #[test]
    fn preprocess_stretches_without_letterbox() {
        // 600×300 su S (480×480): stretch pieno, nessun padding — il blob
        // non deve contenere la zona a zero del letterbox.
        let mut img = image::RgbImage::new(600, 300);
        for p in img.pixels_mut() { *p = image::Rgb([255, 255, 255]); }
        let spec = LayoutModelSpec::pp_doclayout_s();
        let blob = preprocess(&img, &spec);
        assert_eq!(blob.shape(), &[1, 3, 480, 480]);
        // Bianco /255 → 1.0 → ImageNet: (1−0.485)/0.229 ≈ 2.249 sul canale R,
        // anche nell'angolo in basso a destra (che col letterbox era pad=0).
        let v = blob[[0, 0, 479, 479]];
        assert!((v - (1.0 - 0.485) / 0.229).abs() < 1e-3, "trovato {v}");
    }

    #[test]
    fn preprocess_v3_scales_without_imagenet() {
        // V3: norm_type none → solo /255 (is_scale default true in
        // PaddleDetection), niente mean/std.
        let mut img = image::RgbImage::new(10, 10);
        for p in img.pixels_mut() { *p = image::Rgb([255, 255, 255]); }
        let spec = LayoutModelSpec::pp_doclayout_v3();
        let blob = preprocess(&img, &spec);
        assert_eq!(blob.shape(), &[1, 3, 800, 800]);
        assert!((blob[[0, 0, 400, 400]] - 1.0).abs() < 1e-3);
    }

    #[test]
    fn xy_cut_wrapper_two_columns() {
        let boxes = vec![
            lb(300, 500, 400, 400, LayoutClass::Text, 0.9, -1),     // colonna dx
            lb(50, 50, 900, 100, LayoutClass::DocTitle, 0.9, -1),   // titolo
            lb(0, 500, 250, 400, LayoutClass::Text, 0.9, -1),       // colonna sx
        ];
        assert_eq!(xy_cut_order(&boxes), vec![1, 2, 0]);
    }
}
