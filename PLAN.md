# PLAN — pdf-extractor-2-md

Obiettivo: `pdf | tiff | immagine → Markdown` con la migliore qualità possibile.
PDF nativi digitali ispezionati in profondità via pdfium (tutto il testo, non
solo Tj/TJ); pagine scansionate/illeggibili via pipeline PP-OCRv6 con
**Tesseract come secondo motore di fallback**; fusione per pagina.

## Architettura

```
input (pdf/tiff/img bytes)
  │
  ├─ [1] detection  (lopdf, scan dei content stream grezzi, per pagina)
  │       → PageClass {Digital, Scanned, Mixed, Empty}
  │       → pages_needing_ocr + OcrReason {Scanned, NoText, VectorText, Garbled}
  │       → hook QualityOracle (trait: implementazione dell'autore, più avanti)
  │
  ├─ [2] ramo nativo (pdfium)                 per le pagine Digital/Mixed
  │       chars+bbox+font+render-mode → parole/righe (Otsu per letterspacing)
  │       page objects (rect/linee) → tabelle algoritmiche
  │       bookmark, metadata, annotazioni → euristiche struttura
  │       immagini → export + crop su bbox
  │
  ├─ [3] ramo OCR                             per le pagine in pages_needing_ocr
  │       raster pdfium ≤200 DPI (DPI per pagina dagli XObject)
  │       orientamento → layout → engine switch:
  │           scelta utente > (accel. ONNX? → PP-OCRv6 : Tesseract)
  │       tabelle: cell detection + OCR per cella
  │
  ├─ [4] fusione per pagina (nativo ⊕ OCR, overlap di token, provenienza)
  │
  └─ [5] assemblaggio Markdown
          colonne/reading order → heading (bookmark prima, poi font) →
          liste/codice/caption → tabelle GFM → postprocess
```

Workspace Cargo alla radice, un crate `pdf-extractor-2-md` (lib + bin `pdf2md`)
con moduli `detect/`, `native/`, `ocr/`, `fuse/`, `markdown/`. Dipendenze
chiave: `lopdf` (solo detection), `pdfium-render` con load dinamico di
`native/<arch>/pdfium.dll`, il fork `old_project/paddle-ocr-rs` (path-dep o
vendorizzato, ort =2.0.0-rc.13 load-dynamic), `tesseract5-rs` da
`old_project/df-Tesseract` (feature `tesseract-engine`), `image`. Feature a
cipolla stile pdf-inspector: il default compila senza ONNX/Tesseract.

## Fasi

### Fase 0 — Scaffold ✅ prerequisiti già in repo (DLL, modelli)
- Workspace + crate, CLI `pdf2md <input> [-o out.md]` che apre il documento
  (lopdf) e stampa un report per pagina. Caricamento `pdfium.dll` e smoke test.
- LICENSE del nuovo progetto + attribuzioni (pdf-inspector MIT, derivati
  Apache-2.0 del fork paddle).

### Fase 1 — Detection pagina-per-pagina ✅ FATTA (2026-08-23)

Realizzata in `src/detect/` (tokenizer + regole) e `src/integrity.rs` (hook
chk_defaced). 22 test verdi; validata sul corpus `test/` (22 documenti, 659
pagine) **contro la classificazione indipendente via pdfium**: concordanza
piena, incluso il caso `OCR of Degraded Documents` riconosciuto come scansione
ricercabile (0 pagine in OCR) e i due `edpb` come misti (4 pagine in OCR).
Scelte fatte:
- tokenizer PDF vero al posto della ricerca di byte (stringhe, hex, nomi,
  commenti e inline image consumati come unità): un `Tj` dentro una stringa
  non è più un operatore;
- copertura immagine = |det(CTM)| della unit square, niente aritmetica di
  bounding box; soglia 0.85 (misurata sul corpus, separa tutti i casi);
- ⚠ bug trovato e corretto: gli XObject immagine sono **Stream**, non
  Dictionary — risolverli con `as_dict()` faceva leggere ogni scansione come
  pagina vuota (test di regressione `an_image_is_found_through_its_stream_dictionary`).

