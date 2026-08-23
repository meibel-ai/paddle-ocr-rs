# README_CHK_DOCUMENT — Verifica anomalie documento (font defacing + testo nascosto)

Piano e descrizione minuziosa del processo di verifica anti-manomissione sviluppato
dall'autore in `c:\progetti\edito.pdf.fix\` (app Go "Edito PDF Conversion"), da
integrare in **pdf-extractor-2-md**.

**Obiettivo qui**: identificare le anomalie dei PDF nativi digitali (font manomessi
in cui il testo estratto diverge da quello disegnato) **e** i PDF scannerizzati a cui
è stato applicato un comando nascosto per prompt injection (testo invisibile
sovrapposto al raster).

**Scoperta chiave**: il motore di verifica NON è scritto in Go. L'app Go è solo la
shell; il motore è il crate Rust **`chk_defaced` 0.2.4** (© Dario Finardi,
AGPL-3.0-only, pubblicato su crates.io, repo `dariofinardi/chk_defaced-rs`).
Essendo Rust e dell'autore, in questo progetto **si usa direttamente come
dipendenza** — niente porting, niente FFI.

Riferimento concettuale: "What you see is not what your AI reads"
<https://dariofinardi.it/what-you-see-is-not-what-your-ai-reads-c3fed388d3bc>.

---

## 1. Architettura del processo in edito.pdf.fix (com'è fatto oggi)

Tre strati:

```
UI Go (Fyne)                          osutil Go (FFI)                  Rust
ui/doccheck.go  ── JSON Report ──►  osutil/doccheck_windows.go ──►  EditoDocCheck.dll
(presentazione, attenuazione FP)    (LazyDLL, UTF-16 in, C-str out)  (wrapper cdylib)
                                                                        │
                                                                   chk_defaced 0.2.4
                                                                   (motore vero)
```

### 1.1 Lato Go — shell FFI (`support/go-reference/doccheck_windows.go`)

- La DLL `EditoDocCheck.dll` è cercata **accanto all'eseguibile**; se assente la
  funzione risulta non disponibile (`DocCheckAvailable() == false`), mai un errore.
- `EditoDocCheckScan(path_utf16) -> *char`: path passato come UTF-16
  null-terminated; ritorna **sempre** una stringa C UTF-8 (JSON `Report` oppure
  `{"error": "..."}`); mai null.
- La stringa vive nell'allocatore Rust: il Go la copia con `lstrlenA` +
  `RtlMoveMemory` (evita conversioni `uintptr`→`unsafe.Pointer` vietate dal memory
  model Go) e poi chiama `EditoDocCheckFree`.
- Scansione **deterministica, bloccante ma rapida: ~35–90 ms a documento** (niente
  OCR, niente rendering nella build DLL).

### 1.2 Wrapper Rust cdylib (`support/doccheck-ffi/`)

- `chk_defaced = { version = "0.2.4", default-features = false }` — esclude CLI
  (clap), backend HTML (scraper) e le feature OCR/webview: resta la sola scansione
  deterministica PDF/DOCX.
- Il path UTF-16 diventa `PathBuf`; la scansione è avvolta in
  `std::panic::catch_unwind` (un panic del parser su input ostile **non deve**
  attraversare il confine FFI); ogni esito è serializzato JSON con serde.
- Chiamata core: `chk_defaced::scan::scan_path(&path, None)` — il secondo
  argomento è il `FontRegistry` opzionale (la DLL non lo usa; noi possiamo, v. §4).

### 1.3 Presentazione Go (`support/go-reference/doccheck.go`)

Modello del Report (speculare al JSON serde) e logiche di presentazione che vanno
**replicate nella nostra pipeline** perché codificano conoscenza sul dominio:

- Severità ordinate `Info < Low < Medium < High < Critical`.
- Raggruppamento delle rilevazioni **per regola** (decine di occorrenze della
  stessa regola → una voce con conteggio).
- **Attenuazione del falso positivo chiave**: se l'unico driver di
  `hidden_text` è `PDF.INVISIBLE_RENDER_MODE` (render mode 3), il verdetto passa
  da allarme a "possibile falso positivo": è anche il normale layer di testo OCR
  ricercabile dei PDF scansionati — che la stessa app genera rasterizzando
  (cfr. `old_project/../invisible_text.go`: il nostro output usa proprio `3 Tr`).

