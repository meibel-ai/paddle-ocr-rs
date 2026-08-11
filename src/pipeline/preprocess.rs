// ─────────────────────────────────────────────────────────────────────────────
// PROVENIENZA — modulo assorbito da `ocr-pipeline` (workspace ocr-wasm),
// file d'origine `crates/ocr-pipeline/src/preprocess.rs`.
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

//! Stadio ⓪ — pulizia del bitmap prima dell'OCR.
//!
//! Porting Rust dei filtri di `unpaper_python`
//! (`git.jugaad.it/experiments/unpaper_python`, a sua volta port di
//! unpaper 7.0.0), più uno che unpaper non ha. Qui stanno i tre che
//! risolvono i difetti osservati sul campo:
//!
//! - [`black_filter`] — bande nere di bordo lasciate dallo scanner;
//! - [`noise_filter`] — puntinatura isolata (despeckle);
//! - [`rule_filter`] — fili e tratteggi di scansione. **Non è unpaper**:
//!   vedi [`PreprocessLevel`] e `docs/filtro-righelli.md`.
//!
//! Modulo puro: nessun `image`, nessun `imageproc`, nessuna FFI. Lavora
//! direttamente sul buffer RGBA del canvas e compila su wasm32.
//!
//! ## Perché serve, oltre all'ovvio
//!
//! Il rumore non sporca soltanto il testo riconosciuto: **rompe il reading
//! order**. XY-Cut cerca corridoi bianchi che nessuna riga attraversa; un
//! singolo puntino riconosciuto come carattere in mezzo alla gronda fra due
//! colonne annulla il taglio verticale, e l'algoritmo ripiega
//! sull'affettamento orizzontale interlacciando i blocchi affiancati.
//! Ripulire il bitmap è quindi un prerequisito dell'ordinamento, non un
//! abbellimento.

use serde::{Deserialize, Serialize};

/// Intensità dello stadio ⓪.
///
/// Non è più un booleano perché i filtri non sono tutti equivalenti in
/// rischio. `Normal` è unpaper come lo conosciamo — solo interventi
/// *locali*, che non possono spostare il significato di una riga. `Strong`
/// aggiunge [`rule_filter`], che cancella **bande** e quindi decide che una
/// fascia di pagina non è testo: molto più efficace e molto meno timido.
/// Va scelto, non subìto.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PreprocessLevel {
    /// Nessun filtro: il bitmap arriva a Tesseract come l'ha prodotto lo scanner.
    Off,
    /// Bordi neri + despeckle. È il comportamento storico del flag booleano.
    Normal,
    /// In più il filtro dei righelli: fili, tratteggi e bande di scansione.
    Strong,
}

impl PreprocessLevel {
    /// Parsing tollerante, per l'attributo di una `<select>`.
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "off" | "none" | "0" | "false" => Some(Self::Off),
            "normal" | "1" | "true" | "on" => Some(Self::Normal),
            "strong" | "2" | "forte" => Some(Self::Strong),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Normal => "normal",
            Self::Strong => "strong",
        }
    }
}

/// Parametri dei filtri. I default replicano `unpaper_python`, che a sua
/// volta segue unpaper 7.0.0 salvo dove annotato.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreprocessOptions {
    pub noise_filter: bool,
    /// Raggio massimo del grumo da cancellare: area limite `(2i+1)²`.
    /// `2` → 25 px² (default di unpaper_python: non mangia le cifre
    /// piccole). unpaper usa `4` → 81 px².
    pub noise_intensity: u32,
    /// Sotto questo valore un pixel è "scuro" (0.9·255).
    pub white_threshold: u8,
    /// Distanza in pixel entro cui cercare altro inchiostro prima di
    /// cancellare un grumo piccolo.
    ///
    /// **Non è un raffinamento, è indispensabile.** Il solo criterio di
    /// area cancella la punteggiatura e i puntini sulle «i»: a 200 dpi un
    /// punto in corpo 10 misura ~10 px², sotto il limite di 25. Ma il
    /// rumore vero è *isolato*, mentre un punto sta a pochi pixel da
    /// un'asta. Si cancella solo ciò che non ha inchiostro vicino — che è
    /// lo spirito del noisefilter di unpaper, dove il conteggio dei vicini
    /// su anelli concentrici misura appunto l'isolamento.
    pub noise_isolation_px: u32,

    pub black_filter: bool,
    /// Larghezza della striscia di scansione.
    pub bf_scan_size: u32,
    /// Profondità della striscia dal bordo verso l'interno.
    pub bf_scan_depth: u32,
    /// Passo fra due posizioni successive della striscia.
    pub bf_scan_step: u32,
    /// Oscurità media minima (0–1) perché la striscia faccia scattare la
    /// rimozione.
    pub bf_scan_threshold: f32,
    /// Sotto questo valore un pixel è "nero".
    pub bf_black_threshold: u8,
    /// Frazione che definisce la zona centrale protetta: `0.25` esclude
    /// `[w/4, h/4, 3w/4, 3h/4]`. Le strisce che la intersecano vengono
    /// saltate, così il filtro non può mai entrare nel corpo del documento.
    pub bf_border_margin: f32,

    /// Filtro dei righelli — attivo solo a [`PreprocessLevel::Strong`].
    pub rule_filter: bool,
    /// Sopra questa frazione dell'altezza mediana una componente **è
    /// testo**, e la sua fascia di righe diventa zona protetta.
    pub rf_text_min: f32,
    /// Sotto questa frazione dell'altezza mediana una componente è un
    /// **tratto sottile**, candidato a far parte di un righello.
    pub rf_flat_max: f32,
    /// Due tratti stanno nella stessa banda se i loro centri distano meno
    /// di questa frazione dell'altezza mediana.
    pub rf_band_tol: f32,
    /// La banda si cancella solo se copre almeno questa frazione della
    /// dimensione della pagina lungo la sua direzione.
    pub rf_span_min: f32,
    /// Numero minimo di tratti perché un allineamento sia una banda. Sotto
    /// questo, due trattini vicini non bastano a dedurre un righello.
    pub rf_min_strokes: usize,
}

