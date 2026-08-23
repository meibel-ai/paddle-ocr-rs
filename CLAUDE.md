# pdf-extractor-2-md

Estrattore PDF/immagine → **Markdown** in Rust. Unisce il meglio delle pipeline
OCR precedenti dell'autore (raccolte in `old_project\`) con l'ispezione
profonda dei PDF nativi digitali.

## Idea architetturale (decisa, non ridiscutere senza motivo)

- **Due rami, routing pagina-per-pagina.** Una detection veloce (lopdf sui
  content stream grezzi, senza full load) classifica ogni pagina e produce
  `pages_needing_ocr` con **motivi tipizzati** (`scanned` / `no_text` /
  `vector_text` / `garbled`), mai un booleano per documento.
- **Ramo nativo digitale: pdfium** (crate **`pdfium-render`**, binding
  dinamico a `native/<arch>/pdfium.dll`). `FPDFText_*` decodifica ToUnicode/CID/CMap
  internamente (qualità Chrome): NON portare `tounicode.rs`/bcmap wasm di
  pdf-inspector. Da pdfium si prendono anche: render mode (`3 Tr`) per la
  classificazione, font info per gli heading, page objects (rect/linee) per le
  **tabelle algoritmiche**, bookmark, metadata, annotazioni, **estrazione e
  crop delle immagini**. Il raggruppamento char→parole/righe (incl. join
  letterspaced con soglia di Otsu) è compito nostro.
- **Ramo OCR: PP-OCRv6 con Tesseract come secondo motore di fallback.**
  Pipeline Paddle (orientamento → layout → det/rec) dal fork
  `old_project\paddle-ocr-rs`; switch di motore secondo
  `old_project\df-ocr-switcher\src\policy.rs`: scelta utente vince sempre,
  altrimenti acceleratore ONNX presente → Paddle, CPU pura → Tesseract
  (`tesseract5-rs`, FFI in-process). In edito-ocr-v6 Tesseract era invece il
  *secondo lettore* dell'arbitrato (Fase 6): dizionario multilingua, lingua di
  pagina ipotizzata per statistica, parole fuori dizionario **ricroppate ad
  alta risoluzione** dalla pagina e rilette con Tesseract nella lingua giusta.
