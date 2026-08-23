# Struttura del materiale OCR

> **Nota 2026-08-23**: i progetti sorgente (`df-ocr-switcher/`, `df-ppocr-rs/`,
> `df-Tesseract/`, `edito-ocr-v6/`, `pdf-inspector/`) e il fork preesistente
> alla radice (`paddle-ocr-rs/`, ex `src/`+`Cargo.toml` di questo repo) sono
> stati spostati in `old_project\` per fare spazio al nuovo progetto
> pdf-extractor-2-md. I path relativi tra i crate (`../df-ppocr-rs`, ecc.)
> restano validi perché sono rimasti fratelli dentro `old_project\`. `models/`
> e `native/` (DLL runtime) restano alla radice: servono anche al nuovo
> progetto. Quindi ovunque questo documento dica `df-…/` o «radice», leggere
> `old_project\df-…\`.

Clone della pipeline OCR completa di Omissis.Edge (switch PP-OCRv6 ⇄
Tesseract) copiato da `c:\Progetti\Edge` il 2026-08-23. Questa nota
serve a orientarsi senza rileggere i sorgenti.

## Mappa dei folder

```
pdf-extractor-2-md/
├── src/, Cargo.toml, …          ← il fork paddle-ocr-rs PREESISTENTE di questo
│                                   repo (variante nativa ort). NON fa parte del
│                                   clone: è il materiale che c'era già.
│
├── df-ocr-switcher/             ← LA PIPELINE. Crate orchestratore:
│   │                               PDF scansionato / TIFF multipagina / immagine → Markdown
│   ├── src/policy.rs              lo SWITCH: `EnginePolicy::decide` è una funzione
│   │                              pura — scelta utente vince sempre; altrimenti
│   │                              acceleratore ONNX presente → Paddle, CPU pura → Tesseract
│   ├── src/engine/ppocr.rs        engine PP-OCRv6 (via ppocr-rs)
│   ├── src/engine/tesseract.rs    engine Tesseract 5.5 (via tesseract5-rs, FFI in-process)
│   │                              riconoscimento "in gabbia": psm 6 sui crop delle
│   │                              regioni layout + psm 3 full-page di recupero + dedup
│   ├── src/output/markdown.rs     output GFM reale: tabelle pipe + formule LaTeX
│   ├── src/bin/ocr_doc.rs         CLI `ocr-doc` (flag --tables per SLANet_plus)
│   ├── docs/pipeline-ocr.svg      diagramma dell'intera pipeline, 5 stadi + rombo di switch
│   ├── docs/integration-edge/     COPIE DI RIFERIMENTO (non compilano qui) dei due
│   │                              file con cui Edge consuma il crate:
│   │                              `switcher_channel.rs` (bridge, dedup parole/righe,
│   │                              conversione a Page) e `pipeline.rs` (auto-detect
│   │                              engine + enum OcrEngine serializzato lowercase)
│   └── tests/                     step2..step5 + benchmark + conformità
│
├── df-ppocr-rs/                 ← dipendenza path `../df-ppocr-rs` dello switcher.
│                                   Fork ppocr-rs 0.8.5: det/rec/cls PP-OCR, layout
│                                   PP-DocLayoutV3, orientamento applicato UNA volta
│                                   prima di layout+OCR (`page_angle` nel risultato),
│                                   cell detection, SLANet. I bbox vivono nello spazio
│                                   pagina RADDRIZZATA.
│
├── df-Tesseract/
│   ├── tesseract5-rs/           ← dipendenza path dello switcher (feature
│   │                               `tesseract-engine`): binding FFI a tesseract55.dll,
│   │                               TSV hierarchy completa (block/para/line/word)
│   ├── dist/aarch64|x86_64/       DLL Tesseract+Leptonica COMPILATE da questo repo
│   └── TESSERACT-BUILD-HOWTO.md   come rigenerarle da zero
│
├── models/                      ← tutti i modelli della pipeline (~1 GB)
│   ├── paddleocr/
│   │   ├── v6/medium|small|tiny   det.onnx + rec.onnx + dict — PP-OCRv6 latin
│   │   │                          (medium = default qualità/velocità di Edge)
│   │   ├── latin/                 vecchi modelli latin (fallback dichiarato)
│   │   ├── layout/PP-DocLayoutV3.onnx
│   │   ├── orientation/           PP-LCNet doc-ori (0/90/180/270)
│   │   ├── table/                 SLANet_plus (struttura) + RT-DETR-L wired/wireless
│   │   │                          (cell detection — usata ANCHE nel canale Tesseract:
│   │   │                          OCR per-cella, unico modo di separare colonne fuse)
│   │   └── cls/                   text-line classifier
│   ├── tesseract/tessdata/        ita/eng/fra/deu/spa/por + osd (6 lingue EU)
│   └── README-edge-models.md      note originali di Edge sui modelli
│
├── native/                      ← DLL runtime pronte, per architettura
│   ├── aarch64/                   onnxruntime.dll (+providers_shared), tesseract55.dll,
│   │                              leptonica-1.85.0.dll, pdfium.dll
│   └── x86_64/                    stesse DLL, build x64
```

## Come si compone la pipeline (l'ordine conta)

1. **Orientamento** (PP-LCNet doc-ori): la pagina viene raddrizzata UNA
   volta, prima di tutto il resto.
2. **Layout** (PP-DocLayoutV3): regioni testo/tabella/figura/formula —
   per ENTRAMBI gli engine.
3. **Switch** (`policy.rs`): PP-OCRv6 o Tesseract. Scelta utente >
   auto-detect hardware.
4. **Riconoscimento**: Paddle det+rec, oppure Tesseract in-cage sui crop
   delle regioni. Tabelle: cell detection RT-DETR-L + OCR per-cella.
5. **Assemblaggio** in reading order + **Markdown** (GFM + LaTeX).

## Per compilare

```
cd df-ocr-switcher
cargo build --features tesseract-engine[,tables,searchable-pdf]
```

I path relativi `../df-ppocr-rs` e `../df-Tesseract/tesseract5-rs` sono
già giusti in questa disposizione (verificato: `cargo metadata` risolve).
A runtime servono:

- `ORT_DYLIB_PATH` → `native/<arch>/onnxruntime.dll` (ort è load-dynamic);
- `tesseract55.dll` + `leptonica-1.85.0.dll` accanto all'eseguibile
  (o nel PATH);
- i path modelli passati alla config (vedi esempi in
  `df-ocr-switcher/README.md` e `examples/`).

Trappola nota (vissuta): le librerie native NON capiscono i path Windows
verbatim `\\?\C:\…` — se i path arrivano da `canonicalize()`, spogliare
il prefisso prima di passarli a ort/Tesseract (in Edge: `strip_verbatim`).
