# PIANO ATTIVITÀ E TEST — passaggio di consegne (2026-08-24)

Documento operativo per proseguire il lavoro. Ogni attività ha: comandi
esatti, criterio di accettazione, trappole. Leggere prima `CLAUDE.md`
(convenzioni vincolanti) e `RISULTATI.md` (stato delle misure). La storia
completa è in `PLAN.md`.

**Regole sempre valide**
- Il push lo fa l'autore a mano (`git push origin ort-rc13`): preparare i
  commit e fermarsi lì.
- Ogni modifica: unit test + verifica su documenti reali + misura. Mai
  fidarsi di "compila": i bug veri di questo progetto sono emersi SOLO
  misurando (v. RISULTATI §2 "come ci si è arrivati").
- Commit in italiano, codice e commenti in inglese, tipi leggeri, niente
  duplicazioni (CLAUDE.md).
- Macchina **ARM64**: la runtime ONNX giusta è `native/aarch64/onnxruntime.dll`
  (la x86_64 dà errore 193 incomprensibile). `ORT_DYLIB_PATH` si autoimposta
  nel CLI, non toccare.
- I comandi Bash della sessione hanno timeout 10 min: le corse lunghe vanno
  lanciate con `run_in_background` e il binario release NON si può ricompilare
  mentre una corsa lo sta usando (Accesso negato os error 5).
- Path degli script di scoring (fuori repo, nello scratchpad della sessione
  precedente — se mancano, ricrearli da RISULTATI §6 e dalle descrizioni qui):
  `three_way.py` (nativo, 3 configurazioni), `score_ocr.py <times.tsv> <engine>`
  (OCR per livello), `compare.py <dirA> <dirB>`.

---

## A. Arbitrato — ✅ MISURATO (2026-08-24), v. RISULTATI §4

Esito: recall +2,3 su L2, +1,0 su L1, +0,2 su L0, invariato su L3; 1.089
correzioni, 544 numeri segnalati, 0 toccati. Costo 8× su L3 → **resta da
fare**: tetto di sospetti oltre il quale saltare l'arbitrato, e la corsa di
confronto col lessico esteso (`.forms` CC) contro le sole wordlist.