impl Default for PreprocessOptions {
    fn default() -> Self {
        Self {
            noise_filter: true,
            noise_intensity: 2,
            white_threshold: 229,
            noise_isolation_px: 6,
            black_filter: true,
            bf_scan_size: 20,
            bf_scan_depth: 500,
            bf_scan_step: 5,
            bf_scan_threshold: 0.95,
            bf_black_threshold: 84,
            bf_border_margin: 0.25,
            // Spento per default: è il livello `Strong`, non il normale.
            rule_filter: false,
            rf_text_min: 0.70,
            rf_flat_max: 0.45,
            rf_band_tol: 0.60,
            rf_span_min: 0.35,
            rf_min_strokes: 3,
        }
    }
}

impl PreprocessOptions {
    /// Parametri corrispondenti a un livello, lasciando invariato tutto il
    /// resto. È l'unico posto dove un livello si traduce in flag: la UI
    /// passa il livello, non i booleani.
    pub fn for_level(level: PreprocessLevel) -> Self {
        let d = Self::default();
        match level {
            PreprocessLevel::Off => Self {
                black_filter: false,
                noise_filter: false,
                rule_filter: false,
                ..d
            },
            PreprocessLevel::Normal => Self {
                black_filter: true,
                noise_filter: true,
                rule_filter: false,
                ..d
            },
            PreprocessLevel::Strong => Self {
                black_filter: true,
                noise_filter: true,
                rule_filter: true,
                ..d
            },
        }
    }

    /// Il livello che questi flag rappresentano, per poterlo riportare.
    pub fn level(&self) -> PreprocessLevel {
        match (self.black_filter || self.noise_filter, self.rule_filter) {
            (_, true) => PreprocessLevel::Strong,
            (true, false) => PreprocessLevel::Normal,
            (false, false) => PreprocessLevel::Off,
        }
    }
}

/// Quanto è stato effettivamente ripulito — da mostrare, mai silenzioso.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PreprocessStats {
    /// Pixel sbiancati dal filtro dei bordi neri.
    pub black_pixels_cleared: usize,
    /// Grumi isolati rimossi.
    pub noise_blobs_removed: usize,
    /// Pixel sbiancati dal despeckle.
    pub noise_pixels_cleared: usize,
    /// Bande di righello individuate.
    pub rule_bands_found: usize,
    /// Tratti rimossi come parte di un righello.
    pub rule_strokes_removed: usize,
    /// Pixel sbiancati dal filtro dei righelli.
    pub rule_pixels_cleared: usize,
    /// Altezza mediana del testo misurata sulla pagina, in pixel: è il
    /// riferimento da cui derivano tutte le soglie del filtro righelli.
    /// Utile in log — se è assurda, il filtro non è affidabile.
    pub text_height_px: f32,
}

/// Luminanza percettiva (coefficienti ITU-R BT.601, come `PIL.convert("L")`).
#[inline]
fn luma(px: &[u8]) -> u8 {
    ((299 * px[0] as u32 + 587 * px[1] as u32 + 114 * px[2] as u32) / 1000) as u8
}

/// Converte RGBA in una mappa di luminanza.
fn to_gray(rgba: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut g = vec![0u8; w * h];
    for i in 0..w * h {
        g[i] = luma(&rgba[i * 4..i * 4 + 4]);
    }
    g
}

/// Etichetta le componenti connesse dei pixel `mask == true`.
///
/// Ritorna `(labels, sizes)` con `labels[i] == 0` per lo sfondo ed
/// etichette da 1 in poi; `sizes[l]` è l'area della componente `l`.
///
/// BFS iterativa con stack esplicito: su una pagina A4 a 200 dpi sono ~4
/// milioni di pixel e la ricorsione esploderebbe lo stack.
fn label_components(mask: &[bool], w: usize, h: usize, conn8: bool) -> (Vec<u32>, Vec<usize>) {
    let mut labels = vec![0u32; w * h];
    let mut sizes = vec![0usize]; // indice 0 = sfondo
    let mut stack: Vec<usize> = Vec::new();

    for start in 0..w * h {
        if !mask[start] || labels[start] != 0 {
            continue;
        }
        let label = sizes.len() as u32;
        let mut area = 0usize;
        labels[start] = label;
        stack.push(start);

        while let Some(p) = stack.pop() {
            area += 1;
            let (x, y) = (p % w, p / w);
            let visit = |nx: isize, ny: isize, stack: &mut Vec<usize>, labels: &mut Vec<u32>| {
                if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                    return;
                }
                let q = ny as usize * w + nx as usize;
                if mask[q] && labels[q] == 0 {
                    labels[q] = label;
                    stack.push(q);
                }
            };
            let (xi, yi) = (x as isize, y as isize);
            visit(xi - 1, yi, &mut stack, &mut labels);
            visit(xi + 1, yi, &mut stack, &mut labels);
            visit(xi, yi - 1, &mut stack, &mut labels);
            visit(xi, yi + 1, &mut stack, &mut labels);
            if conn8 {
                visit(xi - 1, yi - 1, &mut stack, &mut labels);
                visit(xi + 1, yi - 1, &mut stack, &mut labels);
                visit(xi - 1, yi + 1, &mut stack, &mut labels);
                visit(xi + 1, yi + 1, &mut stack, &mut labels);
            }
        }
        sizes.push(area);
    }
    (labels, sizes)
}

