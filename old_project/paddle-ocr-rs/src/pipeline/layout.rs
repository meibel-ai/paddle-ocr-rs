// ─────────────────────────────────────────────────────────────────────────────
// PROVENIENZA — modulo assorbito da `ocr-pipeline` (workspace ocr-wasm),
// file d'origine `crates/ocr-pipeline/src/layout.rs`.
// Copyright (c) 2026 Dario Finardi / Jugaad s.r.l.
//
// Perche' e' qui: la logica e' ORIGINALE (non deriva dall'upstream Apache-2.0 del motore) ed
// e' condivisa con la pipeline browser, dove `ocr-pipeline` la compila su wasm32. Questo crate
// e' il target NATIVO e ne porta la propria copia per essere autosufficiente. Le due copie
// vanno tenute allineate a mano quando la logica cambia.
//
// Il codice qui e' quello originale; ogni modifica locale e' annotata con [MODIFICA locale].
// ─────────────────────────────────────────────────────────────────────────────

// Copyright (c) 2026 Dario Finardi / Jugaad s.r.l.
// https://omissis.ai
// All rights reserved. Distribuzione vietata senza autorizzazione scritta.

//! Layout analysis engine-agnostic: decodifica dell'output del modello,
//! NMS, reading order (XY-Cut) e associazione linee OCR ↔ layout-box.
//!
//! Modulo **puro** — niente `ort`, niente `image`, niente FFI — compila su
//! `wasm32-unknown-unknown`. È l'implementazione unica usata da:
//!
//! - `wasm-frontend`: il browser fa l'inferenza con onnxruntime-web e passa
//!   qui l'output raw del modello;
//! - `ppocr-rs` (nativo): `LayoutAnalyzer::analyze` e `detect_with_layout`
//!   delegano a queste funzioni, così il riferimento nativo e il wasm
//!   condividono la stessa logica invece di derivarne due copie.
//!
//! ## Specifiche dei modelli
//!
//! Il preprocess NON è deducibile dal grafo ONNX: sta nell'`inference.yml`
//! che accompagna i pesi ([`LayoutModelSpec`] li trascrive). Entrambi i
//! modelli usano `keep_ratio: false` — resize a **stretch**, senza
//! letterbox — e `NormalizeImage` con `is_scale` default **true** in
//! PaddleDetection (cioè `/255` anche quando `norm_type: none`).
//!
//! Le linee OCR restano la fonte di verità del testo: l'associazione non
//! scarta mai una linea, al massimo la mette nel blocco orphan.

use serde::{Deserialize, Serialize};

use crate::pipeline::types::{Block, BoundingBox, Line, Page, Paragraph};

// ─── Specifiche modello ───────────────────────────────────────────────────────

const IMAGENET_MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const IMAGENET_STD:  [f32; 3] = [0.229, 0.224, 0.225];

/// `label_list` di PP-DocLayout-S (23 classi), nell'ordine del suo
/// `inference.yml` — l'ordine È il mapping `class_id → label`.
pub const PP_DOCLAYOUT_S_LABELS: [&str; 23] = [
    "paragraph_title", "image", "text", "number", "abstract", "content",
    "figure_title", "formula", "table", "table_title", "reference",
    "doc_title", "footnote", "header", "algorithm", "footer", "seal",
    "chart_title", "chart", "formula_number", "header_image",
    "footer_image", "aside_text",
];

/// `label_list` di PP-DocLayoutV3 (25 classi). Il modello è stato rimosso
/// dal repo (ripristinabile con `fetch_models.py --extra`) ma la spec resta
/// per la variante "qualità".
pub const PP_DOCLAYOUT_V3_LABELS: [&str; 25] = [
    "abstract", "algorithm", "aside_text", "chart", "content",
    "display_formula", "doc_title", "figure_title", "footer",
    "footer_image", "footnote", "formula_number", "header", "header_image",
    "image", "inline_formula", "number", "paragraph_title", "reference",
    "reference_content", "seal", "table", "text", "vertical_text",
    "vision_footnote",
];

/// Parametri di un modello di layout, trascritti dal suo `inference.yml`.
/// Fonte di verità unica per preprocess e decodifica, sia nativa sia wasm.
#[derive(Debug, Clone, Serialize)]
pub struct LayoutModelSpec {
    pub id: &'static str,
    /// Input spaziale (stretch a `input_w × input_h`, `keep_ratio: false`).
    pub input_w: u32,
    pub input_h: u32,
    /// Interpolazione dichiarata dal yml (`interp: 2` = bicubica).
    pub interp: &'static str,
    /// Divide per 255 prima della normalizzazione (default PaddleDetection).
    pub is_scale: bool,
    /// `None` con `norm_type: none` (V3): solo `/255`, niente mean/std.
    pub mean: Option<[f32; 3]>,
    pub std:  Option<[f32; 3]>,
    /// Il modello ha il terzo input `im_shape` (V3 sì, S no).
    pub has_im_shape: bool,
    /// NMS già dentro il grafo (S sì, V3 no).
    pub in_graph_nms: bool,
    /// Il modello emette la colonna reading order (V3 sì: output `[N,7]`).
    pub emits_reading_order: bool,
    /// `class_id → label`, nell'ordine del `label_list` del yml.
    pub labels: &'static [&'static str],
    /// `draw_threshold` del yml: la soglia che il modello stesso
    /// raccomanda per l'uso dei box. **Da preferire a qualunque valore
    /// scelto a mano**: è tarata da chi ha addestrato la rete.
    pub draw_threshold: f32,
    /// `NMS.score_threshold` del yml. Per i modelli con NMS nel grafo è
    /// già stata applicata: nulla sotto questo valore arriva all'output,
    /// quindi filtrare più in basso non ha effetto.
    pub nms_score_threshold: f32,
}

impl LayoutModelSpec {
    /// PP-DocLayout-S — PicoDet-S, 480×480, `/255` + ImageNet, NMS nel grafo.
    pub fn pp_doclayout_s() -> Self {
        Self {
            id: "PP-DocLayout-S",
            input_w: 480, input_h: 480,
            interp: "bicubic",
            is_scale: true,
            mean: Some(IMAGENET_MEAN), std: Some(IMAGENET_STD),
            has_im_shape: false,
            in_graph_nms: true,
            emits_reading_order: false,
            labels: &PP_DOCLAYOUT_S_LABELS,
            draw_threshold: 0.5,
            nms_score_threshold: 0.3,
        }
    }

    /// PP-DocLayout-M — PicoDet-L, 640×640.
    ///
    /// Identico a S per architettura (GFL), preprocess e `label_list`
    /// (stesse 23 classi **nello stesso ordine**): cambia solo la
    /// risoluzione di input. Sui numeri ufficiali fa 75,2 mAP contro 70,9
    /// di S, a 22,4 MB contro 4,7.
    pub fn pp_doclayout_m() -> Self {
        Self {
            id: "PP-DocLayout-M",
            input_w: 640, input_h: 640,
            interp: "bicubic",
            is_scale: true,
            mean: Some(IMAGENET_MEAN), std: Some(IMAGENET_STD),
            has_im_shape: false,
            in_graph_nms: true,
            emits_reading_order: false,
            labels: &PP_DOCLAYOUT_S_LABELS, // stesso label_list, stesso ordine
            draw_threshold: 0.5,
            nms_score_threshold: 0.3,
        }
    }

    /// PP-DocLayoutV3 — RT-DETR-L, 800×800, solo `/255` (`norm_type: none`).
    pub fn pp_doclayout_v3() -> Self {
        Self {
            id: "PP-DocLayoutV3",
            input_w: 800, input_h: 800,
            interp: "bicubic",
            is_scale: true,
            mean: None, std: None,
            has_im_shape: true,
            in_graph_nms: false,
            emits_reading_order: true,
            labels: &PP_DOCLAYOUT_V3_LABELS,
            draw_threshold: 0.5,
            nms_score_threshold: 0.0, // NMS fuori dal grafo: nessun pre-filtro
        }
    }

    /// `scale_factor` da passare al modello. Con lo stretch è **per-asse**:
    /// `[input_h/h, input_w/w]` — due valori distinti, non uno ripetuto.
    /// Il modello lo usa internamente per riportare le bbox alle coordinate
    /// dell'immagine originale.
    pub fn scale_factor(&self, img_w: u32, img_h: u32) -> [f32; 2] {
        [
            self.input_h as f32 / img_h.max(1) as f32,
            self.input_w as f32 / img_w.max(1) as f32,
        ]
    }

    /// Label della classe, `"text"` come fallback per id fuori range
    /// (stesso comportamento di PaddleX: non si scarta il box).
    pub fn label_of(&self, class_id: usize) -> &'static str {
        self.labels.get(class_id).copied().unwrap_or("text")
    }
}