1. Se il tsv ha <160 righe, rilanciare (il batch è idempotente NO — riscrive
   i .md, ma è deterministico: rilanciare intero va bene):
   `./target/release/pdf2md x --ocr-batch <scratch>/ocr_sample.txt --engine v6-small+tess > times_v6-small+tess.tsv 2> arbiter_report.txt`
   (`ocr_sample.txt` = lista dei 160 PNG; rigenerabile: tutte le pagine di
   circolare_garante, dettaglioAtto, 1785488689299, "QUALITY OF OCR FOR
   DEGRADED TEXT IMAGES", OJ_L_202402853_IT_TXT × L0-L3 da `benchmark/out/`).
2. Scoring: `python score_ocr.py times_v6-small+tess.tsv v6-small+tess`
   → confronto con la colonna v6-small in RISULTATI §4.
   **Accettazione**: recall L1/L2 ≥ v6-small; se scende, l'arbitrato sta
   facendo danni → ispezionare `arbiter_report.txt` (le correzioni sono
   elencate una per una) e stringere le soglie in `src/arbiter.rs`.
3. Il binario attuale include già il lessico esteso (morph/): ricompilare
   `cargo build --release --features "tesseract,ppocr"` e rifare la STESSA
   corsa con output `times_v6-small+tess2.tsv` → misura "wordlist vs forms".
   **Accettazione**: correzioni ≥ v1 e declined ≤ v1 (più parole conosciute =
   sospetti più mirati); tempi/pagina simili (il caricamento dei 36 MB di
   forms pesa solo sull'init).
4. Aggiornare RISULTATI.md §4-5 con le due righe di arbitrato e i tempi;
   commit.

**Criticità presunte (verificare, non fidarsi)**
- Nei declined compare `file` come sospetto: parole inglesi in testi italiani
  vengono sospettate se lo score è basso — rischio di correzioni su termini
  tecnici/nomi propri. Contromisura già attiva: serve il lessico che le
  conosca (con i .forms dovrebbe sparire). Se restano correzioni dubbie:
  aggiungere il "lessico di documento" di v6 (token visti ≥2 volte con score
  alto = parole vere locali, mai correggerle).
- `Outcome.language` è quello dell'ULTIMA pagina del batch (su L3 garbage può
  dire "fra"): è solo cosmetico nel report, ma non usarlo per decisioni.
- L'arbitrato su L3 spreca tempo (26 s/pagina) per testo irrecuperabile:
  valutare un tetto (se >N% delle parole è sospetto, saltare l'arbitrato —
  la pagina va a preprocessing, non a riletture).

## B. Policy di switch automatica — ✅ FATTA (2026-08-24, `src/policy.rs`)

Oggi `--engine` è manuale. Scrivere `src/policy.rs`:
- default **v6-small**; `v6-medium` VIETATO finché il difetto C1 non è
  risolto; Tesseract solo come: (a) paracadute se ort/modelli non caricano,
  (b) script non latini (tessdata `script/`), (c) secondo lettore (già fatto,
  è l'arbiter).
- La vecchia regola "CPU→Tesseract" di df-ocr-switcher NON va copiata: è
  smentita dalle misure (RISULTATI §5).
- Test: forzare il fallimento di ort (ORT_DYLIB_PATH invalido) e verificare
  che la pipeline degradi a Tesseract con un avviso, non un errore.

## C. Integrazione OCR nella conversione PDF — ✅ PONTE FATTO (2026-08-24)

`pdf2md doc.pdf -o out.md` ora legge davvero le pagine instradate: engine
aperto pigramente dalla policy, raster a 200 DPI, stesse colonne/blocchi,
provenienza nel MD (`<!-- pagina N: OCR v6-small -->`). Verificato su una
scansione vera (`test/scansioni/1783964703313.pdf`, 2 pagine): prima due
avvisi, ora testo corretto. **Resta da fare**:
1. ~~raster + engine + assemblaggio~~ fatto.
2. Misura: convertire `test/scansioni/*.pdf` e le 14 pagine garbled di
   `2025_10_24` → prima erano avvisi, ora testo.
   **Accettazione**: `test/scansioni/1783964703313.pdf` produce MD con testo
   (oggi produce solo avvisi); il tempo resta <10 s/pagina.
3. Trappola: l'orientamento pagina NON è gestito (i benchmark sono dritti, i
   PDF veri no) → prima di questa attività o insieme: doc-ori dal fork
   (`DocOrientationClassifier`) sulle sole pagine OCR, idempotente a 0°.

## D. Preprocessing L3 (il guadagno più grosso del ramo OCR)

Nota 2026-08-24: l'autore ha giudicato L3 troppo distruttivo e ha chiesto un
livello intermedio, **L2.5** (`--levels 25`, file `L2.5.png`): stessa
binarizzazione dura e una piega, ma **senza halftone** — il dithering del fax
è l'ingrediente che cancella i tratti sottili delle lettere e porta ogni
motore sotto il 32%. L3 resta invariato, così le misure già pubblicate
restano valide, e L2.5 diventa il livello su cui tarare il preprocessing.

RISULTATI §4: L3 (fax+pieghe) = 15-32% per TUTTI i motori. Serve il passo ⓪:
1. deskew: stimare l'angolo (proiezione a varianza massima, come nello script
   di verifica skew) e raddrizzare SOLO se |angolo| > 0,3° (idempotenza a 0);
2. denoise/binarizzazione: provare prima mediana 3×3 + Otsu globale; misurare
   su L3 con v6-small.
   **Accettazione**: recall L3 v6-small ≥ 60% (da 24,9%). Se non ci si
   arriva, documentare a che punto ci si ferma e perché.
3. Trappola v6: MAI applicare il preprocessing anche a L0-L2 senza misurare —
   "toccare i pixel prima del detector" ha effetti a distanza (PLAN, lezione
   dello sbiancamento).

## E. Tabelle dalle rules (Fase 3 restante)