/// Bounding box inclusivi `(x1, y1, x2, y2)` per etichetta; l'indice 0 è
/// lo sfondo e resta degenere.
fn bounding_boxes(labels: &[u32], w: usize, h: usize,
                  n_labels: usize) -> Vec<(usize, usize, usize, usize)> {
    let mut bbox = vec![(usize::MAX, usize::MAX, 0usize, 0usize); n_labels];
    for y in 0..h {
        for x in 0..w {
            let l = labels[y * w + x] as usize;
            if l != 0 {
                let b = &mut bbox[l];
                b.0 = b.0.min(x);
                b.1 = b.1.min(y);
                b.2 = b.2.max(x);
                b.3 = b.3.max(y);
            }
        }
    }
    bbox
}

/// Direzione della banda cercata dal filtro dei righelli.
#[derive(Copy, Clone, PartialEq, Debug)]
enum Axis {
    /// Righelli orizzontali: sottili in altezza, estesi in larghezza.
    Horizontal,
    /// Righelli verticali: sottili in larghezza, estesi in altezza.
    Vertical,
}

/// Altezza mediana del testo, misurata sulla pagina stessa.
///
/// È il riferimento da cui dipende tutto il filtro dei righelli, e il motivo
/// per cui non ci sono soglie in pixel: una volta noto questo numero, "un
/// tratto sottile" e "una banda estesa" si esprimono come sue frazioni e
/// valgono a 150 come a 600 dpi.
///
/// La mediana su *tutte* le componenti sarebbe sporcata dai puntini e dalla
/// punteggiatura, che sono numerosissimi; si prende quindi la mediana di
/// quelle sopra il 60° percentile in altezza. Resta una statistica della
/// pagina, non un valore scelto a mano.
fn median_text_height(bbox: &[(usize, usize, usize, usize)]) -> f32 {
    let mut hs: Vec<usize> = bbox
        .iter()
        .skip(1)
        .filter(|b| b.0 != usize::MAX)
        .map(|b| b.3 - b.1 + 1)
        .collect();
    if hs.is_empty() {
        return 0.0;
    }
    hs.sort_unstable();
    let p60 = hs[((hs.len() as f32 * 0.60) as usize).min(hs.len() - 1)];
    let core: Vec<usize> = hs.iter().copied().filter(|&v| v >= p60).collect();
    core[core.len() / 2] as f32
}