// ─── LayoutBox ────────────────────────────────────────────────────────────────

/// Classi che per definizione non contengono testo da riconoscere.
///
/// Sono le stesse nei due `label_list` (S/M e V3): `image` e le sue varianti
/// di intestazione e piè di pagina, i grafici, i timbri.
pub const NON_TEXT_LABELS: [&str; 5] =
    ["image", "header_image", "footer_image", "chart", "seal"];

/// Classi che contengono testo, usate come contro-prova: se un box
/// "immagine" è coperto da box di testo, il modello si contraddice.
pub const TEXT_LABELS: [&str; 12] = [
    "text", "paragraph_title", "doc_title", "abstract", "content",
    "footnote", "header", "footer", "reference", "aside_text", "number",
    "vertical_text",
];

/// Quando **non** passare una regione a Tesseract.
///
/// Il guadagno è doppio: non si legge garbage da una figura, e Tesseract non
/// ci costruisce sopra righe che poi attraversano la gronda. Ma l'errore è
/// **asimmetrico**: escludere una regione che *era* testo lo perde per
/// sempre, mentre includere una figura costa solo qualche parola a bassa
/// confidenza che il `NoiseFilter` scarta comunque. Da qui i tre freni.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OcrSkipOptions {
    /// Le classi da escludere. Default: [`NON_TEXT_LABELS`].
    pub labels: Vec<String>,
    /// Confidenza minima. Il default non è inventato: è il `draw_threshold`
    /// del `inference.yml` del modello, cioè la soglia che il modello stesso
    /// considera "abbastanza sicura da mostrare". È **più alta** dello 0,3 di
    /// `NMS.score_threshold` che usiamo per *includere* le regioni, e la
    /// differenza è voluta: per escludere serve più prova che per includere.
    pub min_score: f32,
    /// Frazione massima della pagina che una regione esclusa può coprire.
    ///
    /// **È il freno che rende un errore non catastrofico**, e serve per una
    /// ragione misurata: su `sample-page.png` i modelli S e M emettono un
    /// `image` e un `table` che coprono *tutta* la pagina (copertura 183%,
    /// `docs/layout-soglie.md`). Sbiancare quel box cancellerebbe il
    /// documento. E non si perde nulla a rinunciare: se una regione copre
    /// quasi tutta la pagina, o la pagina è davvero un'immagine — e allora
    /// l'OCR non troverebbe nulla comunque — o il modello ha sbagliato.
    pub max_area_ratio: f32,
    /// Frazione massima dell'area della regione che può essere coperta da
    /// box di classe testuale. Sopra questa, il modello si sta
    /// contraddicendo e si preferisce non toccare nulla.
    pub max_text_overlap: f32,
}

impl OcrSkipOptions {
    /// Default legati alla spec del modello, non a valori scelti a mano.
    pub fn for_spec(spec: &LayoutModelSpec) -> Self {
        Self {
            labels: NON_TEXT_LABELS.iter().map(|s| s.to_string()).collect(),
            min_score: spec.draw_threshold,
            max_area_ratio: 0.40,
            max_text_overlap: 0.20,
        }
    }
}

/// Classi che sono, per costruzione, **una riga sola**: titoli e
/// intestazioni. Sono le uniche su cui `--psm 7` ("tratta il crop come una
/// singola riga di testo") ha senso.
pub const HEADING_LABELS: [&str; 6] = [
    "doc_title", "paragraph_title", "header", "figure_title", "chart_title",
    "table_title",
];

/// Quando ri-riconoscere un titolo isolato (②c).
///
/// Serve per un difetto misurato: sul titolo `A N N O T A Z I O N I
/// D'UFFICIO`, spaziato lettera per lettera, Tesseract a piena pagina perde
/// la nozione di parola e produce `ANN 0: /T1AIZ NON PD' U5FIF:CiH0` con
/// confidenze fra 0 e 29. Ritagliando la sola riga e usando `--psm 7` legge
/// `ANNOTAZIONI D'UFFICIO` esatto.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadingOptions {
    /// Classi candidate. Default: [`HEADING_LABELS`].
    pub labels: Vec<String>,
    /// Confidenza minima del box di layout: il `draw_threshold` del modello.
    pub min_score: f32,
    /// Righe massime nel box. **È il vincolo che rende `psm 7` legittimo**:
    /// quel modo assume una riga sola, e applicarlo a un blocco di più righe
    /// le fonderebbe. Si contano le righe della prima passata, non si stima.
    pub max_lines: usize,
    /// Altezza massima del box in frazione di pagina: un `header` che copre
    /// mezza pagina non è un titolo, è un errore del modello.
    pub max_height_ratio: f32,
    /// Ingrandimento del crop prima di ri-riconoscerlo.
    ///
    /// **Valore empirico, e non monotono**: misurato su quel titolo, 1× dà
    /// `A N N 0 /TiA:Z lO;:N1 D'UFFICIO`, 2× migliora, **3× è esatto**, 4×
    /// torna a sbagliare. Un solo campione non basta a derivarne una regola
    /// (la formulazione giusta sarebbe un'altezza di testo obiettivo), quindi
    /// resta un parametro. Sbagliarlo non fa danno: ②c tiene il risultato
    /// solo se la confidenza migliora.
    pub upscale: f32,
    /// Page segmentation mode per il ri-riconoscimento.
    pub psm: u32,
}

impl HeadingOptions {
    pub fn for_spec(spec: &LayoutModelSpec) -> Self {
        Self {
            labels: HEADING_LABELS.iter().map(|s| s.to_string()).collect(),
            min_score: spec.draw_threshold,
            max_lines: 1,
            max_height_ratio: 0.10,
            upscale: 3.0,
            psm: 7,
        }
    }
}

/// Esito di ②c, con il motivo di ogni scarto.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HeadingPlan {
    /// Regioni da ri-riconoscere come riga singola.
    pub regions: Vec<LayoutBox>,
    /// Candidate rifiutate: `(label, score, motivo)`.
    pub rejected: Vec<(String, f32, String)>,
}

/// Seleziona i titoli da ri-riconoscere isolati (②c).
///
/// `line_boxes` sono le righe della **prima passata**: servono a contare
/// quante righe cadono nel box, che è la condizione di legittimità di
/// `psm 7`. Un box senza righe dentro non si tocca — non c'è niente da
/// migliorare e il crop conterrebbe solo grafica.
pub fn heading_regions(boxes: &[LayoutBox], line_boxes: &[BoundingBox],
                       _page_w: u32, page_h: u32,
                       opts: &HeadingOptions) -> HeadingPlan {
    let mut plan = HeadingPlan::default();
    for b in boxes {
        if !opts.labels.iter().any(|l| l == &b.label) {
            continue;
        }
        if b.score < opts.min_score {
            plan.rejected.push((b.label.clone(), b.score,
                format!("confidenza sotto {:.2}", opts.min_score)));
            continue;
        }
        let hgt = (b.bbox.bottom - b.bbox.top).max(0) as f32;
        let ratio = hgt / (page_h.max(1) as f32);
        if ratio > opts.max_height_ratio {
            plan.rejected.push((b.label.clone(), b.score,
                format!("alto il {:.0}% della pagina (limite {:.0}%)",
                        ratio * 100.0, opts.max_height_ratio * 100.0)));
            continue;
        }
        let inside = line_boxes.iter().filter(|lb| {
            let cx = (lb.left + lb.right) as f32 / 2.0;
            let cy = (lb.top + lb.bottom) as f32 / 2.0;
            b.contains(cx, cy)
        }).count();
        if inside == 0 {
            plan.rejected.push((b.label.clone(), b.score,
                "nessuna riga dentro: niente da migliorare".into()));
            continue;
        }
        if inside > opts.max_lines {
            plan.rejected.push((b.label.clone(), b.score,
                format!("{inside} righe dentro (psm {} vale per una sola)", opts.psm)));
            continue;
        }
        plan.regions.push(b.clone());
    }
    plan
}

/// Esito della decisione, con il **motivo** di ogni scarto: una regione
/// esclusa dall'OCR è un intervento distruttivo e non deve essere silenzioso.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OcrSkipPlan {
    /// Regioni da non passare a Tesseract.
    pub skip: Vec<LayoutBox>,
    /// Candidate rifiutate: `(label, score, motivo)`.
    pub rejected: Vec<(String, f32, String)>,
}

/// Area dell'intersezione fra due bbox, in pixel.
fn intersect_area(a: &BoundingBox, b: &BoundingBox) -> f32 {
    let x = (a.right.min(b.right) - a.left.max(b.left)).max(0) as f32;
    let y = (a.bottom.min(b.bottom) - a.top.max(b.top)).max(0) as f32;
    x * y
}