**Aperto**: su `2025_10_24_Laiuto…pdf` chk_defaced emette
`PDF.GLYPH_SEMANTIC_REPLACEMENT` High per le **legature** (`ﬁ ﬂ ﬀ ﬃ ﬄ`
"mapped from f") e per 3 coppie da subsetting (`j`←`h`, `k`←`ü`, `l`←`þ`),
che instradano 10 pagine pulite in OCR. Le legature sono già escluse nel
percorso specimen-OCR (`support/chk_defaced/src/specimen.rs`: "FP garantito")
ma non in quello deterministico — decisione dell'autore se aggiungere la
stessa guardia in `glyphmatch::legitimately_identical`.

### Riferimenti Fase 1  (pdf-inspector `detector.rs`)
- Scan dei content stream grezzi senza full load: conteggio `Tj/TJ/Tf/Do`,
  path op, immagini; strategie `EarlyExit/Full/Sample(n)` con prima+ultima
  pagina sempre incluse.
- Classi per pagina e `OcrReason` tipizzati; vector text
  (`path_ops≥1000 && >200×text_ops && alfanumerici<30`); template/tiled scan.
- Classificazione Digital/Scanned/Mixed/Empty sul **rendering mode** (`3 Tr`),
  non sulla presenza di testo (rif: edito-ocr-v6 `quality.py:180-253`).