/// Individua le etichette che appartengono a una banda di righello.
///
/// ## Perché il criterio è questo e non uno più semplice
///
/// Due criteri più ovvi sono **entrambi sbagliati**, e sono stati misurati
/// (`docs/filtro-righelli.md`):
///
/// - *dimensione da sola* → cancella la punteggiatura. È l'errore già fatto
///   col despeckle, che produsse `MUTUO N` e `Cassınate`.
/// - *dimensione + allineamento + estensione* → **51,7% delle componenti
///   rimosse**: anche i punti e le virgole di una riga di testo sono
///   sottili, allineati fra loro ed estesi per mezza pagina.
///
/// Il discriminante che regge è che il righello **non sta su nessuna riga di
/// testo**. La zona di testo è l'insieme delle posizioni trasverse occupate
/// da componenti alte come il testo; una virgola poggia sulla base di una
/// riga e quindi è protetta, un filo in mezzo al bianco no.
fn find_rule_bands(bbox: &[(usize, usize, usize, usize)], w: usize, h: usize, med: f32,
                   opts: &PreprocessOptions, axis: Axis) -> (Vec<usize>, usize) {
    let n = bbox.len();
    let (size_across, size_along) = match axis {
        Axis::Horizontal => (h, w),
        Axis::Vertical => (w, h),
    };
    // Intervallo trasverso alla banda (dove la banda è "sottile") e
    // intervallo lungo la banda (dove deve essere "estesa").
    let across = |b: &(usize, usize, usize, usize)| match axis {
        Axis::Horizontal => (b.1, b.3),
        Axis::Vertical => (b.0, b.2),
    };
    let along = |b: &(usize, usize, usize, usize)| match axis {
        Axis::Horizontal => (b.0, b.2),
        Axis::Vertical => (b.1, b.3),
    };
    let valid = |l: usize| bbox[l].0 != usize::MAX;

    // Zona di testo. La condizione sulla larghezza minima esclude le strisce
    // alte e sottilissime dei bordi di scansione: sono alte come il testo ma
    // non sono testo, e senza questo vincolo creerebbero una zona protetta
    // fittizia capace di schermare il righello.
    let mut occupied = vec![false; size_across];
    for l in 1..n {
        if !valid(l) {
            continue;
        }
        let b = &bbox[l];
        let hgt = (b.3 - b.1 + 1) as f32;
        let wid = (b.2 - b.0 + 1) as f32;
        if hgt >= opts.rf_text_min * med && wid >= opts.rf_text_min * med * 0.25 {
            let (a1, a2) = across(b);
            for p in a1..=a2.min(size_across - 1) {
                occupied[p] = true;
            }
        }
    }

    let touches_text = |b: &(usize, usize, usize, usize)| {
        let (a1, a2) = across(b);
        (a1..=a2.min(size_across - 1)).any(|p| occupied[p])
    };

    let mut cand: Vec<usize> = (1..n)
        .filter(|&l| valid(l))
        .filter(|&l| {
            let b = &bbox[l];
            let (a1, a2) = across(b);
            ((a2 - a1 + 1) as f32) <= opts.rf_flat_max * med && !touches_text(b)
        })
        .collect();
    if cand.is_empty() {
        return (Vec::new(), 0);
    }

    let center = |l: usize| {
        let (a1, a2) = across(&bbox[l]);
        (a1 + a2) as f32 / 2.0
    };
    cand.sort_by(|&a, &b| center(a).partial_cmp(&center(b)).unwrap());

    let mut out: Vec<usize> = Vec::new();
    let mut bands = 0usize;
    let tol = opts.rf_band_tol * med;
    let mut group: Vec<usize> = vec![cand[0]];

    let flush = |group: &[usize], out: &mut Vec<usize>, bands: &mut usize| {
        if group.len() < opts.rf_min_strokes {
            return;
        }
        let lo = group.iter().map(|&l| along(&bbox[l]).0).min().unwrap();
        let hi = group.iter().map(|&l| along(&bbox[l]).1).max().unwrap();
        if ((hi - lo + 1) as f32) < opts.rf_span_min * size_along as f32 {
            return;
        }
        *bands += 1;
        out.extend_from_slice(group);
        // La banda è ormai PROVATA non-testo: si sgombra per intero. Così
        // cadono anche i tratti appena più spessi della soglia di
        // sottigliezza, che altrimenti resterebbero dentro una fascia già
        // riconosciuta come artefatto e verrebbero letti come caratteri.
        // Il vincolo di non toccare la zona di testo resta.
        let blo = group.iter().map(|&l| across(&bbox[l]).0).min().unwrap();
        let bhi = group.iter().map(|&l| across(&bbox[l]).1).max().unwrap();
        for l in 1..n {
            if !valid(l) {
                continue;
            }
            let (a1, a2) = across(&bbox[l]);
            if a1 >= blo && a2 <= bhi && !touches_text(&bbox[l]) {
                out.push(l);
            }
        }
    };

    for &l in &cand[1..] {
        if center(l) - center(*group.last().unwrap()) <= tol {
            group.push(l);
        } else {
            flush(&group, &mut out, &mut bands);
            group = vec![l];
        }
    }
    flush(&group, &mut out, &mut bands);

    out.sort_unstable();
    out.dedup();
    (out, bands)
}

/// Rimuove fili, tratteggi e bande di scansione letti come testo.
///
/// **Questo filtro non è in unpaper**, e non per dimenticanza: i suoi
/// quattro filtri decidono tutti in base alla *densità*, e questo artefatto
/// sta esattamente nel buco fra loro. Misurato sul filo di
/// `Mutuo 2013.tiff`: densità massima 43,8% in finestra 20×20 (il
/// `blackfilter` scatta a 95%), 6,06% in finestra 100×100 (il `blurfilter`
/// cancella *sotto* 1%), tratti fino a 28×7 px (il `noisefilter` prende
/// pochi pixel), immagine bilevel (il `grayfilter` non si applica). Densità
/// media, dimensione media, nero pieno.
///
/// Effetto misurato su quella pagina: 1605 → 1392 caratteri, cioè 213 in
/// meno e **tutti garbage**, con lo 0,09% di inchiostro rimosso e il testo
/// vero intatto.
pub fn rule_filter(rgba: &mut [u8], w: usize, h: usize, opts: &PreprocessOptions,
                   stats: &mut PreprocessStats) {
    if w == 0 || h == 0 {
        return;
    }
    let gray = to_gray(rgba, w, h);
    let mask: Vec<bool> = gray.iter().map(|&v| v < opts.white_threshold).collect();
    let (labels, sizes) = label_components(&mask, w, h, true);
    let bbox = bounding_boxes(&labels, w, h, sizes.len());

    let med = median_text_height(&bbox);
    stats.text_height_px = med;
    // Senza una statistica di testo credibile il filtro non ha riferimento:
    // meglio non fare nulla che inventare una soglia.
    if med < 2.0 {
        return;
    }

    let (mut kill, bands_h) = find_rule_bands(&bbox, w, h, med, opts, Axis::Horizontal);
    let (kv, bands_v) = find_rule_bands(&bbox, w, h, med, opts, Axis::Vertical);
    kill.extend(kv);
    kill.sort_unstable();
    kill.dedup();

    stats.rule_bands_found += bands_h + bands_v;
    stats.rule_strokes_removed += kill.len();
    if kill.is_empty() {
        return;
    }

    let mut doomed = vec![false; sizes.len()];
    for l in kill {
        doomed[l] = true;
    }
    for i in 0..w * h {
        let l = labels[i] as usize;
        if l != 0 && doomed[l] {
            rgba[i * 4] = 255;
            rgba[i * 4 + 1] = 255;
            rgba[i * 4 + 2] = 255;
            stats.rule_pixels_cleared += 1;
        }
    }
}