/// Decide quali regioni di layout non passare a Tesseract.
///
/// Va chiamata **dopo ③ e prima di ②**: nella pipeline il layout precede il
/// riconoscimento, quindi le regioni si possono sbiancare sul bitmap invece
/// di filtrare le parole dopo. È meglio, perché così l'analisi di pagina di
/// Tesseract non vede affatto la figura e non ci raggruppa righe sopra.
pub fn ocr_skip_regions(boxes: &[LayoutBox], page_w: u32, page_h: u32,
                        opts: &OcrSkipOptions) -> OcrSkipPlan {
    let mut plan = OcrSkipPlan::default();
    let page_area = (page_w as f32 * page_h as f32).max(1.0);

    for b in boxes {
        if !opts.labels.iter().any(|l| l == &b.label) {
            continue;
        }
        if b.score < opts.min_score {
            plan.rejected.push((b.label.clone(), b.score,
                format!("confidenza sotto {:.2}", opts.min_score)));
            continue;
        }
        let area = ((b.bbox.right - b.bbox.left).max(0) as f32)
                 * ((b.bbox.bottom - b.bbox.top).max(0) as f32);
        let ratio = area / page_area;
        if ratio > opts.max_area_ratio {
            plan.rejected.push((b.label.clone(), b.score,
                format!("copre il {:.0}% della pagina (limite {:.0}%)",
                        ratio * 100.0, opts.max_area_ratio * 100.0)));
            continue;
        }
        // Contro-prova: quanta parte di questa "immagine" il modello la
        // chiama anche testo?
        let text_over: f32 = boxes.iter()
            .filter(|o| TEXT_LABELS.iter().any(|l| l == &o.label))
            .map(|o| intersect_area(&b.bbox, &o.bbox))
            .sum();
        let overlap = if area > 0.0 { text_over / area } else { 0.0 };
        if overlap > opts.max_text_overlap {
            plan.rejected.push((b.label.clone(), b.score,
                format!("il {:.0}% è coperto da box di testo (limite {:.0}%)",
                        overlap * 100.0, opts.max_text_overlap * 100.0)));
            continue;
        }
        plan.skip.push(b.clone());
    }
    plan
}

#[cfg(test)]
mod ocr_skip_tests {
    use super::*;

    fn lb(label: &str, score: f32, l: i32, t: i32, r: i32, b: i32) -> LayoutBox {
        LayoutBox {
            bbox: BoundingBox { left: l, top: t, right: r, bottom: b },
            class_id: 0,
            label: label.to_string(),
            score,
            reading_order: -1,
        }
    }

    fn opts() -> OcrSkipOptions {
        OcrSkipOptions::for_spec(&LayoutModelSpec::pp_doclayout_v3())
    }

    #[test]
    fn skips_a_confident_logo() {
        // Logo in intestazione: 200×100 su una pagina 1000×1000 = 2%.
        let boxes = vec![
            lb("header_image", 0.91, 50, 20, 250, 120),
            lb("text", 0.88, 50, 200, 950, 800),
        ];
        let plan = ocr_skip_regions(&boxes, 1000, 1000, &opts());
        assert_eq!(plan.skip.len(), 1);
        assert_eq!(plan.skip[0].label, "header_image");
        assert!(plan.rejected.is_empty());
    }

    #[test]
    fn keeps_a_low_confidence_image() {
        let boxes = vec![lb("image", 0.31, 50, 20, 250, 120)];
        let plan = ocr_skip_regions(&boxes, 1000, 1000, &opts());
        assert!(plan.skip.is_empty(), "sotto draw_threshold non si esclude");
        assert_eq!(plan.rejected.len(), 1);
        assert!(plan.rejected[0].2.contains("confidenza"));
    }

    /// Il caso misurato su `sample-page.png`: S e M emettono un `image` che
    /// copre tutta la pagina. Sbiancarlo cancellerebbe il documento.
    #[test]
    fn never_skips_a_full_page_image() {
        let boxes = vec![lb("image", 0.99, 0, 0, 1000, 1000)];
        let plan = ocr_skip_regions(&boxes, 1000, 1000, &opts());
        assert!(plan.skip.is_empty(), "una regione che copre la pagina non si esclude mai");
        assert!(plan.rejected[0].2.contains("100%"));
    }

    /// Se il modello chiama la stessa area sia immagine sia testo, si
    /// contraddice: non si tocca nulla.
    #[test]
    fn keeps_an_image_that_overlaps_text() {
        let boxes = vec![
            lb("image", 0.95, 100, 100, 300, 300),
            lb("text", 0.90, 120, 120, 280, 280),   // 160×160 su 200×200 = 64%
        ];
        let plan = ocr_skip_regions(&boxes, 1000, 1000, &opts());
        assert!(plan.skip.is_empty());
        assert!(plan.rejected[0].2.contains("coperto da box di testo"));
    }

    #[test]
    fn labels_are_configurable_and_default_to_non_text() {
        let boxes = vec![
            lb("table", 0.99, 100, 100, 300, 300),
            lb("seal", 0.99, 400, 400, 500, 500),
        ];
        // `table` non è fra le classi non testuali: contiene testo.
        let plan = ocr_skip_regions(&boxes, 1000, 1000, &opts());
        assert_eq!(plan.skip.len(), 1);
        assert_eq!(plan.skip[0].label, "seal");

        // Ma il parametro esiste: chi vuole escludere le tabelle può.
        let mut o = opts();
        o.labels = vec!["table".into()];
        let plan = ocr_skip_regions(&boxes, 1000, 1000, &o);
        assert_eq!(plan.skip.len(), 1);
        assert_eq!(plan.skip[0].label, "table");
    }

    fn bb(l: i32, t: i32, r: i32, b: i32) -> BoundingBox {
        BoundingBox { left: l, top: t, right: r, bottom: b }
    }

    fn hopts() -> HeadingOptions {
        HeadingOptions::for_spec(&LayoutModelSpec::pp_doclayout_v3())
    }

    /// Il caso reale: `#18 header 0.55` sopra `ANNOTAZIONI D'UFFICIO`, con
    /// una sola riga dentro. Pagina 1732×2482, titolo y=149..204.
    #[test]
    fn picks_the_single_line_heading() {
        let boxes = vec![lb("header", 0.55, 430, 140, 1330, 210)];
        let lines = vec![bb(439, 149, 1323, 204)];
        let plan = heading_regions(&boxes, &lines, 1732, 2482, &hopts());
        assert_eq!(plan.regions.len(), 1);
        assert!(plan.rejected.is_empty());
    }

    /// `psm 7` assume UNA riga: su un blocco di più righe le fonderebbe.
    #[test]
    fn refuses_a_multi_line_block() {
        let boxes = vec![lb("paragraph_title", 0.90, 100, 100, 900, 300)];
        let lines = vec![bb(110, 110, 880, 150), bb(110, 200, 880, 240)];
        let plan = heading_regions(&boxes, &lines, 1732, 2482, &hopts());
        assert!(plan.regions.is_empty());
        assert!(plan.rejected[0].2.contains("2 righe dentro"));
    }

    #[test]
    fn refuses_an_empty_or_oversized_heading() {
        // Nessuna riga dentro: solo grafica, niente da migliorare.
        let boxes = vec![lb("header", 0.90, 100, 100, 900, 160)];
        let plan = heading_regions(&boxes, &[], 1732, 2482, &hopts());
        assert!(plan.regions.is_empty());
        assert!(plan.rejected[0].2.contains("nessuna riga"));

        // Un "header" alto mezza pagina è un errore del modello.
        let boxes = vec![lb("header", 0.90, 100, 100, 900, 1400)];
        let lines = vec![bb(110, 110, 880, 150)];
        let plan = heading_regions(&boxes, &lines, 1732, 2482, &hopts());
        assert!(plan.regions.is_empty());
        assert!(plan.rejected[0].2.contains("alto il"));
    }

    #[test]
    fn heading_defaults_are_tied_to_the_spec() {
        for spec in [LayoutModelSpec::pp_doclayout_s(),
                     LayoutModelSpec::pp_doclayout_v3()] {
            let o = HeadingOptions::for_spec(&spec);
            assert_eq!(o.min_score, spec.draw_threshold);
            assert_eq!(o.max_lines, 1, "psm 7 vale solo per una riga");
            assert_eq!(o.psm, 7);
        }
    }