- **`paddle-ocr-rs` è codice dell'autore**: path-dep su
  `old_project\paddle-ocr-rs`, aggiornabile da GitHub
  (`dariofinardi/paddle-pipeline-ocr-rs`, che è l'origin di questo repo).
- **Fusione per pagina** tra testo nativo e OCR (stile
  `pdf-inspector/src/vision/fusion.rs`), con provenienza tracciata.
- **Integrità documento: `chk_defaced` 0.2.4** (crate dell'autore, AGPL-3.0,
  vendored in `support/chk_defaced/`): font defacing (estratto ≠ disegnato,
  collisioni di outline hash) + testo nascosto/prompt injection (render mode,
  tiny text, colore ≈ background). È la risoluzione del punto "garbage
  detection" prima lasciato aperto. Piano d'integrazione minuzioso in
  `README_CHK_DOCUMENT.md` — leggerlo prima di toccare l'argomento. Regole
  chiave: `defaced` ⇒ pagina in OCR con motivo `garbled`; `hidden_text` ⇒ il
  testo nascosto va nel MD **marcato**, mai omesso; il layer OCR legittimo
  (`Tr 3` distribuito su scansione) non è un attacco. chk_defaced è
  **path-dep sul vendored** (`support/chk_defaced/`) allineato a **lopdf 0.44**
  come il resto del progetto (un solo lopdf nell'albero, `Document`
  scambiabile); `default-features = false` — mai abilitare CLI/HTML/webview,
  e **niente wasm in tutto il progetto** (scelta esplicita dell'autore).

## Layout del repo

- `PLAN.md` — piano di implementazione a fasi (leggere prima di lavorare).
- `STRUTTURA-OCR.md` — mappa del materiale OCR clonato da Edge (path ora sotto
  `old_project\`).
- `models/` — modelli ONNX + tessdata (~1 GB, gitignored). V. `models/README-edge-models.md`.
- `native/<arch>/` — DLL runtime: `onnxruntime.dll`, `pdfium.dll`,
  `tesseract55.dll`, `leptonica-1.85.0.dll` (aarch64 e x86_64).
- `old_project/` — SOLO riferimento, non compilare dentro il workspace nuovo:
  - `edito-ocr-v6/` — Python/ONNX, la pipeline OCR migliore (produce PDF
    ricercabili, non MD). Il suo `DECISIONS.md` e i docstring contengono le
    **soglie misurate** da riusare. Branch `feature/cloudrun` = solo infra.
  - `pdf-inspector/` — firecrawl/pdf-inspector v1.17 (MIT, terzi): riferimento
    per detection, PDF→MD, tabelle algoritmiche. Attribuire nel LICENSE ciò
    che si porta.
  - `paddle-ocr-rs/` — fork ort rc13 dell'autore, compila con 67 test verdi.
  - `df-ocr-switcher/`, `df-ppocr-rs/`, `df-Tesseract/` — pipeline Edge con
    switch Paddle⇄Tesseract e output GFM.

## Costanti misurate da rispettare (provenienza edito-ocr-v6)

`LIMIT_SIDE_LEN=1280` per la detection (non 960; oltre non rende);
batch rec = 8 ordinato per aspect-ratio, `target_w` multiplo di 8 (batch più
grandi degradano la qualità, misurato); raster di lavoro ≤200 DPI (400 DPI
peggiora sulle pagine vettoriali); soglie geometriche sempre in **frazioni di
grandezze intrinseche** (altezza mediana di riga), mai in pixel; XY-Cut sul
corridoio **più largo**, non il primo; il reading order **riordina, non
filtra** (invariante da verificare); orphan recovery: le righe fuori layout si
clusterizzano e si reinseriscono, mai scartate; stadi idempotenti a zero (a 0°
non toccare i pixel); niente scarti silenziosi (ogni riga scartata risale nei
log/risultato); opzioni non implementate → **errore**, mai accettate senza
effetto.

## Trappole note

- Le librerie native (ort, pdfium, Tesseract) NON capiscono i path Windows
  verbatim `\\?\C:\…`: spogliare il prefisso dopo `canonicalize()`.
- ort è `load-dynamic`: serve `ORT_DYLIB_PATH` → `native/<arch>/onnxruntime.dll`;
  `tesseract55.dll` + `leptonica-1.85.0.dll` accanto all'eseguibile o nel PATH.
- Un `character_dict` sbagliato produce garbage **senza errori**: dict sempre
  estratto dallo yml del modello, mai hardcodato.
- Preprocess det in **BGR**: invertire i canali non dà errore, dà risultati
  peggiori in silenzio.
- Niente WASM: scelta esplicita dell'autore.

## Convenzioni di codice (vincolanti)

- **Inglese nel codice**: naming (tipi, funzioni, campi, variabili) e commenti
  in inglese, sempre. I documenti di progetto (`CLAUDE.md`, `PLAN.md`,
  `README_CHK_DOCUMENT.md`, `STRUTTURA-OCR.md`) restano in italiano.
- **Tipi leggeri e ordinati**: un tipo = una responsabilità. Struct piccole,
  campi solo per ciò che va davvero memorizzato.
- **Non duplicare**: un dato vive in un posto solo. Se un valore si ricava da
  altri campi, è un metodo derivato, non un campo in più (es. `shown_bytes()`
  da `visible_bytes + invisible_bytes`). Niente getter banali su campi
  pubblici, niente wrapper che rigirano una sola chiamata, niente alias di
  metodi già esistenti: prima di aggiungerne uno, cercare quello che c'è.
- Non re-implementare ciò che una dipendenza già espone (es. l'`Assessment` di
  chk_defaced non va ricopiato in una struct nostra: si legge il suo).
- Ogni costante calibrata porta accanto la misura che l'ha prodotta (stile
  edito-ocr-v6) o il riferimento al file di provenienza in `old_project\`.
- Test: fixture mirate per patologia + snapshot Markdown (stile pdf-inspector).