/// Despeckle: cancella i grumi scuri isolati di area ≤ `(2·intensity+1)²`.
///
/// Corrisponde a `NoiseFilter` di unpaper_python (unpaper
/// `--noisefilter-intensity`). Connettività 8, come l'originale.
pub fn noise_filter(rgba: &mut [u8], w: usize, h: usize, opts: &PreprocessOptions,
                    stats: &mut PreprocessStats) {
    let gray = to_gray(rgba, w, h);
    let mask: Vec<bool> = gray.iter().map(|&v| v < opts.white_threshold).collect();
    let (labels, sizes) = label_components(&mask, w, h, true);

    let max_area = ((2 * opts.noise_intensity + 1) as usize).pow(2);
    let mut remove: Vec<bool> = sizes.iter().map(|&a| a > 0 && a <= max_area).collect();

    // Bounding box per etichetta, per il test di isolamento.
    let n_labels = sizes.len();
    let bbox = bounding_boxes(&labels, w, h, n_labels);

    // Un grumo piccolo si cancella solo se è ISOLATO: se entro
    // `noise_isolation_px` c'è inchiostro di un'altra componente, è
    // punteggiatura o il puntino di una «i», non rumore.
    let r = opts.noise_isolation_px as usize;
    for l in 1..n_labels {
        if !remove[l] {
            continue;
        }
        let (x1, y1, x2, y2) = bbox[l];
        let sx = x1.saturating_sub(r);
        let sy = y1.saturating_sub(r);
        let ex = (x2 + r + 1).min(w);
        let ey = (y2 + r + 1).min(h);
        let mut has_neighbour = false;
        'scan: for y in sy..ey {
            for x in sx..ex {
                let o = labels[y * w + x] as usize;
                if o != 0 && o != l {
                    has_neighbour = true;
                    break 'scan;
                }
            }
        }
        if has_neighbour {
            remove[l] = false;
        }
    }
    stats.noise_blobs_removed += remove.iter().filter(|&&r| r).count();

    for i in 0..w * h {
        let l = labels[i] as usize;
        if l != 0 && remove[l] {
            rgba[i * 4] = 255;
            rgba[i * 4 + 1] = 255;
            rgba[i * 4 + 2] = 255;
            stats.noise_pixels_cleared += 1;
        }
    }
}

/// Rimuove le bande nere di bordo lasciate dallo scanner.
///
/// Corrisponde a `BlackFilter` di unpaper_python (unpaper 7.0
/// `--blackfilter-*`): strisce che scorrono lungo i quattro bordi; dove la
/// striscia è abbastanza scura, si sbianca **tutta la componente connessa
/// nera che la tocca**.
///
/// La proprietà che rende il filtro sicuro: tocca solo pixel raggiungibili
/// da una striscia di bordo densa, quindi annotazioni e firme nei margini
/// sopravvivono se non sono attaccate a una banda nera. In più le strisce
/// che intersecano la zona centrale protetta vengono saltate del tutto.
pub fn black_filter(rgba: &mut [u8], w: usize, h: usize, opts: &PreprocessOptions,
                    stats: &mut PreprocessStats) {
    if w == 0 || h == 0 {
        return;
    }
    let gray = to_gray(rgba, w, h);
    let dark: Vec<bool> = gray.iter().map(|&v| v < opts.bf_black_threshold).collect();
    let (labels, _) = label_components(&dark, w, h, false); // 4-conn come unpaper

    let (wf, hf) = (w as f32, h as f32);
    let ex_l = (wf * opts.bf_border_margin) as usize;
    let ex_r = (wf * (1.0 - opts.bf_border_margin)) as usize;
    let ex_t = (hf * opts.bf_border_margin) as usize;
    let ex_b = (hf * (1.0 - opts.bf_border_margin)) as usize;

    let overlaps_exclude = |x1: usize, y1: usize, x2: usize, y2: usize| {
        x1 < ex_r && x2 > ex_l && y1 < ex_b && y2 > ex_t
    };
    let darkness = |x1: usize, y1: usize, x2: usize, y2: usize| -> f32 {
        if x2 <= x1 || y2 <= y1 {
            return 0.0;
        }
        let mut sum = 0u64;
        for y in y1..y2 {
            for x in x1..x2 {
                sum += gray[y * w + x] as u64;
            }
        }
        let n = ((x2 - x1) * (y2 - y1)) as f32;
        (255.0 - sum as f32 / n) / 255.0
    };

    // Etichette da sbiancare: quelle toccate da una striscia scura.
    let mut hit = vec![false; (labels.iter().copied().max().unwrap_or(0) as usize) + 1];
    let mark = |x1: usize, y1: usize, x2: usize, y2: usize, hit: &mut Vec<bool>| {
        for y in y1..y2 {
            for x in x1..x2 {
                let l = labels[y * w + x] as usize;
                if l != 0 {
                    hit[l] = true;
                }
            }
        }
    };

    let size = opts.bf_scan_size.max(1) as usize;
    let step = opts.bf_scan_step.max(1) as usize;
    let depth_h = (opts.bf_scan_depth as usize).min(h);
    let depth_v = (opts.bf_scan_depth as usize).min(w);

    // Passata orizzontale: bordo superiore, poi inferiore.
    let mut x = 0usize;
    while x < w {
        let x2 = (x + size).min(w);
        if !overlaps_exclude(x, 0, x2, depth_h) && darkness(x, 0, x2, depth_h) >= opts.bf_scan_threshold {
            mark(x, 0, x2, depth_h, &mut hit);
        }
        let y1 = h.saturating_sub(depth_h);
        if !overlaps_exclude(x, y1, x2, h) && darkness(x, y1, x2, h) >= opts.bf_scan_threshold {
            mark(x, y1, x2, h, &mut hit);
        }
        x += step;
    }

    // Passata verticale: bordo sinistro, poi destro.
    let mut y = 0usize;
    while y < h {
        let y2 = (y + size).min(h);
        if !overlaps_exclude(0, y, depth_v, y2) && darkness(0, y, depth_v, y2) >= opts.bf_scan_threshold {
            mark(0, y, depth_v, y2, &mut hit);
        }
        let x1 = w.saturating_sub(depth_v);
        if !overlaps_exclude(x1, y, w, y2) && darkness(x1, y, w, y2) >= opts.bf_scan_threshold {
            mark(x1, y, w, y2, &mut hit);
        }
        y += step;
    }

    for i in 0..w * h {
        let l = labels[i] as usize;
        if l != 0 && hit[l] {
            rgba[i * 4] = 255;
            rgba[i * 4 + 1] = 255;
            rgba[i * 4 + 2] = 255;
            stats.black_pixels_cleared += 1;
        }
    }
}