    /// Il caso osservato: una riga sola spezzata da uno spazio largo fra
    /// parole non è una pagina a due colonne.
    #[test]
    fn one_line_split_by_a_wide_gap_is_not_two_columns() {
        // «Esaminata dalla» | «Commissione edilizia nella seduta del»
        let lines = vec![bb(115, 280, 280, 320), bb(380, 280, 990, 320)];
        assert!(split_columns(&lines).is_empty(),
                "due frammenti sulla stessa riga non sono colonne");
    }

    /// Il titolo spaziato lettera per lettera: ogni lettera è un box, e ogni
    /// spazio un corridoio. Deve restare una pagina a colonna singola.
    #[test]
    fn letter_spaced_heading_is_not_many_columns() {
        let lines: Vec<BoundingBox> =
            (0..10).map(|i| bb(440 + i * 60, 160, 470 + i * 60, 210)).collect();
        assert!(split_columns(&lines).is_empty());
    }

    /// Il caso che ②b deve continuare a servire: due colonne vere, ognuna
    /// con la propria pila di righe. Qui la divisione va fatta.
    #[test]
    fn real_two_columns_are_still_detected() {
        let mut lines = Vec::new();
        for k in 0..6 {
            lines.push(bb(100, 200 + k * 60, 900, 240 + k * 60));   // colonna sx
            lines.push(bb(1100, 200 + k * 60, 1900, 240 + k * 60)); // colonna dx
        }
        let bands = split_columns(&lines);
        assert_eq!(bands.len(), 2, "due colonne vere vanno divise");
        assert_eq!(bands[0].lines.len(), 6);
        assert_eq!(bands[1].lines.len(), 6);
        assert!(bands[0].right < bands[1].left);
    }

    #[test]
    fn default_threshold_comes_from_the_model_spec() {
        for spec in [LayoutModelSpec::pp_doclayout_s(),
                     LayoutModelSpec::pp_doclayout_m(),
                     LayoutModelSpec::pp_doclayout_v3()] {
            let o = OcrSkipOptions::for_spec(&spec);
            assert_eq!(o.min_score, spec.draw_threshold);
            // Per escludere serve più prova che per includere.
            assert!(o.min_score > spec.nms_score_threshold
                    || spec.nms_score_threshold == 0.0);
        }
    }
}

/// Regione di layout in pixel sull'immagine di input (il modello rimappa
/// internamente via `scale_factor`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutBox {
    pub bbox: BoundingBox,
    pub class_id: u32,
    /// Label dal `label_list` del modello (es. `"text"`, `"doc_title"`).
    pub label: String,
    pub score: f32,
    /// Reading order: dal modello se lo emette, altrimenti assegnato da
    /// [`ensure_reading_order`] via XY-Cut. `-1` = non assegnato.
    pub reading_order: i32,
}

impl LayoutBox {
    fn center(&self) -> (f32, f32) {
        (
            (self.bbox.left + self.bbox.right) as f32 / 2.0,
            (self.bbox.top + self.bbox.bottom) as f32 / 2.0,
        )
    }
    /// Inclusivo su left/top, esclusivo su right/bottom (come il nativo).
    fn contains(&self, px: f32, py: f32) -> bool {
        px >= self.bbox.left as f32 && px < self.bbox.right as f32
            && py >= self.bbox.top as f32 && py < self.bbox.bottom as f32
    }
    fn distance_to(&self, px: f32, py: f32) -> f32 {
        let (cx, cy) = self.center();
        ((cx - px).powi(2) + (cy - py).powi(2)).sqrt()
    }
}

/// Mapping label → categoria semantica semplificata (8 classi), coerente
/// col `CLASS_MAPPING` PaddleX e con `LayoutClass::semantic` nativo.
/// Copre i label di entrambi i modelli ("formula" è di S, "display_formula"
/// e "inline_formula" di V3); tutto il resto → `"text"`.
pub fn semantic_of(label: &str) -> &'static str {
    match label {
        "doc_title" | "paragraph_title" => "title",
        "header" => "header",
        "footer" => "footer",
        "reference" => "list",
        "chart" | "footer_image" | "header_image" | "image" | "seal" => "figure",
        "table" => "table",
        "display_formula" | "inline_formula" | "formula" => "equation",
        _ => "text",
    }
}

// ─── Decodifica output modello ────────────────────────────────────────────────

