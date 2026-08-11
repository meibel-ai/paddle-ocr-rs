// ─────────────────────────────────────────────────────────────────────────────
// PROVENIENZA — modulo assorbito da `ocr-pipeline` (workspace ocr-wasm),
// file d'origine `crates/ocr-pipeline/src/types.rs`.
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

//! Modello dati engine-agnostic del risultato OCR gerarchico.
//!
//! Ogni engine ha la sua shape nativa. Questo modulo definisce **una
//! struttura comune** che tutti devono produrre, in modo che:
//!
//! - il sidecar JSON abbia un formato unico;
//! - il consumer legga una sola shape indipendentemente dall'engine;
//! - cambiare engine non richieda di toccare tutti i consumer;
//! - l'highlight overlay e il layer di testo PDF lavorino sulla stessa shape.
//!
//! ## Filosofia
//!
//! Gerarchia: `Page → Block → Paragraph → Line → Word`. Tutti i livelli
//! sono **sempre presenti** nella struct (anche se vuoti / synthetic),
//! così i consumer non devono fare branch per-engine.
//!
//! Quando un engine NON espone un livello si emette un livello synthetic
//! 1:1 con il parent: ogni line dentro un para da 1, dentro un block da 1.
//! Quando non espone le word individualmente, `Line.words` resta vuoto —
//! il consumer line-level continua a funzionare, quello word-level ottiene
//! `vec.is_empty() ⇒ no highlight word-level disponibile`.
//!
//! ## Nota sul port WASM
//!
//! Questo crate è **puro dato**: nessuna dipendenza da `ort`, da FFI C++ o
//! dal crate `image`. Compila su `wasm32-unknown-unknown` ed è il modello
//! condiviso fra il wasm e il resto della pipeline (vedi `docs/pipeline.md`).
//!
//! Le conversioni dagli engine vivono presso il chiamante:
//! - **tesseract.js** produce `blocks/paragraphs/lines/words` con `bbox` e
//!   `confidence`: mappatura 1:1 su questa gerarchia.
//! - **PP-DocLayout** fornisce i `Block` con `semantic_class` e l'ordine.

use serde::{Deserialize, Serialize};

/// Bounding box axis-aligned in coordinate pixel dell'immagine sorgente
/// (top-left = origin). Tipi `i32` per allinearsi ai backend che usano
/// interi con segno.
///
/// **Attenzione**: per il layer di testo PDF su pagine storte o ruotate
/// l'AABB non basta — serve il quadrilatero a 4 corner. Vedi
/// `docs/pipeline.md`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct BoundingBox {
    pub left:   i32,
    pub top:    i32,
    pub right:  i32,
    pub bottom: i32,
}

impl BoundingBox {
    pub fn from_lrtb(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self { left, top, right, bottom }
    }
    /// Builder duale a [`Self::from_lrtb`] partendo da `(x, y, w, h)`.
    pub fn from_xywh(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { left: x, top: y, right: x + w, bottom: y + h }
    }
    pub fn width(&self)  -> i32 { (self.right  - self.left).max(0) }
    pub fn height(&self) -> i32 { (self.bottom - self.top).max(0) }
    pub fn is_empty(&self) -> bool { self.width() == 0 && self.height() == 0 }

    /// Unione con un altro bbox. Un bbox vuoto è neutro — utile per
    /// derivare il bbox di un parent con un `fold`.
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() { return other; }
        if other.is_empty() { return self; }
        Self {
            left:   self.left.min(other.left),
            top:    self.top.min(other.top),
            right:  self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
        }
    }
}

/// Singola parola riconosciuta. Confidence in scala `0.0..=100.0`
/// (scala Tesseract; gli engine che emettono `0.0..=1.0` vanno moltiplicati
/// per 100 nel converter, per uniformità).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Word {
    pub text:       String,
    pub bbox:       BoundingBox,
    pub confidence: f32,
}

/// Linea di testo. `text` è la concatenazione delle word con singolo
/// spazio (preserva la reading order). Quando l'engine non espone le word
/// individualmente, `words` è vuoto ma `text/bbox/confidence` restano
/// popolati a livello linea.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Line {
    pub text:       String,
    pub bbox:       BoundingBox,
    pub confidence: f32,
    pub words:      Vec<Word>,
}

/// Paragrafo: 1+ linee con bbox unificato. In documenti senza struttura o
/// con engine senza paragraph detection si emette 1 paragrafo per linea.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Paragraph {
    pub bbox:  BoundingBox,
    pub lines: Vec<Line>,
}