/// Esegue lo stadio ⓪ sul buffer RGBA, in place.
///
/// Ordine: bordi neri → righelli → despeckle. Le due precedenze non sono
/// arbitrarie.
///
/// - **Bordi neri prima del despeckle**, come in `unpaper_python`:
///   invertirli frammenterebbe la banda di bordo in grumi che il black
///   filter non riconosce più come connessi alla striscia.
/// - **Righelli prima del despeckle**: il despeckle mangerebbe i trattini
///   più piccoli e isolati della banda, riducendo il numero di tratti
///   allineati sotto `rf_min_strokes` — cioè cancellando l'*indizio* invece
///   dell'artefatto, e lasciando in piedi i trattini grandi, che sono
///   proprio quelli che Tesseract legge come caratteri.
pub fn preprocess_rgba(rgba: &mut [u8], w: usize, h: usize,
                       opts: &PreprocessOptions) -> PreprocessStats {
    let mut stats = PreprocessStats::default();
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return stats;
    }
    if opts.black_filter {
        black_filter(rgba, w, h, opts, &mut stats);
    }
    if opts.rule_filter {
        rule_filter(rgba, w, h, opts, &mut stats);
    }
    if opts.noise_filter {
        noise_filter(rgba, w, h, opts, &mut stats);
    }
    stats
}