/// Decodifica l'output raw `[N, cols]` del modello:
/// `[class_id, score, xmin, ymin, xmax, ymax, (reading_order)]` per riga.
/// Filtra per confidence e `class_id >= 0` (le righe di padding del NMS
/// in-graph hanno class -1), clampa dentro l'immagine.
pub fn decode_layout(
    raw: &[f32],
    cols: usize,
    labels: &[&'static str],
    score_thresh: f32,
    img_w: u32,
    img_h: u32,
) -> Vec<LayoutBox> {
    if cols < 6 {
        return Vec::new();
    }
    let (img_w, img_h) = (img_w as i32, img_h as i32);
    let mut out = Vec::new();
    for row in raw.chunks_exact(cols) {
        let class_id = row[0] as i32;
        let score = row[1];
        if class_id < 0 || score < score_thresh {
            continue;
        }
        let l = (row[2].max(0.0) as i32).min(img_w);
        let t = (row[3].max(0.0) as i32).min(img_h);
        let r = (row[4].max(0.0) as i32).min(img_w);
        let b = (row[5].max(0.0) as i32).min(img_h);
        if r <= l || b <= t {
            continue;
        }
        let reading_order = if cols >= 7 { row[6] as i32 } else { -1 };
        out.push(LayoutBox {
            bbox: BoundingBox::from_lrtb(l, t, r, b),
            class_id: class_id as u32,
            label: labels.get(class_id as usize).copied().unwrap_or("text").to_string(),
            score,
            reading_order,
        });
    }
    out
}

fn iou(a: &BoundingBox, b: &BoundingBox) -> f32 {
    let w = (a.right.min(b.right) - a.left.max(b.left)).max(0);
    let h = (a.bottom.min(b.bottom) - a.top.max(b.top)).max(0);
    let inter = (w * h) as f32;
    let union = (a.width() * a.height() + b.width() * b.height()) as f32 - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

/// Non-maximum suppression O(N²): tiene il box con score più alto, scarta
/// gli overlap con IoU > soglia. Serve solo ai modelli senza NMS nel grafo
/// (V3); per S è già fatta dal modello.
pub fn nms(mut boxes: Vec<LayoutBox>, iou_thresh: f32) -> Vec<LayoutBox> {
    boxes.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    let mut keep = vec![true; boxes.len()];
    for i in 0..boxes.len() {
        if !keep[i] { continue; }
        for j in (i + 1)..boxes.len() {
            if keep[j] && iou(&boxes[i].bbox, &boxes[j].bbox) > iou_thresh {
                keep[j] = false;
            }
        }
    }
    boxes.into_iter().zip(keep).filter_map(|(b, k)| k.then_some(b)).collect()
}

// ─── Reading order (XY-Cut) ───────────────────────────────────────────────────

/// Ordina i rettangoli in reading order con XY-Cut ricorsivo. Ritorna gli
/// indici in `rects` nell'ordine di lettura.
///
/// 1. Cerca un taglio orizzontale (y-gap che nessun box attraversa): il
///    gruppo sopra si legge prima di quello sotto.
/// 2. Altrimenti un taglio verticale: colonna sinistra prima della destra.
/// 3. Altrimenti fallback a ordinamento per (y_centro, x_centro).
pub fn xy_cut_order(rects: &[BoundingBox]) -> Vec<usize> {
    let indices: Vec<usize> = (0..rects.len()).collect();
    let mut out = Vec::with_capacity(rects.len());
    xy_cut_rec(rects, &indices, &mut out);
    out
}

#[derive(Clone, Copy)]
enum Axis { X, Y }

/// Sceglie il taglio migliore fra i due assi: vince il **corridoio piu'
/// largo**.
///
/// Provare sempre prima la Y (come fa l'XY-Cut ingenuo) sbaglia sui
/// documenti a colonne: fra le righe di due colonne affiancate esiste
/// quasi sempre anche un gap orizzontale, quindi il taglio Y scatta per
/// primo e produce bande di righe — interlacciando le colonne. Il
/// corridoio fra colonne e' tipicamente piu' largo dell'interlinea, quindi
/// confrontare l'ampiezza dei gap risolve senza soglie arbitrarie.
fn best_cut(rects: &[BoundingBox], indices: &[usize]) -> Option<(Vec<usize>, Vec<usize>)> {
    let y = find_cut(rects, indices, Axis::Y);
    let x = find_cut(rects, indices, Axis::X);
    match (y, x) {
        (Some((gy, ay, by)), Some((gx, ax, bx))) => {
            if gx > gy { Some((ax, bx)) } else { Some((ay, by)) }
        }
        (Some((_, a, b)), None) | (None, Some((_, a, b))) => Some((a, b)),
        (None, None) => None,
    }
}

fn xy_cut_rec(rects: &[BoundingBox], indices: &[usize], out: &mut Vec<usize>) {
    match indices.len() {
        0 => {}
        1 => out.push(indices[0]),
        _ => {
            if let Some((first, second)) = best_cut(rects, indices) {
                xy_cut_rec(rects, &first, out);
                xy_cut_rec(rects, &second, out);
            } else {
                let mut sorted = indices.to_vec();
                sorted.sort_by(|&a, &b| {
                    let ka = (rects[a].top + rects[a].bottom, rects[a].left + rects[a].right);
                    let kb = (rects[b].top + rects[b].bottom, rects[b].left + rects[b].right);
                    ka.cmp(&kb)
                });
                out.extend(sorted);
            }
        }
    }
}

/// Taglio sull'asse dato: valido solo se nessun box attraversa la linea.
/// Ritorna `(ampiezza_del_corridoio, prima_partizione, seconda)`.
fn find_cut(rects: &[BoundingBox], indices: &[usize], axis: Axis)
    -> Option<(i32, Vec<usize>, Vec<usize>)>
{
    let span = |i: usize| match axis {
        Axis::Y => (rects[i].top, rects[i].bottom),
        Axis::X => (rects[i].left, rects[i].right),
    };

    // Sweep-line: (valore, is_start); end prima di start a parità di valore
    // (box che finisce a 100 + box che inizia a 100 → nessun gap).
    let mut events: Vec<(i32, bool)> = Vec::with_capacity(indices.len() * 2);
    for &i in indices {
        let (s, e) = span(i);
        events.push((s, true));
        events.push((e, false));
    }
    events.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    // Si cerca il corridoio **più largo**, non il primo.
    //
    // Prendere il primo sembra equivalente ma non lo è: su un documento
    // con blocchi affiancati sopra e una tabella a tutta pagina sotto, il
    // primo gap orizzontale è quello fra le prime due righe, e la
    // ricorsione affetta la pagina in bande sottili — interlacciando la
    // colonna sinistra con il riquadro destro riga per riga
    // (`Cap. Accordato ... VIA ROTABILE 74`). Col gap più largo, il primo
    // taglio cade invece nello stacco strutturale fra intestazione e
    // corpo, e dentro l'intestazione vince poi la gronda verticale: si
    // legge tutta la colonna sinistra e poi tutto il riquadro destro.
    let mut active = 0i32;
    let mut pending_end: Option<i32> = None;
    let mut best: Option<(i32, i32, i32)> = None; // (ampiezza, fine, inizio successivo)
    for &(v, is_start) in &events {
        if is_start {
            if active == 0 {
                if let Some(end) = pending_end {
                    let gap = v - end;
                    if gap > 0 && best.map_or(true, |(g, _, _)| gap > g) {
                        best = Some((gap, end, v));
                    }
                }
            }
            active += 1;
        } else {
            active -= 1;
            if active == 0 {
                pending_end = Some(v);
            }
        }
    }
    let (gap, cut, next_start) = best?;

    let first: Vec<usize> = indices.iter().copied().filter(|&i| span(i).1 <= cut).collect();
    let second: Vec<usize> = indices.iter().copied().filter(|&i| span(i).0 >= next_start).collect();

    (first.len() + second.len() == indices.len()).then_some((gap, first, second))
}

/// Se nessun box ha un reading order dal modello (caso PP-DocLayout-S),
/// lo assegna via [`xy_cut_order`]. Se anche un solo box ce l'ha, non
/// tocca nulla (l'ordine appreso vince sull'euristica).
pub fn ensure_reading_order(boxes: &mut [LayoutBox]) {
    if boxes.is_empty() || boxes.iter().any(|b| b.reading_order >= 0) {
        return;
    }
    let rects: Vec<BoundingBox> = boxes.iter().map(|b| b.bbox).collect();
    for (pos, idx) in xy_cut_order(&rects).into_iter().enumerate() {
        boxes[idx].reading_order = pos as i32;
    }
}

/// Sort per reading order ascending, `-1` in fondo.
pub fn sort_by_reading_order(boxes: &mut [LayoutBox]) {
    boxes.sort_by_key(|b| if b.reading_order < 0 { i32::MAX } else { b.reading_order });
}

/// Postprocess completo dell'output raw: decodifica → NMS (solo se non è
/// già nel grafo) → reading order (dal modello o XY-Cut) → sort.
/// È il percorso unico usato da `analyze` nativo e da `decode_layout` wasm.
pub fn postprocess_layout(
    raw: &[f32],
    cols: usize,
    spec: &LayoutModelSpec,
    score_thresh: f32,
    nms_iou: f32,
    img_w: u32,
    img_h: u32,
) -> Vec<LayoutBox> {
    let mut boxes = decode_layout(raw, cols, spec.labels, score_thresh, img_w, img_h);
    if !spec.in_graph_nms {
        boxes = nms(boxes, nms_iou);
    }
    ensure_reading_order(&mut boxes);
    sort_by_reading_order(&mut boxes);
    boxes
}

// ─── Associazione linee ↔ layout-box ─────────────────────────────────────────

/// Risultato di [`associate_lines`]. Indici riferiti agli slice di input.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Association {
    /// Per ogni linea: indice del layout-box assegnato. `None` solo se
    /// `boxes` è vuoto (nessun layout → tutto orphan).
    pub line_to_box: Vec<Option<usize>>,
    /// `0.0` se il centroide della linea è dentro il box; `> 0.0` se
    /// assegnata via orphan-recovery (distanza dal centro del box più
    /// vicino); `INFINITY` se non c'erano box.
    pub distance: Vec<f32>,
    /// Permutazione delle linee in reading order: primario il reading
    /// order del box assegnato (`-1` in fondo), secondario y del centroide,
    /// terziario x.
    pub order: Vec<usize>,
}

/// Associazione generica linee ↔ layout-box: containment del centroide,
/// fallback nearest-neighbor (orphan recovery). Nessuna linea viene mai
/// scartata. Le linee possono venire da qualunque engine (tesseract.js,
/// PP-OCR, …): serve solo il bbox.
pub fn associate_lines(line_boxes: &[BoundingBox], boxes: &[LayoutBox]) -> Association {
    let centroids: Vec<(f32, f32)> = line_boxes.iter()
        .map(|b| ((b.left + b.right) as f32 / 2.0, (b.top + b.bottom) as f32 / 2.0))
        .collect();

    let mut line_to_box = Vec::with_capacity(line_boxes.len());
    let mut distance = Vec::with_capacity(line_boxes.len());
    for &(cx, cy) in &centroids {
        if let Some(i) = boxes.iter().position(|lb| lb.contains(cx, cy)) {
            line_to_box.push(Some(i));
            distance.push(0.0);
        } else {
            // Orphan recovery: box più vicino per distanza dal centro.
            let nearest = boxes.iter().enumerate()
                .map(|(i, lb)| (i, lb.distance_to(cx, cy)))
                .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
            match nearest {
                Some((i, d)) => { line_to_box.push(Some(i)); distance.push(d); }
                None         => { line_to_box.push(None);    distance.push(f32::INFINITY); }
            }
        }
    }

    // Ordine globale: prima il reading order del box assegnato, poi —
    // DENTRO ogni box — XY-Cut sulle righe stesse.
    //
    // Il secondo passaggio non e' un dettaglio: quando il modello di layout
    // emette un box unico su tutta la pagina (caso frequente con
    // PP-DocLayout-S sulle scansioni), un ordinamento y-poi-x
    // **interlaccia le colonne** riga per riga. XY-Cut trova il corridoio
    // verticale e legge prima la colonna sinistra, poi la destra. Su
    // pagina a colonna singola degrada naturalmente all'ordine per y.
    let mut groups: std::collections::BTreeMap<(i32, usize), Vec<usize>> = Default::default();
    for i in 0..line_boxes.len() {
        let key = match line_to_box[i] {
            Some(bi) => {
                let ro = boxes[bi].reading_order;
                (if ro >= 0 { ro } else { i32::MAX }, bi)
            }
            None => (i32::MAX, usize::MAX),
        };
        groups.entry(key).or_default().push(i);
    }

    let mut order = Vec::with_capacity(line_boxes.len());
    for members in groups.values() {
        order.extend(order_lines_xy_cut(members, line_boxes));
    }

    Association { line_to_box, distance, order }
}

/// Ordina un gruppo di righe in reading order con XY-Cut sui loro bbox.
/// Ritorna gli indici originali (quelli in `members`), non le posizioni.
pub fn order_lines_xy_cut(members: &[usize], line_boxes: &[BoundingBox]) -> Vec<usize> {
    if members.len() <= 1 {
        return members.to_vec();
    }
    let rects: Vec<BoundingBox> = members.iter().map(|&i| line_boxes[i]).collect();
    xy_cut_order(&rects).into_iter().map(|k| members[k]).collect()
}

/// Banda verticale di una colonna, in pixel sull'immagine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnBand {
    pub left: i32,
    pub right: i32,
    /// Righe che cadono in questa banda (indici in `line_boxes`).
    pub lines: Vec<usize>,
}

