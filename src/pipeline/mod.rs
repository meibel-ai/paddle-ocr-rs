// ─────────────────────────────────────────────────────────────────────────────
// PROVENIENZA — logica pura assorbita dal crate `ocr-pipeline` (workspace ocr-wasm),
// Copyright (c) 2026 Dario Finardi / Jugaad s.r.l.
//
// Sottoinsieme portato: `types` (BoundingBox), `layout` (spec dei modelli, decodifica
// output, XY-Cut, associazione linee/box) e `preprocess` (bordi neri, despeckle, filtro
// dei righelli). NON portati `pdf`/`pdfwriter`: scrivono il layer di testo nel PDF, cosa
// che serve alla pipeline browser e non al riconoscimento.
//
// Motivo dell'assorbimento: rendere `ppocr-rs` autosufficiente — senza path-dependency
// verso un crate proprietario — e quindi pubblicabile e usabile da progetti terzi.
// ─────────────────────────────────────────────────────────────────────────────

pub mod layout;
pub mod preprocess;
pub mod types;

// Ri-esportazioni alla radice del modulo, identiche a quelle del `lib.rs` di `ocr-pipeline`:
// il codice che usava `ocr_pipeline::BoundingBox` continua a funzionare come
// `crate::pipeline::BoundingBox` senza toccare i punti d'uso.
pub use types::{Block, BoundingBox, Hierarchy, Line, Page, Paragraph, Word};