---

## 2. Il motore `chk_defaced` — descrizione minuziosa

Sorgente vendored in `support/chk_defaced/` (copiato dal registry cargo, versione
0.2.4 identica a quella linkata dalla DLL). Due modelli di minaccia **ortogonali**
più igiene Unicode e provenienza.

### 2.0 Dispatch e report (`scan/mod.rs`, `finding.rs`)

- `scan_path(path, registry)` smista per estensione: `pdf` / `docx` (/ `html` con
  feature); formato non supportato → **errore**, mai silenzio.
- `Report { file, format, fonts_examined, metadata, assessment, findings,
  phrases, verdict }`.
- `Finding { rule, severity, category, message, location, confidence }` con
  `Category ∈ {FontIntegrity, UnicodeHygiene, HiddenContent, Structural}`.
- `Report::finalize()` calcola l'`Assessment` esplicito (idempotente, ricalcolato
  dopo eventuale escalation OCR):
  - `defaced` = almeno un finding `FontIntegrity` a severità ≥ High;
  - `hidden_text` = almeno un finding `HiddenContent` ≥ Medium;
  - `ok` = nessun finding ≥ High; `max_severity` = peggiore presente.
- `Verdict ∈ {Confirmed, Refuted, Unconfirmed}`: opinione render-level (OCR)
  sopra il segnale deterministico. `Refuted` = il font ha una collisione ma non è
  usata sul testo visibile ("rig caricato ma non sparato").
- `phrases: Vec<PhraseDiff{extracted, presumed, page, ocr}>`: le frasi colpite —
  testo **estratto** (ciò che legge un RAG/LLM) vs **presunto reso** (mappa di
  sostituzione applicata, case preservato) — con pagina 1-based, max 100,
  estrazione pagina-per-pagina.

### 2.1 Ramo A — Font defacement (estratto ≠ disegnato)

Il testo estratto mente; i glifi disegnano altro. Vettore: contratti/clausole che
il modello legge diverse da come appaiono. Controlli in `scan/pdf.rs`,
`pdf_glyph.rs`, `font.rs`, `glyphmatch.rs`, `registry.rs`.

Per ogni oggetto `/Type /Font` del grafo lopdf:

1. **`PDF.TOUNICODE_GARBLED`** (High, FontIntegrity, conf 0.85) — conta le
   destinazioni del `ToUnicode` (bfchar `<src><dst>`, bfrange `<lo><hi><dst>` e
   forma array `[<d0><d1>…]`; la destinazione è il token affidabile) che cadono
   in **PUA** (`U+E000–F8FF` + i due piani supplementari) o sono codepoint
   **invisibili/zero-width** (BOM `FEFF`, soft hyphen `00AD`, `200B–200D`,
   `2060–2064`). Soglia **≥ 4** (misurata: PDF puliti 1–2, attacchi 20–150+).
   Esenzione famiglie math/symbol (`cmex/cmsy/cmmi/msam/msbm/stmary/wasy/symbol/
   wingding/webding/dingbat`) che mappano legittimamente in PUA.

2. **`PDF.CUSTOM_DIFFERENCES`** (Info, conf 0.3) — `/Encoding` con
   `/Differences`: legittimo ma è **il** vettore dei redirect glifo↔codepoint.
   Segnalato, mai bloccato.

3. **`FONT.PUA_CMAP`** (High, conf 0.75) — sul programma font embeddato
   (FontFile/2/3 decompresso, parse `ttf-parser`, tutte le facce di una
   collection): se la **cmap interna** ha ≥ 16 voci e > 50% mappa in PUA su font
   non-symbol → offuscamento. NB onestà del v1: un font embeddato è quasi sempre
   un **subset** (name table e cmap ridotte, GID rinumerati), quindi il solo
   mismatch di cmap contro il registry NON prova nulla — servono gli outline.