/// Frazione della larghezza pagina oltre la quale una riga è considerata
/// "a tutta pagina" (titoli, intestazioni) e quindi esclusa dal calcolo
/// delle colonne: attraversando la gronda impedirebbe qualunque taglio
/// verticale.
const FULL_WIDTH_RATIO: f32 = 0.7;

/// Righe minime perché una banda sia una colonna e non un frammento di riga.
/// Una colonna è per definizione una **pila** di righe.
const MIN_LINES_PER_BAND: usize = 2;

/// Larghezza minima di una colonna, in frazione di pagina. A 0,15 restano
/// ammesse fino a ~6 colonne: molto oltre qualunque documento reale, e
/// abbastanza per scartare i frammenti di riga, che stanno sotto il 10%.
const MIN_BAND_WIDTH_RATIO: f32 = 0.15;

/// Partiziona le righe in **bande verticali** (colonne), da sinistra a
/// destra. Ritorna `vec![]` se la pagina è a colonna singola.
///
/// Serve al caso dei documenti bilingui affiancati: ogni banda può essere
/// ri-riconosciuta con la propria lingua. Le righe a tutta pagina sono
/// escluse dalle bande (restano al chiamante) perché appartengono alla
/// pagina, non a una colonna.
pub fn split_columns(line_boxes: &[BoundingBox]) -> Vec<ColumnBand> {
    if line_boxes.len() < 2 {
        return Vec::new();
    }
    let page_left = line_boxes.iter().map(|b| b.left).min().unwrap_or(0);
    let page_right = line_boxes.iter().map(|b| b.right).max().unwrap_or(0);
    let page_w = (page_right - page_left).max(1) as f32;

    let narrow: Vec<usize> = (0..line_boxes.len())
        .filter(|&i| (line_boxes[i].width() as f32) < FULL_WIDTH_RATIO * page_w)
        .collect();
    if narrow.len() < 2 {
        return Vec::new();
    }

    // Solo tagli verticali, ricorsivamente: le bande escono già ordinate
    // da sinistra a destra.
    fn rec(rects: &[BoundingBox], idx: &[usize], out: &mut Vec<Vec<usize>>) {
        match find_cut(rects, idx, Axis::X) {
            Some((_, a, b)) => { rec(rects, &a, out); rec(rects, &b, out); }
            None => out.push(idx.to_vec()),
        }
    }
    let mut groups = Vec::new();
    rec(line_boxes, &narrow, &mut groups);
    if groups.len() < 2 {
        return Vec::new();
    }

    // Freni di plausibilità. Senza questi, `rec` taglia a OGNI corridoio: su
    // una pagina a colonna singola ma sparsa — un titolo spaziato lettera per
    // lettera, righe puntinate, pochi blocchi corti — ogni spazio fra parole
    // diventa una "colonna". Osservato su un modulo edilizio: 8 bande su una
    // pagina che di colonne ne ha una, con
    // «Esaminata dalla | Commissione edilizia nella seduta del» spezzato in
    // due bande perché lo spazio fra le parole era largo.
    //
    // I due criteri vengono dalla definizione di colonna, non da tuning:
    //
    // - una colonna è una **pila** di righe; una banda con una riga sola è un
    //   frammento di riga separato da uno spazio largo;
    // - una colonna di testo non può essere più stretta di una frazione
    //   sensata della pagina. A 0,15 restano ammesse fino a ~6 colonne, molto
    //   oltre qualunque documento reale.
    //
    // Se **una** banda non li passa, si rinuncia all'intera divisione invece
    // di tenerne una parte: una divisione parziale taglierebbe comunque
    // dentro il contenuto, ed è esattamente il danno che si vuole evitare.
    let plausible = groups.iter().all(|lines| {
        if lines.len() < MIN_LINES_PER_BAND {
            return false;
        }
        let l = lines.iter().map(|&i| line_boxes[i].left).min().unwrap_or(0);
        let r = lines.iter().map(|&i| line_boxes[i].right).max().unwrap_or(0);
        ((r - l) as f32) >= MIN_BAND_WIDTH_RATIO * page_w
    });
    if !plausible {
        return Vec::new();
    }

    groups.into_iter().map(|lines| ColumnBand {
        left:  lines.iter().map(|&i| line_boxes[i].left).min().unwrap_or(0),
        right: lines.iter().map(|&i| line_boxes[i].right).max().unwrap_or(0),
        lines,
    }).collect()
}

/// Numero di colonne rilevate fra le righe date: quante partizioni produce
/// un taglio verticale al primo livello. `1` = pagina a colonna singola.
/// Diagnostica per capire se il documento e' multi-colonna prima ancora di
/// guardare il layout.
pub fn detect_columns(line_boxes: &[BoundingBox]) -> usize {
    fn count(rects: &[BoundingBox], idx: &[usize]) -> usize {
        if idx.len() <= 1 {
            return 1;
        }
        let y = find_cut(rects, idx, Axis::Y);
        let x = find_cut(rects, idx, Axis::X);
        // Stessa scelta di `best_cut`: vince il corridoio piu' largo.
        let vertical = match (&y, &x) {
            (Some((gy, ..)), Some((gx, ..))) => gx > gy,
            (None, Some(_)) => true,
            _ => false,
        };
        if vertical {
            let (_, a, b) = x.unwrap();
            // Un taglio verticale separa colonne: si sommano.
            count(rects, &a) + count(rects, &b)
        } else if let Some((_, a, b)) = y {
            // Uno orizzontale no: si prende il massimo dei due blocchi.
            count(rects, &a).max(count(rects, &b))
        } else {
            1
        }
    }
    let idx: Vec<usize> = (0..line_boxes.len()).collect();
    count(line_boxes, &idx)
}

/// Costruisce la [`Page`] finale: un [`Block`] per ogni layout-box
/// (nell'ordine dato, che è il reading order), con le linee assegnate
/// dentro; le linee senza box finiscono in un Block orphan in fondo.
/// I box senza linee restano come Block vuoti: portano la classe semantica
/// delle regioni non testuali (figure, tabelle).
pub fn build_page(
    lines: Vec<Line>,
    boxes: &[LayoutBox],
    page_number: u32,
    width_px: u32,
    height_px: u32,
    page_angle: u32,
) -> Page {
    let line_bboxes: Vec<BoundingBox> = lines.iter().map(|l| l.bbox).collect();
    let assoc = associate_lines(&line_bboxes, boxes);

    // Raggruppa gli indici linea per box, nell'ordine di lettura globale
    // (che dentro un box degrada a y-poi-x).
    let mut per_box: Vec<Vec<usize>> = vec![Vec::new(); boxes.len()];
    let mut orphan_idx: Vec<usize> = Vec::new();
    for &i in &assoc.order {
        match assoc.line_to_box[i] {
            Some(b) => per_box[b].push(i),
            None    => orphan_idx.push(i),
        }
    }

    // Sposta le Line fuori dal Vec senza clonare.
    let mut slots: Vec<Option<Line>> = lines.into_iter().map(Some).collect();
    let mut take = |idxs: &[usize]| -> Vec<Line> {
        idxs.iter().map(|&i| slots[i].take().expect("linea già consumata")).collect()
    };

    let mut blocks: Vec<Block> = boxes.iter().enumerate().map(|(bi, lb)| {
        let lines = take(&per_box[bi]);
        Block {
            bbox: lb.bbox,
            semantic_class: Some(semantic_of(&lb.label).to_string()),
            table_html: None,
            paragraphs: vec![Paragraph { bbox: lb.bbox, lines }],
        }
    }).collect();

    if !orphan_idx.is_empty() {
        let lines = take(&orphan_idx);
        let bbox = lines.iter().fold(BoundingBox::default(), |acc, l| acc.union(l.bbox));
        blocks.push(Block {
            bbox,
            semantic_class: None,
            table_html: None,
            paragraphs: vec![Paragraph { bbox, lines }],
        });
    }

    Page { page_number, width_px, height_px, page_angle, blocks }
}