Le rules ci sono (`src/native/objects.rs`), la griglia no:
1. `derive_grid`: cluster delle x delle rules verticali e delle y delle
   orizzontali (tolleranze in frazioni dell'altezza riga mediana!); celle =
   intersezioni; assegnare le righe di testo alle celle (centro nel
   rettangolo); emettere GFM.
2. Riferimento: il fork ha già `derive_grid`/`grid_to_gfm` in
   `old_project/paddle-ocr-rs/src/cell_detection.rs` — valutare il riuso
   prima di riscrivere.
3. Fixture: `old_project/pdf-inspector/tests/fixtures/2013-app2.pdf` (~98
   rules orizzontali/pagina già rilevate).
   **Accettazione**: quel PDF produce tabelle pipe con le colonne GIUSTE
   (SR/valuta separati), e — test anti-regressione fondamentale — ROPOLL
   (paper 2 colonne) NON produce nessuna tabella. È il falso positivo che
   affossa pdf-inspector (72,5% recall): guardia = una "tabella" senza almeno
   2 rules verticali E 2 orizzontali che si intersecano non è una tabella.

## F. Attività minori pronte
- liste/codice nel Markdown (bullet `•-*`, font monospace già rilevato);
- postprocess: numeri di pagina (folio già `furniture` col modello; senza
  modello usare posizione+ripetizione), URL→link;
- `--images files`: le figure oggi finiscono nella dir del MD; opzione dir
  dedicata;
- esporre `keep_furniture` sul CLI.

## G. Indagini aperte (criticità presunte, in ordine di rischio)

1. **v6-medium perde lettere sui crop inclinati** (RISULTATI §4): ipotesi
   principale = il rec medium è più sensibile all'interpolazione del crop
   prospettico del fork. Esperimento minimo: prendere 5 crop di riga da una
   L1, ruotarli via (deskew perfetto), passarli al rec medium isolato: se
   l'errore sparisce, la cura è il deskew (D); se resta, aprire issue sul
   fork (modello o preprocessing del rec).
2. **CC BY-SA dei lessici**: scelta fatta (morph-it ramo CC BY-SA 2.0,
   DEMorphy CC BY-SA 4.0) ma è una valutazione di licenza dell'AUTORE:
   fargliela confermare prima di un rilascio. Gli hunspell GPL in quarantena
   si possono eliminare se conferma.
3. **Il campione OCR è 5 documenti su 16**: prima di conclusioni definitive
   sul confronto motori, estendere ad altri 3-4 documenti (in particolare
   `ag 434` legale e `commercialista` con dialoghi).
4. **Similarità e soglie dell'arbiter** (0,90 score / 0,6 sim / 65 conf):
   ereditate da v6 su scala diversa (CTC 0-1 vs fuzz 0-100) — vanno
   RICALIBRATE su questo benchmark: curva correzioni-giuste/sbagliate al
   variare di SUSPECT_SCORE su un centinaio di casi etichettati a mano dal
   report dell'arbiter.
5. **Gutter <3 em senza modello**: il caso "titolo+colonna adiacente" della
   rivista resta aperto; una regola sul cambio di corpo è stata provata e
   rimossa perché non spostava le metriche. Non riprovarla senza un caso di
   test che la giustifichi.
6. **chk_defaced, 3 finding non-legatura su 2025_10_24** (`h→j`, `ü→k`,
   `þ→l`): 14 pagine buone vanno in OCR. Decisione dell'autore; possibile
   esperimento: specimen-OCR (feature `ocr-specimen` di chk_defaced) su quel
   font per confermare/confutare.
7. **DirectML/CoreML per LayoutV3**: mai provato; su ARM64 Windows DirectML
   potrebbe dare 3-5× sull'inferenza (0,9 s/pagina → ~0,2). Feature ort già
   nel fork. Rischio: EP non disponibile → fallback CPU silenzioso, misurare
   per accorgersene.

## H. Igiene repo
- `rm` dei `.md` generati dai batch OCR dentro `benchmark/out/` prima di
  eventuali backup (sono megabyte di derivati rigenerabili);
- il crate root `Cargo.toml exclude` non copre `RISULTATI.md`/`PIANO-*.md`:
  irrilevante finché non si pubblica;
- 21+ commit locali su `ort-rc13`: ricordare all'autore il push.