4. **`PDF.MANY_SUBSETS`** (Medium, conf 0.6) — "variante B": stessa famiglia
   frazionata in **≥ 5 subset tag** distinti (`ABCDEF+Arial`) = possibile
   subsetting dinamico per-pagina/per-run che spezza gli anchor onesti.

5. **`PDF.GLYPH_SEMANTIC_REPLACEMENT`** (High, conf 0.9) — il cuore, "variante
   A3", deterministico, senza OCR né rendering (`pdf_glyph.rs::pdf_outline_scan`):

   - Tre layer incrociati: `ToUnicode` (code → char estratto), code → glifo
     (cmap interna del font; per i CID: Identity-H + `CIDToGIDMap`, sia
     `/Identity` sia **stream** big-endian u16 a offset `2*CID`, GID 0 filtrato),
     glifo → **outline** (`glyf`/CFF).
   - Ogni outline è hashata (**FNV-1a** sullo stream di comandi move/line/quad/
     curve/close a coordinate esatte in font unit — `font.rs::glyph_outline_hash`;
     un subset copia l'outline del padre verbatim ⇒ hash direttamente
     confrontabili). Il ramo (1) vale **solo per i Type0** (`is_type0`): per i
     font semplici il code del content stream passa da `/Encoding` e NON è un
     GID — hasharlo fabbricava collisioni spurie.
   - **Collisione** = stesso hash raggiungibile da ≥ 2 lettere diverse. La più
     frequente è la "verità", le altre le "bugie" → mappa `(estratto, disegnato)`.
   - Scope **lettere latine** (`glyphmatch::letter_latin`: alfabetiche, script
     Latin, lowercased — include accentate europee; altri script: né finding né FP).
   - Filtro `legitimately_identical` (zero-FP): stessa lettera; **cross-script**
     (b/β/в — condivisione legittima di glifo, l'attacco A3 è within-script);
     equivalenza **NFKD** (legature, presentation form); stessa **base ASCII**
     via deunicode (ð/đ/ɖ → "d", ø → "o"); coppia `i`/`l`; **skeleton TR39**
     (unicode-security).
   - **Soppressione artefatto shift uniforme** (`uniform_shift`): se un unico
     shift alfabetico ≠ 0 copre ≥ 8 coppie di lettere ASCII e ≥ 80% del totale
     (tipico LaTeX/subsetting: c→a, d→b, …) NON è un attacco (un Caesar globale
     ingarbuglierebbe ogni parola visibile; l'attacco vero è chirurgico) →
     un solo Info **`PDF.FONT_ENCODING_ARTIFACT`** e mappa azzerata.
   - Con sostituzioni non vuote: `verdict = Unconfirmed` e generazione dei
     `PhraseDiff` per pagina.

6. **`PDF.CANONICAL_TAMPER_CONFIRMED`** (High, conf 0.95) — check canonico
   contro il **FontRegistry** (se fornito, `registry.rs`): indice JSON dei font di
   sistema con identità (family/subfamily/full/postscript/version/copyright/
   manufacturer), `file_sha256`, `cmap_sha256` (cmap canonicalizzata), `cmap_len`,
   e soprattutto **outline hash per codepoint latino**. Per un font embeddato di
   famiglia nota: glifo che matcha l'hash canonico del suo stesso codepoint =
   onesto; glifo che matcha l'hash canonico di **un'altra** lettera = tamper
   **confermato con direzione corretta** (`verdict = Confirmed`, la mappa
   canonica sostituisce quella frequentista). Glifo modificato che non matcha
   nulla = dubbio (→ OCR), non un falso tamper.
   `identify()`: `Pristine` (sha file identico) / `KnownButCmapModified` /
   `KnownVariant` (cmap uguale, binario diverso = subset legittimo) /
   `Unidentified`.

**Escalation OCR** (feature opzionali, non nella DLL — noi le abbiamo quasi gratis,
v. §4):

- **Specimen-OCR** (`specimen.rs`, feature `ocr-specimen`) — per il caso
  irrisolvibile deterministicamente: font **completamente custom senza anchor
  onesto** (ogni code rimappato 1:1 → nessuna collisione interna). Si fabbrica la
  ground truth: per ogni **glifo distinto** (costo ∝ glifi, non pagine) si
  rasterizza uno specimen con tiny-skia (altezza 72 px, 6 ripetizioni, padding 12,
  gap 16, bianco/nero antialias) e lo si legge con Tesseract PSM 7. Voto:
  scarta letture sotto **confidenza 65** (misurato: lettere genuine ≥ 80,
  confusioni c/e, s/f, ı/l, a/d ≤ 47), maggioranza **stretta** tra le letture
  confidenti, pareggio → nessun finding; **legature escluse** (`U+FB00–FB4F`,
  disegnano più lettere → FP garantito). Confronto case-insensitive
  (`legitimately_identical_ci`). Su un paper LaTeX reale: da 8 FP a **0**, veri
  positivi invariati. Regola: `PDF.GLYPH_SEMANTIC_REPLACEMENT_OCR` (High, 0.8).
  Corre **solo se** il pass deterministico non ha già trovato replacement.
- **Atlas render-OCR** (`atlas.rs`, feature `ocr-atlas`) — rende le pagine con
  **pdfium**, OCR del rendering (ciò che un umano vede) vs testo estratto (ciò che
  una macchina legge): similarità **Jaccard su word-set** normalizzati
  (`textdiff.rs`; lowercase, run alfanumerici, len ≥ 2) e sostituzioni allineate
  parola-parola. Cattura anche la forma **localizzata/posizionale** di A3 che gli
  outline non possono esprimere, e produce la ground truth `ocr` nei PhraseDiff.
- DOCX: `render.rs` (webview wry/tao, feature `render-wry`) — un browser applica
  fedelmente le `@font-face` manomesse; i convertitori DOCX→PDF che ri-shapano i
  glifi "lavano" l'attacco. Non ci serve per i PDF.

### 2.2 Ramo B — Testo nascosto / occultato (estratto ma invisibile) → prompt injection

Inverso di A: il testo c'è nell'estrazione (l'LLM lo legge) ma un umano non lo
vede. Vettori: prompt injection, keyword-stuffing ATS, clausole nascoste.
`visibility.rs::pdf_visibility_scan` — deterministico, interprete parziale del
content stream con **modello del pittore**:

- Operatori tracciati: `q/Q` (stack GState), `cm` (CTM, convenzione row-vector),
  `BT` (reset Tm), `Tf` (font correnti + size), `Tr` (render mode), `Tm`,
  `Td/TD`, colori fill `g/rg/k` (CMYK→RGB naive), `sc/scn` → **colore ignoto,
  colore non giudicato** (anti-FP), `re` (solo rettangoli axis-aligned come
  background; path complessi ignorati = conservativo), painting
  `f/F/f*/b/b*/B/B*` (le rect riempite diventano `Region{bbox, colour}` in
  z-order, cap **4000** regioni, si tengono le più recenti = topmost),
  `S/s/n` (clear path), `Do` (XObject = regione opaca **di colore ignoto** +
  contributo alla copertura immagine), `Tj/TJ/'/"` (testo mostrato).
- Decodifica del testo mostrato per **citare** il nascosto nel finding (un
  consumatore deve VEDERE cosa è nascosto, non solo saperlo): `ToUnicode` del
  font corrente (2 byte per Type0, 1 per semplici); senza ToUnicode → proiezione
  byte stampabili `0x20–0x7E` + `0xA0–0xFF` (Latin-1). Campione cap 200 char.
- Per ogni run: dimensione efficace = `font_size × vscale(Tm·CTM)`; centro run
  stimato con avanzamento medio **0.5 em**; background locale = la regione
  **topmost** sotto il centro, con esenzione dei fill quasi-full-page
  (area > 0.9 · pagina = artefatto geometrico, non un box dietro il testo);
  default pagina bianca.
- **Tre vettori rilevati** (accumulati per pagina con bbox dei centri):
  1. `PDF.INVISIBLE_RENDER_MODE` (Medium, conf 0.5) — `Tr 3` (né fill né
     stroke) o `Tr 7` (solo clip);
  2. `PDF.TINY_TEXT` (Medium, conf 0.6) — dimensione efficace on-page
     `0 < h < 1.5 pt` (sub-visibile);
  3. `PDF.INVISIBLE_TEXT_COLOR` (Medium, conf 0.6) — colore testo ≈ colore del
     **background locale reale** (ε = 0.08 per canale): cattura
     bianco-su-bianco E blu-su-blu; bianco su header colorato = visibile, NON
     flaggato (questo è ciò che rende il check preciso).
  - Soglia comune `MIN_CHARS = 4` (artefatti trascurabili ignorati).
  - **Off-page/clipped deliberatamente NON deterministico**: servirebbe la
    geometria completa CTM-annidata/Form-XObject, che un interprete parziale
    sbaglia sui PDF reali (produceva candidati spurii) → lo risolve il pass
    render-OCR (pdfium disegna solo la pagina visibile).
- **Classificatore searchable-scan** (il punto critico per il NOSTRO caso d'uso
  sui PDF scansionati): una pagina è "scansione+OCR legittima" quando **tutte**:
  - le immagini coprono **≥ 70%** della pagina (occupancy grid **40×40**
    dell'unione dei bbox immagine, robusta a overlap e collage di tile);
  - **≥ 70%** del testo mostrato è invisibile (Tr 3/7);
  - il testo invisibile è **distribuito** sulla pagina: il bbox dei centri run
    copre ≥ **50%** della pagina nella dimensione minore (un layer OCR copre
    tutta la pagina; **un blocco iniettato è localizzato**).
  Se sì → il testo Tr3 è il layer OCR: **non** conteggiato come nascosto, un solo
  Info `PDF.OCR_TEXT_LAYER` che spiega il perché. **MA il testo camuffato per
  colore resta segnalato anche su pagine OCR** (è un vettore deliberato distinto,
  non il meccanismo OCR). ⇒ Un PDF scansionato con un comando di injection
  nascosto viene beccato o dal blocco Tr3 localizzato (spread < 0.5), o dal
  colore camuffato, o (fallback) dal confronto render-OCR.
- I finding sono **candidati (Medium)** per costruzione: sollevano il dubbio; la
  conferma a High spetta al pass render-OCR (garanzia dura verificata sul corpus
  pulito: **nessun High deterministico**). Equivalente DOCX:
  `DOCX.INVISIBLE_TEXT_COLOR` / `DOCX.TINY_TEXT` (High, luma > 0.9 = near-white
  su pagina bianca assunta, `w:sz < 8` half-points = < 4 pt, esenzione
  shading/highlight colorato) + `w:vanish`.

### 2.3 Igiene Unicode (`unicode.rs`)

Sul testo estratto, format-agnostiche: `UNICODE.PUA` (High, soglia ≥ 4),
`UNICODE.ZERO_WIDTH` (Medium, ZWSP/ZWNJ/ZWJ/word-joiner/BOM/soft-hyphen),
`UNICODE.BIDI_OVERRIDE` (Medium, `202A–202E`, `2066–2069` — possono invertire
l'ordine visivo). Nota: gli override bidi sono un vettore di injection testuale
autonomo, anche senza font manomessi.

### 2.4 Provenienza (`metadata.rs`)

`/Info` PDF (Title/Author/Creator/Producer/date, decodifica UTF-16BE con BOM o
Latin-1, date `D:YYYYMMDD…` normalizzate) e docProps DOCX (Dublin Core +
app.xml). Valore forense: Producer anomalo, o revision "1" su un contratto
"negoziato", sono segnali di contesto da mostrare accanto ai finding.

---

## 3. Catalogo regole (riassunto operativo)

| Regola | Sev | Significato |
|---|---|---|
| `PDF.TOUNICODE_GARBLED` | High | ToUnicode → PUA/invisibili ≥ 4: estratto garbled, resa normale |
| `FONT.PUA_CMAP` | High | cmap interna > 50% PUA su font non-symbol |
| `PDF.GLYPH_SEMANTIC_REPLACEMENT` | High | collisione outline: glifo disegna 'X', estratto 'Y' |
| `PDF.CANONICAL_TAMPER_CONFIRMED` | High | conferma vs registry outline canonici (direzione certa) |
| `PDF.GLYPH_SEMANTIC_REPLACEMENT_OCR` | High | conferma specimen-OCR (font senza anchor) |
| `PDF.MANY_SUBSETS` | Medium | ≥ 5 subset stessa famiglia (variante B) |
| `PDF.CUSTOM_DIFFERENCES` | Info | /Differences presente (vettore, non prova) |
| `PDF.FONT_ENCODING_ARTIFACT` | Info | shift alfabetico uniforme = artefatto LaTeX, non attacco |
| `PDF.INVISIBLE_TEXT_COLOR` | Medium | testo ≈ colore del background locale (camouflage) |
| `PDF.TINY_TEXT` | Medium | dimensione efficace < 1.5 pt |
| `PDF.INVISIBLE_RENDER_MODE` | Medium | Tr 3/7 fuori dal pattern OCR-scan |
| `PDF.OCR_TEXT_LAYER` | Info | scansione ricercabile legittima (immagine ≥70% + Tr3 ≥70% + spread ≥50%) |
| `UNICODE.PUA` / `ZERO_WIDTH` / `BIDI_OVERRIDE` | High/Med/Med | igiene del testo estratto |

Assessment: `defaced` = FontIntegrity ≥ High; `hidden_text` = HiddenContent ≥ Medium.

---

## 4. Integrazione in pdf-extractor-2-md (piano)

`chk_defaced` è esattamente il "codice suo" previsto dal CLAUDE.md per il punto
lasciato aperto («la rilevazione "testo presente ma spazzatura" sarà risolta da
codice suo — esporla come trait/hook»).

### Fase 1 — Dipendenza e hook
- `Cargo.toml`: `chk_defaced = { version = "0.2.4", default-features = false }`
  (stessa configurazione della DLL: niente clap/scraper/OCR). Fonte vendored di
  riferimento in `support/chk_defaced/`.
- Definire il trait/hook previsto (es. `DocumentIntegrityCheck`) con
  un'implementazione che chiama `scan::scan_path` (o meglio le funzioni `_doc`
  su `lopdf::Document` già caricato, per evitare un secondo parse) e restituisce
  il `Report`.
- Routing: `assessment.defaced == true` ⇒ le pagine coinvolte entrano in
  `pages_needing_ocr` con motivo tipizzato `garbled` (il testo nativo NON è
  affidabile: si estrae via OCR del raster, che legge ciò che è disegnato).
  Attenzione al doppio parse: noi usiamo pdfium per l'estrazione, chk_defaced usa
  lopdf — accettabile (35–90 ms), ma il documento lopdf va caricato una volta sola.

### Fase 2 — Report injection nel risultato MD
- `assessment.hidden_text == true` ⇒ il risultato dell'estrazione porta un blocco
  di **avvertimento** con le regole scattate e i **campioni citati** del testo
  nascosto (il consumatore deve vedere cosa è nascosto). Nessuno scarto
  silenzioso: il testo nascosto va nel MD **marcato** (es. blockquote
  `> ⚠ testo non visibile nell'originale: "…"`), mai omesso di nascosto, per la
  regola "il reading order riordina, non filtra".
- Replicare l'attenuazione FP del Go: se l'unico driver è
  `PDF.INVISIBLE_RENDER_MODE` e la pagina è classificata dal nostro router come
  `scanned`, declassare a nota informativa (è il layer OCR).

### Fase 3 — Conferma render-OCR "quasi gratis" (nostro vantaggio strutturale)
La pipeline ha già entrambi i lati del confronto che chk_defaced ottiene con la
feature `ocr-atlas`:
- ramo nativo: testo estratto con pdfium;
- ramo OCR: PP-OCRv6/Tesseract sul raster (che disegna SOLO il visibile).
⇒ Nella **fusione per pagina** (stile `fusion.rs`), calcolare la similarità
Jaccard word-set (riusare `chk_defaced::textdiff`) tra estratto e OCR:
- divergenza alta + finding defacing ⇒ `verdict = Confirmed`, severità High;
- parole presenti nell'estratto ma assenti nell'OCR del raster ⇒ conferma di
  testo nascosto (incluso off-page/clipped, che il deterministico non copre);
- questo chiude anche il caso "PDF scannerizzato + comando injection nascosto":
  il layer Tr3 legittimo coincide con l'OCR del raster; il blocco iniettato no.
- Opzionale (solo pagine sospette): escalation `ocr-specimen` con il nostro
  Tesseract già in-process (`tesseract5-rs` è la stessa dipendenza usata da
  chk_defaced — condividere tessdata in `models/`).

### Fase 4 — FontRegistry (ground truth canonica)
- Build una tantum: `FontRegistry::build_from_dir` sui font di sistema
  (`C:\Windows\Fonts`) **più** `models/fonts/` (sostituti metrici liberi:
  Liberation, Arimo=Arial, Tinos=Times, Cousine=Courier, Carlito=Calibri,
  Caladea=Cambria, EB Garamond) → `fonts-index.json` (cache locale, gitignored
  come i modelli). Passarlo a `scan_path` abilita
  `PDF.CANONICAL_TAMPER_CONFIRMED` con direzione certa delle sostituzioni.

### Vincoli da rispettare (convenzioni repo)
- Ogni soglia citata sopra è **misurata a monte** (corpus chk_defaced): riportarla
  nei commenti con provenienza `support/chk_defaced/src/<file>.rs`.
- AGPL-3.0-only: crate dell'autore, uso autorizzato per definizione; annotare in
  LICENSE l'attribuzione come già fatto per pdf-inspector.
- Trappola nota: lopdf e i path Windows `\\?\` — stesso strip del prefisso dopo
  `canonicalize()` che usiamo per ort/pdfium/Tesseract.

---

## 5. File di supporto copiati (da edito.pdf.fix e dal registry cargo)

| Cartella | Contenuto | Provenienza |
|---|---|---|
| `support/chk_defaced/` | Sorgente completo crate `chk_defaced` 0.2.4 (src, examples, tests, Cargo, LICENSE AGPL) | `~/.cargo/registry/src/...` (identico al crates.io usato dalla DLL) |
| `support/doccheck-ffi/` | Wrapper cdylib `EditoDocCheck` (lib.rs + Cargo.toml): pattern FFI catch_unwind/CString | `edito.pdf.fix/native/doccheck/` |
| `support/go-reference/` | `doccheck_windows.go` (consumo FFI) + `doccheck.go` (modello Report, ranking severità, attenuazione FP render-mode) | `edito.pdf.fix/osutil/`, `edito.pdf.fix/ui/` |
| `support/fontmaps/` | `agl.go` — Adobe Glyph List ufficiale (nome glifo → Unicode BMP, no legature multi-cp); `cff_strings.go` — i 391 SID standard CFF (Adobe TN 5176 App. A). Tabelle dati, banali da portare in Rust | `edito.pdf.fix/fixer/` |
| `models/cmaps/` | `cmaps.dat` (CMap CID predefinite), `umaps.dat` (ToUnicode predefinite), `mod.dat` — pack binari embeddati, esposti da `assets.GetCMap(name)` come fallback strutturale | `edito.pdf.fix/fixer/assets/fonts/` |
| `models/fonts/` | Famiglie canoniche libere (Liberation ×12, Arimo, Tinos, Cousine, Carlito, Caladea, EB Garamond, Noto Symbols, Amiri, Semplifica*) + licenze Apache-2.0/OFL. Ground truth per il FontRegistry §4 | `edito.pdf.fix/fixer/assets/fonts/` |

Nota: `models/fonts/` (~16 MB) e `models/cmaps/` (~2.5 MB) sono binari — valutare
se gitignorarli come `models/` o committarli (sono stabili e piccoli rispetto ai
modelli ONNX).

## 6. Cosa NON portare

- Il ponte Go/DLL (siamo già in Rust: dipendenza diretta).
- Il backend DOCX/HTML e il render webview (`render-wry`): fuori scope PDF→MD.
- L'interprete di visibilità NON va riscritto sopra pdfium "perché ce l'abbiamo":
  la versione lopdf di chk_defaced è già calibrata sul corpus (FP del painter
  model, esenzioni, soglie); pdfium serve invece per la conferma render-OCR (§4
  Fase 3), che è il suo ruolo naturale.