/// Block: unità di layout di alto livello (colonna di testo, tabella,
/// figura, ecc.). L'ordine dei `Block` dentro `Page` **è** il reading
/// order: determina l'ordine del content stream PDF e quindi l'ordine di
/// copia-incolla ed estrazione.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Block {
    pub bbox: BoundingBox,
    /// Categoria semantica dal modello di layout, es. `"text"`, `"title"`,
    /// `"table"`, `"figure"`, `"header"`, `"footer"`. `None` per i blocchi
    /// orphan o quando il layout non è stato eseguito. Stringa libera —
    /// il `label_list` dipende dal modello (23 classi per PP-DocLayout-S).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_class: Option<String>,
    /// HTML della struttura, per i blocchi tabella, quando disponibile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table_html: Option<String>,
    pub paragraphs: Vec<Paragraph>,
}

/// Singola pagina di un documento.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Page {
    /// 1-based, numero pagina nel documento sorgente.
    pub page_number: u32,
    /// Dimensioni del bitmap sorgente in pixel — servono a scalare i bbox
    /// verso lo spazio PDF (`s = altezza_pagina_pt / height_px`).
    pub width_px:  u32,
    pub height_px: u32,
    /// Rotazione applicata al bitmap prima dell'OCR (0/90/180/270).
    ///
    /// **Necessaria per il layer di testo**: quando il classificatore di
    /// orientamento raddrizza la pagina, tutte le coordinate qui sotto sono
    /// nello spazio ruotato e vanno riportate nello spazio della pagina PDF
    /// originale. Senza questo campo l'errore è silenzioso.
    #[serde(default)]
    pub page_angle: u32,
    pub blocks: Vec<Block>,
}

/// Risultato gerarchico completo, multi-pagina.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hierarchy {
    pub pages: Vec<Page>,
}

impl Hierarchy {
    /// Iteratore flat su tutte le linee di tutte le pagine.
    pub fn iter_lines(&self) -> impl Iterator<Item = &Line> {
        self.pages.iter()
            .flat_map(|pg| pg.blocks.iter())
            .flat_map(|b| b.paragraphs.iter())
            .flat_map(|p| p.lines.iter())
    }
    /// Iteratore flat su tutte le word di tutte le pagine.
    pub fn iter_words(&self) -> impl Iterator<Item = &Word> {
        self.iter_lines().flat_map(|l| l.words.iter())
    }
    pub fn line_count(&self) -> usize { self.iter_lines().count() }
    pub fn word_count(&self) -> usize { self.iter_words().count() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bbox_from_xywh_to_lrtb_roundtrip() {
        let b = BoundingBox::from_xywh(10, 20, 100, 40);
        assert_eq!((b.left, b.top, b.right, b.bottom), (10, 20, 110, 60));
        assert_eq!(b.width(), 100);
        assert_eq!(b.height(), 40);
    }

    #[test]
    fn bbox_union_treats_empty_as_neutral() {
        let a = BoundingBox::default();
        let b = BoundingBox::from_lrtb(10, 10, 20, 20);
        assert_eq!(a.union(b), b);
        assert_eq!(b.union(a), b);
        let c = BoundingBox::from_lrtb(15, 5, 30, 18);
        assert_eq!(b.union(c), BoundingBox::from_lrtb(10, 5, 30, 20));
    }

    fn sample_page() -> Page {
        let line = Line {
            text: "Mario Rossi".into(),
            bbox: BoundingBox::from_lrtb(0, 0, 200, 30),
            confidence: 95.0,
            words: vec![
                Word { text: "Mario".into(), bbox: BoundingBox::from_lrtb(0, 0, 90, 30), confidence: 96.0 },
                Word { text: "Rossi".into(), bbox: BoundingBox::from_lrtb(100, 0, 200, 30), confidence: 94.0 },
            ],
        };
        let para = Paragraph { bbox: line.bbox, lines: vec![line] };
        let block = Block {
            bbox: para.bbox,
            semantic_class: Some("text".into()),
            table_html: None,
            paragraphs: vec![para],
        };
        Page { page_number: 1, width_px: 800, height_px: 1200, page_angle: 0, blocks: vec![block] }
    }

    #[test]
    fn hierarchy_iterators_walk_all_levels() {
        let h = Hierarchy { pages: vec![sample_page()] };
        assert_eq!(h.line_count(), 1);
        assert_eq!(h.word_count(), 2);
        assert_eq!(h.iter_words().nth(1).unwrap().text, "Rossi");
    }

    #[test]
    fn page_angle_survives_roundtrip() {
        let mut page = sample_page();
        page.page_angle = 270;
        let h = Hierarchy { pages: vec![page] };
        let json = serde_json::to_string(&h).unwrap();
        assert!(json.contains("\"page_angle\":270"));
        assert_eq!(serde_json::from_str::<Hierarchy>(&json).unwrap(), h);
    }

    #[test]
    fn page_angle_defaults_when_absent() {
        // Retrocompat con i sidecar scritti prima che il campo esistesse.
        let json = r#"{"pages":[{"page_number":1,"width_px":8,"height_px":9,"blocks":[]}]}"#;
        let h: Hierarchy = serde_json::from_str(json).unwrap();
        assert_eq!(h.pages[0].page_angle, 0);
    }
}