/// Come [`preprocess_rgba`] ma pilotato dal livello, che è ciò che la UI
/// espone. Scorciatoia per `preprocess_rgba(.., &PreprocessOptions::for_level(l))`.
pub fn preprocess_rgba_level(rgba: &mut [u8], w: usize, h: usize,
                             level: PreprocessLevel) -> PreprocessStats {
    preprocess_rgba(rgba, w, h, &PreprocessOptions::for_level(level))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Immagine bianca `w×h` in RGBA.
    fn white(w: usize, h: usize) -> Vec<u8> {
        vec![255u8; w * h * 4]
    }
    fn set(rgba: &mut [u8], w: usize, x: usize, y: usize, v: u8) {
        let i = (y * w + x) * 4;
        rgba[i] = v; rgba[i + 1] = v; rgba[i + 2] = v;
    }
    fn get(rgba: &[u8], w: usize, x: usize, y: usize) -> u8 {
        rgba[(y * w + x) * 4]
    }

    #[test]
    fn labels_find_separate_blobs() {
        // Due pixel isolati non adiacenti → due componenti.
        let (w, h) = (5usize, 1usize);
        let mask = vec![true, false, false, true, false];
        let (labels, sizes) = label_components(&mask, w, h, true);
        assert_eq!(sizes.len(), 3); // sfondo + 2
        assert_ne!(labels[0], labels[3]);
        assert_eq!(sizes[1], 1);
        assert_eq!(sizes[2], 1);
    }

    #[test]
    fn noise_filter_removes_speck_and_keeps_text() {
        let (w, h) = (60usize, 40usize);
        let mut img = white(w, h);
        // Puntino isolato 2×2 (area 4 ≤ 25) → deve sparire.
        for (x, y) in [(5, 5), (6, 5), (5, 6), (6, 6)] {
            set(&mut img, w, x, y, 0);
        }
        // Blocco 8×8 (area 64 > 25) → è contenuto, deve restare.
        for y in 20..28 {
            for x in 20..28 {
                set(&mut img, w, x, y, 0);
            }
        }
        let mut st = PreprocessStats::default();
        noise_filter(&mut img, w, h, &PreprocessOptions::default(), &mut st);

        assert_eq!(get(&img, w, 5, 5), 255, "il puntino va rimosso");
        assert_eq!(get(&img, w, 24, 24), 0, "il blocco di testo va preservato");
        assert_eq!(st.noise_blobs_removed, 1);
        assert_eq!(st.noise_pixels_cleared, 4);
    }

    /// Regressione osservata sul campo: il solo criterio di area
    /// cancellava i punti e i puntini sulle «i» (`MUTUO N`, `Cassınate`,
    /// `E 220 000 00`). Un grumo piccolo va rimosso solo se ISOLATO.
    #[test]
    fn noise_filter_preserves_punctuation_and_i_dots() {
        let (w, h) = (60usize, 40usize);
        let mut img = white(w, h);
        // Asta della «i»: 2×8 px a x=10.
        for y in 12..20 {
            for x in 10..12 {
                set(&mut img, w, x, y, 0);
            }
        }
        // Puntino della «i» 2×2 a 4 px sopra l'asta → area 4, ma NON isolato.
        for (x, y) in [(10, 7), (11, 7), (10, 8), (11, 8)] {
            set(&mut img, w, x, y, 0);
        }
        // Punto fermo 2×2 accanto a un'asta, a 3 px → non isolato.
        for y in 12..20 {
            for x in 30..32 {
                set(&mut img, w, x, y, 0);
            }
        }
        for (x, y) in [(35, 18), (36, 18), (35, 19), (36, 19)] {
            set(&mut img, w, x, y, 0);
        }
        // Rumore vero: lontano da tutto.
        for (x, y) in [(50, 3), (51, 3)] {
            set(&mut img, w, x, y, 0);
        }

        let mut st = PreprocessStats::default();
        noise_filter(&mut img, w, h, &PreprocessOptions::default(), &mut st);

        assert_eq!(get(&img, w, 10, 7), 0, "il puntino della i va preservato");
        assert_eq!(get(&img, w, 35, 18), 0, "il punto fermo va preservato");
        assert_eq!(get(&img, w, 50, 3), 255, "il rumore isolato va rimosso");
        assert_eq!(st.noise_blobs_removed, 1, "solo il grumo isolato");
    }

    #[test]
    fn black_filter_clears_left_border_band() {
        let (w, h) = (200usize, 100usize);
        let mut img = white(w, h);
        // Banda nera verticale sul bordo sinistro (x 0..6), a piena altezza.
        for y in 0..h {
            for x in 0..6 {
                set(&mut img, w, x, y, 0);
            }
        }
        // Testo al centro, nella zona protetta.
        for y in 45..55 {
            for x in 90..110 {
                set(&mut img, w, x, y, 0);
            }
        }
        let opts = PreprocessOptions { bf_scan_depth: 6, ..Default::default() };
        let mut st = PreprocessStats::default();
        black_filter(&mut img, w, h, &opts, &mut st);

        assert_eq!(get(&img, w, 2, 50), 255, "la banda di bordo va rimossa");
        assert_eq!(get(&img, w, 100, 50), 0, "il testo centrale va preservato");
        assert!(st.black_pixels_cleared >= 600);
    }

    #[test]
    fn black_filter_spares_margin_annotations() {
        let (w, h) = (200usize, 100usize);
        let mut img = white(w, h);
        // Segno isolato nel margine sinistro, NON attaccato al bordo.
        for y in 40..50 {
            for x in 20..30 {
                set(&mut img, w, x, y, 0);
            }
        }
        let opts = PreprocessOptions { bf_scan_depth: 6, ..Default::default() };
        let mut st = PreprocessStats::default();
        black_filter(&mut img, w, h, &opts, &mut st);
        assert_eq!(get(&img, w, 25, 45), 0, "annotazione a margine preservata");
        assert_eq!(st.black_pixels_cleared, 0);
    }

    /// Pagina sintetica con tre righe di testo, la punteggiatura sulla
    /// linea di base e un filo tratteggiato nel bianco in alto.
    ///
    /// Le proporzioni contano: le componenti di testo devono essere la
    /// maggioranza, altrimenti la mediana viene trascinata giù dai trattini
    /// e il riferimento non è più l'altezza del testo. È vero anche sulle
    /// pagine reali (1778 componenti, mediana 24 px) ed è il motivo per cui
    /// il filtro non si fida di una pagina quasi vuota.
    fn page_with_hairline() -> (Vec<u8>, usize, usize) {
        let (w, h) = (200usize, 100usize);
        let mut img = white(w, h);
        // 3 righe × 20 glifi 4×15: sono la statistica del testo.
        for &y0 in &[40usize, 60, 80] {
            for i in 0..20 {
                let x0 = 5 + 10 * i;
                for y in y0..y0 + 15 {
                    for x in x0..x0 + 4 {
                        set(&mut img, w, x, y, 0);
                    }
                }
            }
        }
        // Punteggiatura 3×3 sulla base della prima riga, negli spazi fra i
        // glifi: piccola, allineata, estesa per 163 px. Il criterio ingenuo
        // la cancellerebbe.
        for &x0 in &[10usize, 50, 90, 130, 170] {
            for y in 51..54 {
                for x in x0..x0 + 3 {
                    set(&mut img, w, x, y, 0);
                }
            }
        }
        // Il filo: 16 trattini 10×3 a y=10, dal bordo al bordo.
        //
        // La misura 10×3 = 30 px² non è arbitraria: deve stare **sopra** il
        // limite del despeckle `(2·2+1)² = 25`, come i trattini veri (fino a
        // 28×7 px). Con trattini più piccoli il filo lo cancellerebbe già il
        // despeckle, e il test non distinguerebbe più `Normal` da `Strong`.
        for i in 0..16 {
            let x0 = 5 + 12 * i;
            for y in 10..13 {
                for x in x0..x0 + 10 {
                    set(&mut img, w, x, y, 0);
                }
            }
        }
        (img, w, h)
    }

    #[test]
    fn rule_filter_removes_hairline_and_keeps_punctuation() {
        let (mut img, w, h) = page_with_hairline();
        let opts = PreprocessOptions::for_level(PreprocessLevel::Strong);
        let mut st = PreprocessStats::default();
        rule_filter(&mut img, w, h, &opts, &mut st);

        assert_eq!(st.text_height_px, 15.0, "altezza del testo misurata male");
        assert_eq!(st.rule_bands_found, 1, "una sola banda, quella del filo");
        assert_eq!(get(&img, w, 6, 11), 255, "il filo va rimosso");
        assert_eq!(get(&img, w, 102, 11), 255, "anche in mezzo alla pagina");
        assert_eq!(get(&img, w, 11, 52), 0,
                   "la punteggiatura sulla riga di base va preservata");
        assert_eq!(get(&img, w, 6, 45), 0, "il testo va preservato");
        // 16 trattini × 30 px: solo il filo, nient'altro.
        assert_eq!(st.rule_pixels_cleared, 480);
    }

    /// Il difetto che il criterio ingenuo produceva: «sottile + allineato +
    /// esteso» descrive anche i punti di una riga di testo, e sulla pagina
    /// reale cancellava il 51,7% delle componenti. Qui si verifica che la
    /// zona di testo li protegga anche in assenza del filo.
    #[test]
    fn rule_filter_leaves_a_clean_page_alone() {
        let (mut img, w, h) = page_with_hairline();
        // Si rimuove il filo a mano: resta una pagina con solo testo e punti.
        for i in 0..16 {
            let x0 = 5 + 12 * i;
            for y in 10..13 {
                for x in x0..x0 + 10 {
                    set(&mut img, w, x, y, 255);
                }
            }
        }
        let before = img.clone();
        let opts = PreprocessOptions::for_level(PreprocessLevel::Strong);
        let mut st = PreprocessStats::default();
        rule_filter(&mut img, w, h, &opts, &mut st);
        assert_eq!(st.rule_bands_found, 0, "nessuna banda su una pagina pulita");
        assert_eq!(img, before, "una pagina pulita non va toccata");
    }

    #[test]
    fn rule_filter_needs_a_credible_text_statistic() {
        // Pagina quasi vuota: solo tre trattini. Senza testo da cui dedurre
        // la scala, il filtro deve astenersi invece di inventare una soglia.
        let (w, h) = (200usize, 100usize);
        let mut img = white(w, h);
        for &x0 in &[5usize, 100, 190] {
            set(&mut img, w, x0, 10, 0);
        }
        let before = img.clone();
        let opts = PreprocessOptions::for_level(PreprocessLevel::Strong);
        let mut st = PreprocessStats::default();
        rule_filter(&mut img, w, h, &opts, &mut st);
        assert_eq!(img, before);
        assert_eq!(st.rule_bands_found, 0);
    }

    #[test]
    fn levels_map_to_filters() {
        let off = PreprocessOptions::for_level(PreprocessLevel::Off);
        assert!(!off.black_filter && !off.noise_filter && !off.rule_filter);
        let normal = PreprocessOptions::for_level(PreprocessLevel::Normal);
        assert!(normal.black_filter && normal.noise_filter && !normal.rule_filter);
        let strong = PreprocessOptions::for_level(PreprocessLevel::Strong);
        assert!(strong.black_filter && strong.noise_filter && strong.rule_filter);

        // Il livello si deve poter rileggere dai flag, per riportarlo.
        for l in [PreprocessLevel::Off, PreprocessLevel::Normal, PreprocessLevel::Strong] {
            assert_eq!(PreprocessOptions::for_level(l).level(), l);
            assert_eq!(PreprocessLevel::parse(l.as_str()), Some(l));
        }
        // Il booleano storico continua a valere: era esattamente `Normal`.
        assert_eq!(PreprocessLevel::parse("true"), Some(PreprocessLevel::Normal));
        assert_eq!(PreprocessLevel::parse("false"), Some(PreprocessLevel::Off));
        assert_eq!(PreprocessLevel::parse("pippo"), None);
    }

    #[test]
    fn level_off_is_a_noop() {
        let (mut img, w, h) = page_with_hairline();
        let before = img.clone();
        let st = preprocess_rgba_level(&mut img, w, h, PreprocessLevel::Off);
        assert_eq!(img, before);
        assert_eq!(st, PreprocessStats::default());
    }

    /// `Normal` non deve toccare il filo: è la differenza fra i due livelli,
    /// e se sparisse la scelta a tre stati non avrebbe senso.
    #[test]
    fn level_normal_leaves_the_hairline_and_strong_removes_it() {
        let (mut a, w, h) = page_with_hairline();
        let sa = preprocess_rgba_level(&mut a, w, h, PreprocessLevel::Normal);
        assert_eq!(get(&a, w, 6, 11), 0, "Normal non tocca il filo");
        assert_eq!(sa.rule_pixels_cleared, 0);

        let (mut b, _, _) = page_with_hairline();
        let sb = preprocess_rgba_level(&mut b, w, h, PreprocessLevel::Strong);
        assert_eq!(get(&b, w, 6, 11), 255, "Strong lo rimuove");
        assert!(sb.rule_pixels_cleared > 0);
        assert_eq!(get(&b, w, 11, 52), 0, "senza perdere la punteggiatura");
    }

    #[test]
    fn preprocess_is_a_noop_when_disabled() {
        let (w, h) = (20usize, 20usize);
        let mut img = white(w, h);
        set(&mut img, w, 3, 3, 0);
        let before = img.clone();
        let opts = PreprocessOptions { noise_filter: false, black_filter: false, ..Default::default() };
        let st = preprocess_rgba(&mut img, w, h, &opts);
        assert_eq!(img, before);
        assert_eq!(st, PreprocessStats::default());
    }
}