- Trait `QualityOracle` (testo c'è ma è spazzatura?) con default permissivo:
  punto aperto riservato al codice dell'autore.

### Fase 2 — Estrazione nativa via pdfium  (rif: pdf-inspector, ri-ancorato)
- `FPDFText_*`: char, bbox, font (nome/peso/size), render mode, angolo.
- Raggruppamento char→parole→righe: soglia spazio dalla larghezza reale dello
  spazio del font; join letterspaced con soglia di **Otsu**
  (rif: `text_utils.rs:643-890`); split su column-gap.
- Page objects: rect e polilinee per tabelle/sottolineature; immagini con CTM.
- Bookmark (`FPDFBookmark_*`), metadata (`FPDF_GetMetaText`), annotazioni
  (`FPDFAnnot_*`: Link, FreeText, Highlight con `/Contents`) — raccolti come
  segnali per le euristiche di struttura.
- Export immagini: bitmap decodificata o render del crop al bbox, file accanto
  all'output MD e riferimento `![...]()` posizionale.

### Fase 3 — Markdown dal ramo nativo  (rif: pdf-inspector `markdown/`, `layout.rs`)
- Colonne: istogramma di proiezione + XY-cut fallback + Y-band; spanning lines
  pre-mascherate; clamp difensivo delle coordinate malformate.
- Heading: **bookmark come fonte primaria** quando coprono il documento; poi
  tier dimensioni font (≥1.2× body, correzione base-size dalle note), fallback
  bold ≥1.05×, classificatore document-sequence.
- Tabelle **algoritmiche** (più veloci del modello sui nativi): cascata
  rects (union-find) → linee/griglie → euristica sui gap, prima valida vince,
  con i veti anti-falsi-positivi (chart masking, furniture ripetuta, prosa
  parallela). Limite 25 colonne.
- Liste/codice/caption; postprocess: de-sillabazione, dot leader, numeri di
  pagina, URL→link.
- Test snapshot con fixture per patologia (riusare quelle di pdf-inspector).

### Fase 4 — Ramo OCR con fallback Tesseract  (rif: paddle-ocr-rs, df-ocr-switcher, edito-ocr-v6)
- Raster con pdfium: DPI per pagina dagli XObject, clamp [150, 200], pagine
  vettoriali a 200 (rif: `pipeline.py:497-522`).
- Pipeline Paddle dal fork: orientamento (idempotente a 0°) → PP-DocLayoutV3 →
  det (`LIMIT_SIDE_LEN=1280`) → rec (batch 8 per aspect-ratio, zero-padding già
  nel fork) → reading order XY-Cut corridoio più largo + orphan recovery.
- **Switch motore** (rif: `df-ocr-switcher/src/policy.rs`): scelta utente vince
  sempre; auto: acceleratore ONNX presente → Paddle, CPU pura → Tesseract.
  Tesseract "in gabbia": psm 6 sui crop delle regioni layout + psm 3 full-page
  di recupero + dedup; tabelle via cell detection RT-DETR-L + OCR per cella
  (unico modo di separare colonne fuse, vale per entrambi i motori).
- Invariante: il reading order riordina, non filtra (warning se il conteggio
  righe cambia). Nessuno scarto silenzioso.

### Fase 5 — Fusione e Mixed  (rif: pdf-inspector `vision/fusion.rs`)
- Per pagina: overlap di token tra nativo e OCR, scelta adattiva, inserimento
  dei soli frammenti novel; provenienza (`PageContentSource`) nel risultato.
- Retry testo invisibile sui Mixed (includere `3 Tr` se il visibile è
  garbage/vuoto — sblocca i layer OCR dietro le scansioni).
- Upgrade a posteriori: TextBased con MD garbage → tutte le pagine in OCR.

### Fase 6 — Qualità avanzata (opzionale, dopo che 1-5 funzionano)
- Arbitrato alla v6: Tesseract come **secondo lettore**. Meccanismo (descritto
  dall'autore): dizionario **multilingua** (hunspell, 6 lingue EU in
  `models/tesseract/tessdata/`); la lingua della pagina si ipotizza per
  **statistica** sulle parole riconosciute; ogni parola assente dal dizionario
  viene **ricroppata ad alta risoluzione** dalla pagina del PDF (non riusando
  il raster di lavoro a 200 DPI) e riletta con Tesseract **nella lingua
  ipotizzata**. Soglie misurate v6: `SIM_MIN=0.6`, arbitrato sotto confidenza
  98, crop ×2 (`--psm 7 --oem 1`). Numeri **segnalati e mai corretti** (solo
  cifra-contro-cifra); correzione confusioni non-generativa su hunspell;
  lessico di documento; word boxes dai timestep CTC (le tre correzioni di
  `recognize.py:127-241`) se servono ancore/posizioni nel MD.
- Benchmark su corpus reale; scoring semantico (non diff char-per-char).

## Ordine di lavoro consigliato
0 → 1 → 2 → 3 (a questo punto i PDF nativi producono MD di qualità) →
4 → 5 (documenti scansionati e misti) → 6.

## Verifica di fattibilità (2026-08-23, su codice reale)

Confermato: pdfium-render 0.8.37 copre path segments (tabelle algoritmiche),
char-level completo (render mode `3 Tr`, font name/weight/fixed-pitch,
tight/loose bounds, `is_generated`, `is_hyphen`), annotazioni con `contents`,
bookmark con destination, metadata Info, immagini raw/processed, form fields,
password, load da bytes, `thread_safe`. paddle-ocr-rs: `max_side_len` è
parametro del chiamante (→1280 ok), input `RgbImage`, confidenze per parola
(`WordBox.score`). ort =rc.13 uniforme tra i fork, nomi package distinti
(`paddle-ocr-rs` vs `ppocr-rs`). Modelli e DLL completi per entrambe le arch.

## Lacune individuate (da colmare, in ordine di fase)
- **Struct tree**: pdfium-render espone solo le FFI grezze
  (`FPDF_StructTree_*`), nessun wrapper — scrivere un piccolo wrapper nostro o
  usare lopdf (rif: pdf-inspector `structure_tree.rs`). [Fase 2/3]
- **XMP e `/Lang`**: pdfium dà solo i tag `/Info` — se servono, leggerli con
  lopdf. [Fase 2]
- **Input immagine/TIFF**: pdfium apre solo PDF; immagini e TIFF multipagina
  entrano direttamente nel ramo OCR (rif: `df-ocr-switcher/src/input/tiff.rs`,
  DPI per frame). Flusso da esplicitare. [Fase 4]
- **Rasterizzazione con DPI per pagina**: nessun codice Rust esistente — da
  portare da `edito-ocr-v6 pipeline.py:497-560` su pdfium. [Fase 4]
- **Switcher**: `process_file` accetta solo immagini/TIFF e dipende da
  `ppocr-rs` 0.8.5 (duplicherebbe il fork radice). Portare nel nuovo crate solo
  `policy.rs` + `engine/tesseract.rs` (in-cage), non dipendere dal crate
  intero. `has_accelerator` è un input: il rilevamento acceleratore ONNX va
  implementato. [Fase 4]
- **Batch recognition**: il fork fa padding batch-wide corretto ma inferenza
  un'immagine alla volta (`recognize_one` in loop) — aggiungere il vero batch 8
  ordinato per aspect-ratio (v6) come ottimizzazione. [Fase 4]
- **Tesseract multilingua**: `Ocr5Engine` fissa la lingua all'init — per
  l'arbitrato serve una cache di engine per lingua. [Fase 6]
- ~~Dizionari hunspell assenti~~ **RISOLTO 2026-08-23**: v. `models/README-lessico.md`.
  `models/hunspell/` = en/fr/es/pt non-GPL (MIT/BSD, MPL); it e de non
  esistono non-GPL → alternativa **Apache-2.0**: `models/wordlists/`
  (tesseract langdata_lstm, ordinate per frequenza — coprono anche il
  rilevamento lingua al posto di wordfreq); i GPL in quarantena in
  `models/hunspell-gpl/` per il confronto qualitativo di Fase 6. [Fase 6]
- **Oracolo lessicale in Rust**: `zspell`/binding hunspell per i dizionari
  non-GPL; per it/de lookup sulle wordlist; lingua di pagina dal rango di
  frequenza nelle wordlist (sostituto dello zipf di
  `arbitrate.py:252-290`). [Fase 6]
- ~~Modello formule non presente~~ **RISOLTO 2026-08-23**: scaricato
  `models/formula/inference.onnx` + `.yml` (PP-FormulaNet_plus-M, HF
  jinzhenj). Resta la scelta d'uso: LaTeX o crop-immagine. [Fase 6]
- **Corpus di verità per benchmark OCR** — approccio deciso dall'autore
  (2026-08-23): PDF **nativi digitali** forniti dall'autore, **rasterizzati**
  e poi degradati con rumore sintetico. La ground truth è il testo nativo
  estratto via pdfium dallo stesso PDF: gratis e allineata alla parola.
  Struttura prevista: `benchmark/<doc>/{originale.pdf, pagina-N.png, …}` con
  cartella gitignorata (documenti privati). Degradazioni da applicare a
  livelli crescenti: rotazione lieve (0,3-1°), blur gaussiano, artefatti JPEG,
  rumore sale-pepe, contrasto/illuminazione non uniforme, binarizzazione
  aggressiva (simula fax). Confronto a **multiset di parole** per pagina
  (lezione di `probe.py:126-132`), non a stringa concatenata. Limite noto da
  dichiarare nei risultati: il rumore sintetico non copre i difetti di
  scansione fisica (piega del foglio, timbri sovrapposti, mezzitoni della
  carta) — integrare più avanti con qualche scansione vera. [Fase 6]

## Punti aperti
- ~~`QualityOracle` (garbage detection): implementazione dell'autore~~
  **RISOLTO 2026-08-23**: il codice dell'autore è arrivato — crate
  **`chk_defaced` 0.2.4** (AGPL-3.0-only, crates.io, vendored in
  `support/chk_defaced/`). Copre font defacing (testo estratto ≠ disegnato) e
  testo nascosto/prompt injection. Piano d'integrazione dettagliato in
  `README_CHK_DOCUMENT.md` (§4): hook `DocumentIntegrityCheck`,
  `assessment.defaced` ⇒ pagine in `pages_needing_ocr` con motivo `garbled`,
  `hidden_text` ⇒ testo nascosto nel MD **marcato** mai omesso, conferma
  render-OCR "quasi gratis" nella fusione (Jaccard estratto-vs-OCR),
  FontRegistry da `models/fonts/` + font di sistema.
  Verifiche di compatibilità (2026-08-23, sul vendored):
  - `pdfium-render 0.8.37` = identica alla nostra; `ocr-tesseract` usa
    `tesseract5-rs` (stessa dep di df-Tesseract, tessdata condivisi). ✓
  - ⚠ `lopdf 0.36` vs il nostro `0.42`: i tipi `Document` NON sono
    interoperabili → niente riuso del documento già caricato; a breve
    `scan_path` (doppio parse, 35-90 ms, accettabile), a regime bump di
    chk_defaced a lopdf 0.42 (crate dell'autore).
  - Il trait `QualityOracle` in `src/detect.rs` diventa l'adattatore verso
    il `Report` di chk_defaced (Fase 1 del §4 del README).

## Decisioni prese (2026-08-23)
- Crate pdfium: **`pdfium-render`** (deciso dall'autore), binding dinamico a
  `native/<arch>/pdfium.dll`.
- **`paddle-ocr-rs` come path-dep** su `old_project\paddle-ocr-rs` quando
  servirà (Fase 4): è codice dell'autore, si aggiorna da GitHub
  (`dariofinardi/paddle-pipeline-ocr-rs`) — che è anche l'origin di questo
  stesso repo (branch `ort-rc13`).