// ─── Test ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn bb(l: i32, t: i32, r: i32, b: i32) -> BoundingBox {
        BoundingBox::from_lrtb(l, t, r, b)
    }

    fn lbox(l: i32, t: i32, r: i32, b: i32, label: &str, score: f32, ro: i32) -> LayoutBox {
        LayoutBox {
            bbox: bb(l, t, r, b),
            class_id: 0,
            label: label.into(),
            score,
            reading_order: ro,
        }
    }

    fn line(text: &str, l: i32, t: i32, r: i32, b: i32) -> Line {
        Line { text: text.into(), bbox: bb(l, t, r, b), confidence: 90.0, words: Vec::new() }
    }

    #[test]
    fn spec_scale_factor_is_per_axis() {
        let s = LayoutModelSpec::pp_doclayout_s();
        // 1000×1400 → [480/1400, 480/1000]: due valori DIVERSI.
        let sf = s.scale_factor(1000, 1400);
        assert!((sf[0] - 480.0 / 1400.0).abs() < 1e-6);
        assert!((sf[1] - 480.0 / 1000.0).abs() < 1e-6);
        assert!(sf[0] != sf[1]);
    }

    /// M ed S condividono tutto tranne la risoluzione: se un giorno
    /// divergessero (label_list diverso, NMS fuori dal grafo) questo test
    /// lo segnala prima che i box vengano interpretati con la classe
    /// sbagliata.
    #[test]
    fn m_and_s_differ_only_in_input_size() {
        let s = LayoutModelSpec::pp_doclayout_s();
        let m = LayoutModelSpec::pp_doclayout_m();
        assert_eq!((m.input_w, m.input_h), (640, 640));
        assert_eq!(m.labels, s.labels);
        assert_eq!(m.in_graph_nms, s.in_graph_nms);
        assert_eq!(m.has_im_shape, s.has_im_shape);
        assert_eq!(m.emits_reading_order, s.emits_reading_order);
        assert_eq!(m.mean, s.mean);
        assert_eq!(m.draw_threshold, s.draw_threshold);
    }

    /// Le soglie vengono dal yml, non da scelte arbitrarie.
    #[test]
    fn thresholds_come_from_the_yml() {
        for spec in [LayoutModelSpec::pp_doclayout_s(), LayoutModelSpec::pp_doclayout_m()] {
            assert_eq!(spec.draw_threshold, 0.5, "draw_threshold del yml");
            assert_eq!(spec.nms_score_threshold, 0.3, "NMS.score_threshold del yml");
        }
        // V3 ha la NMS fuori dal grafo: nessun pre-filtro applicato.
        assert_eq!(LayoutModelSpec::pp_doclayout_v3().nms_score_threshold, 0.0);
    }

    #[test]
    fn specs_match_the_yml() {
        let s = LayoutModelSpec::pp_doclayout_s();
        assert_eq!((s.input_w, s.input_h), (480, 480));
        assert!(s.in_graph_nms && !s.has_im_shape && !s.emits_reading_order);
        assert_eq!(s.labels.len(), 23);
        assert!(s.mean.is_some());

        let v3 = LayoutModelSpec::pp_doclayout_v3();
        assert_eq!((v3.input_w, v3.input_h), (800, 800));
        assert!(!v3.in_graph_nms && v3.has_im_shape && v3.emits_reading_order);
        assert_eq!(v3.labels.len(), 25);
        // norm_type: none → solo /255, niente mean/std.
        assert!(v3.is_scale && v3.mean.is_none());
    }

    #[test]
    fn decode_filters_padding_and_low_score() {
        let spec = LayoutModelSpec::pp_doclayout_s();
        // 3 righe [class, score, l, t, r, b]: valida, sotto soglia, padding.
        let raw = [
            2.0, 0.9, 10.0, 10.0, 100.0, 50.0,
            2.0, 0.2, 10.0, 60.0, 100.0, 90.0,
            -1.0, 0.0, 0.0, 0.0, 0.0, 0.0,
        ];
        let boxes = decode_layout(&raw, 6, spec.labels, 0.5, 800, 600);
        assert_eq!(boxes.len(), 1);
        assert_eq!(boxes[0].label, "text"); // class 2 in S = text
        assert_eq!(boxes[0].reading_order, -1);
    }

    #[test]
    fn decode_reads_reading_order_from_col7() {
        let v3 = LayoutModelSpec::pp_doclayout_v3();
        let raw = [22.0, 0.9, 10.0, 10.0, 100.0, 50.0, 3.0];
        let boxes = decode_layout(&raw, 7, v3.labels, 0.5, 800, 600);
        assert_eq!(boxes[0].label, "text"); // class 22 in V3 = text
        assert_eq!(boxes[0].reading_order, 3);
    }

    #[test]
    fn nms_keeps_highest_score() {
        let boxes = vec![
            lbox(0, 0, 100, 50, "text", 0.85, -1),
            lbox(5, 5, 105, 55, "text", 0.95, -1), // overlap >> soglia
            lbox(200, 200, 250, 250, "doc_title", 0.70, -1),
        ];
        let kept = nms(boxes, 0.5);
        assert_eq!(kept.len(), 2);
        assert!((kept[0].score - 0.95).abs() < 1e-6);
    }

    #[test]
    fn xy_cut_two_columns_reads_left_first() {
        // Titolo a tutta pagina, poi due colonne affiancate.
        let rects = [
            bb(300, 500, 700, 900),  // colonna destra
            bb(50, 50, 950, 150),    // titolo
            bb(-50, 500, 250, 900),  // colonna sinistra (coordinate anche negative)
        ];
        assert_eq!(xy_cut_order(&rects), vec![1, 2, 0]);
    }

    #[test]
    fn ensure_reading_order_only_when_model_gives_none() {
        // S: nessun ordine dal modello → XY-Cut.
        let mut s_boxes = vec![
            lbox(0, 500, 900, 900, "text", 0.9, -1),
            lbox(0, 0, 900, 400, "doc_title", 0.9, -1),
        ];
        ensure_reading_order(&mut s_boxes);
        assert_eq!(s_boxes[0].reading_order, 1); // sotto → secondo
        assert_eq!(s_boxes[1].reading_order, 0); // sopra → primo

        // V3: l'ordine appreso non va toccato.
        let mut v3_boxes = vec![
            lbox(0, 500, 900, 900, "text", 0.9, 0),
            lbox(0, 0, 900, 400, "doc_title", 0.9, -1),
        ];
        ensure_reading_order(&mut v3_boxes);
        assert_eq!(v3_boxes[1].reading_order, -1);
    }

    #[test]
    fn postprocess_s_orders_via_xy_cut() {
        let spec = LayoutModelSpec::pp_doclayout_s();
        // Due box in ordine inverso di lettura: il basso prima nel raw.
        let raw = [
            2.0, 0.9, 10.0, 500.0, 900.0, 900.0,
            11.0, 0.9, 10.0, 10.0, 900.0, 400.0, // class 11 in S = doc_title
        ];
        let boxes = postprocess_layout(&raw, 6, &spec, 0.5, 0.5, 1000, 1000);
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].label, "doc_title"); // sopra, letto per primo
        assert_eq!(boxes[1].label, "text");
    }

    #[test]
    fn associate_containment_and_orphan_recovery() {
        let boxes = vec![
            lbox(0, 0, 500, 300, "text", 0.9, 0),
            lbox(0, 400, 500, 800, "text", 0.9, 1),
        ];
        let lines = [
            bb(10, 450, 490, 480),  // dentro il box 1
            bb(10, 50, 490, 80),    // dentro il box 0
            bb(10, 320, 490, 380),  // nel gap → orphan recovery sul più vicino
        ];
        let a = associate_lines(&lines, &boxes);
        assert_eq!(a.line_to_box[0], Some(1));
        assert_eq!(a.line_to_box[1], Some(0));
        assert!(a.line_to_box[2].is_some());
        assert_eq!(a.distance[0], 0.0);
        assert!(a.distance[2] > 0.0);
        // Reading order: box 0 prima di box 1; l'orphan segue il suo box.
        assert_eq!(a.order[0], 1);
    }

    #[test]
    fn associate_without_boxes_sorts_by_y_then_x() {
        let lines = [bb(0, 100, 50, 120), bb(0, 10, 50, 30), bb(60, 10, 110, 30)];
        let a = associate_lines(&lines, &[]);
        assert!(a.line_to_box.iter().all(Option::is_none));
        assert!(a.distance.iter().all(|d| d.is_infinite()));
        assert_eq!(a.order, vec![1, 2, 0]);
    }

    /// Documento a due colonne dentro UN SOLO layout-box (il caso reale di
    /// PP-DocLayout-S sulle scansioni): le righe non devono interlacciarsi.
    #[test]
    fn two_columns_in_a_single_box_are_not_interleaved() {
        // Colonna sx x∈[0,400], colonna dx x∈[600,1000], stesse y.
        let lines = [
            bb(0, 100, 400, 130),    // 0 sx riga 1
            bb(600, 100, 1000, 130), // 1 dx riga 1
            bb(0, 200, 400, 230),    // 2 sx riga 2
            bb(600, 200, 1000, 230), // 3 dx riga 2
            bb(0, 300, 400, 330),    // 4 sx riga 3
            bb(600, 300, 1000, 330), // 5 dx riga 3
        ];
        // Un unico box su tutta la pagina, come emette S.
        let boxes = vec![lbox(0, 0, 1000, 400, "image", 0.43, 0)];
        let a = associate_lines(&lines, &boxes);
        // Prima TUTTA la colonna sinistra, poi tutta la destra.
        assert_eq!(a.order, vec![0, 2, 4, 1, 3, 5]);
    }

    #[test]
    fn full_width_heading_is_read_before_the_columns() {
        let lines = [
            bb(600, 200, 1000, 230), // dx
            bb(0, 10, 1000, 60),     // titolo a tutta pagina
            bb(0, 200, 400, 230),    // sx
        ];
        let boxes = vec![lbox(0, 0, 1000, 400, "text", 0.9, 0)];
        let a = associate_lines(&lines, &boxes);
        assert_eq!(a.order, vec![1, 2, 0]);
    }

    #[test]
    fn split_columns_finds_the_two_bands() {
        let lines = [
            bb(0, 100, 400, 130), bb(600, 100, 1000, 130),
            bb(0, 200, 400, 230), bb(600, 200, 1000, 230),
        ];
        let bands = split_columns(&lines);
        assert_eq!(bands.len(), 2);
        assert_eq!((bands[0].left, bands[0].right), (0, 400));
        assert_eq!((bands[1].left, bands[1].right), (600, 1000));
        assert_eq!(bands[0].lines, vec![0, 2]);
        assert_eq!(bands[1].lines, vec![1, 3]);
    }

    #[test]
    fn split_columns_ignores_full_width_headings() {
        // Il titolo attraversa la gronda: senza esclusione nessun taglio
        // verticale sarebbe possibile e le colonne resterebbero invisibili.
        let lines = [
            bb(0, 0, 1000, 40),       // titolo a tutta pagina
            bb(0, 100, 400, 130), bb(600, 100, 1000, 130),
            bb(0, 200, 400, 230), bb(600, 200, 1000, 230),
        ];
        let bands = split_columns(&lines);
        assert_eq!(bands.len(), 2);
        // La riga 0 non appartiene a nessuna banda.
        assert!(!bands.iter().any(|b| b.lines.contains(&0)));
        assert_eq!(bands[0].lines, vec![1, 3]);
        assert_eq!(bands[1].lines, vec![2, 4]);
    }

    #[test]
    fn split_columns_returns_empty_on_single_column() {
        let lines = [bb(0, 0, 1000, 30), bb(0, 50, 1000, 80), bb(0, 100, 900, 130)];
        assert!(split_columns(&lines).is_empty());
    }

    /// Caso reale (quietanza Banca Popolare del Cassinate): due blocchi
    /// affiancati in testa — dettagli del mutuo a sinistra, indirizzo del
    /// destinatario a destra — seguiti da righe a TUTTA PAGINA (data/RIF e
    /// tabella importi).
    ///
    /// Il taglio verticale non esiste a livello di pagina, perché la
    /// tabella attraversa la gronda. Con il "primo gap" la ricorsione
    /// affettava in bande e usciva `Cap. Accordato ... VIA ROTABILE 74`;
    /// col gap più largo il primo taglio cade nello stacco fra
    /// intestazione e corpo, e i due blocchi restano interi.
    #[test]
    fn side_by_side_blocks_above_a_full_width_table() {
        let lines = [
            // Colonna sinistra (x 95–620)
            bb(95, 330, 460, 360),   // 0 MUTUO N.
            bb(95, 370, 300, 400),   // 1 FONDIARI TV
            bb(95, 455, 560, 485),   // 2 Data stipula
            bb(95, 500, 620, 530),   // 3 Cap. Accordato
            // Riquadro destro (x 1050–1300)
            bb(1050, 375, 1220, 405), // 4 EGR. SIG.
            bb(1050, 415, 1290, 445), // 5 BUONO ANDREA
            bb(1050, 500, 1300, 530), // 6 VIA ROTABILE 74
            // Righe a tutta pagina, molto più in basso (stacco ~250 px)
            bb(95, 830, 1900, 865),   // 7 CASSINO ... RIF.
            bb(95, 900, 1900, 940),   // 8 intestazione tabella
            bb(95, 1060, 1900, 1100), // 9 riga importi
        ];
        let order = xy_cut_order(&lines);
        let pos = |i: usize| order.iter().position(|&k| k == i).unwrap();

        // Tutta la colonna sinistra prima di tutto il riquadro destro.
        for &l in &[0usize, 1, 2, 3] {
            for &r in &[4usize, 5, 6] {
                assert!(pos(l) < pos(r),
                    "riga sinistra {l} deve precedere la destra {r}: ordine {order:?}");
            }
        }
        // Le righe a tutta pagina vengono dopo l'intestazione.
        for &full in &[7usize, 8, 9] {
            assert!(pos(6) < pos(full), "il corpo deve seguire l'intestazione");
        }
        // E fra loro restano in ordine verticale.
        assert!(pos(7) < pos(8) && pos(8) < pos(9));
    }

    #[test]
    fn detect_columns_counts_the_gutter() {
        let single = [bb(0, 0, 1000, 30), bb(0, 50, 1000, 80)];
        assert_eq!(detect_columns(&single), 1);

        let double = [
            bb(0, 100, 400, 130), bb(600, 100, 1000, 130),
            bb(0, 200, 400, 230), bb(600, 200, 1000, 230),
        ];
        assert_eq!(detect_columns(&double), 2);

        // Titolo a tutta pagina + due colonne sotto: resta 2.
        let mixed = [
            bb(0, 0, 1000, 40),
            bb(0, 100, 400, 130), bb(600, 100, 1000, 130),
        ];
        assert_eq!(detect_columns(&mixed), 2);
    }

    #[test]
    fn build_page_no_line_lost_and_orphan_block_last() {
        let boxes = vec![
            lbox(0, 0, 500, 300, "doc_title", 0.9, 0),
            lbox(0, 400, 500, 800, "image", 0.9, 1), // resta vuoto
        ];
        let lines = vec![
            line("titolo", 10, 50, 490, 80),
        ];
        let page = build_page(lines, &boxes, 3, 1000, 1400, 90);
        assert_eq!(page.page_number, 3);
        assert_eq!(page.page_angle, 90);
        // 2 block dai layout-box, nessun orphan.
        assert_eq!(page.blocks.len(), 2);
        assert_eq!(page.blocks[0].semantic_class.as_deref(), Some("title"));
        assert_eq!(page.blocks[0].paragraphs[0].lines[0].text, "titolo");
        // Il box figure resta come block vuoto (porta la classe semantica).
        assert_eq!(page.blocks[1].semantic_class.as_deref(), Some("figure"));
        assert!(page.blocks[1].paragraphs[0].lines.is_empty());
    }

    #[test]
    fn build_page_orphans_go_last_with_union_bbox() {
        let lines = vec![
            line("a", 0, 10, 100, 30),
            line("b", 0, 50, 200, 80),
        ];
        let page = build_page(lines, &[], 1, 800, 600, 0);
        assert_eq!(page.blocks.len(), 1);
        let orphan = &page.blocks[0];
        assert_eq!(orphan.semantic_class, None);
        assert_eq!(orphan.bbox, bb(0, 10, 200, 80));
        assert_eq!(orphan.paragraphs[0].lines.len(), 2);
    }

    #[test]
    fn semantic_covers_both_label_sets() {
        for l in PP_DOCLAYOUT_S_LABELS.iter().chain(PP_DOCLAYOUT_V3_LABELS.iter()) {
            // Nessun label deve andare in panico e tutti mappano a una
            // delle 8 categorie.
            let s = semantic_of(l);
            assert!(["text", "title", "list", "figure", "table", "header", "footer", "equation"].contains(&s));
        }
        assert_eq!(semantic_of("formula"), "equation");          // S
        assert_eq!(semantic_of("display_formula"), "equation");  // V3
        assert_eq!(semantic_of("qualcosa_di_nuovo"), "text");    // fallback
    }
}
