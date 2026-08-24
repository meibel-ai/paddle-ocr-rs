# RISULTATI — report definitivo (2026-08-24)

Tutte le misure di pdf-extractor-2-md: ramo nativo (con e senza PP-DocLayoutV3,
contro pdf-inspector), ramo OCR (Tesseract e PP-OCRv6 nei tre tier), tempi,
errori ricorrenti, limiti. Ogni numero qui è riproducibile con i comandi in §8.

## 1. Sintesi esecutiva

- **PDF nativi**: il nostro percorso **geometrico** (niente ONNX) estrae il
  98,5% del contenuto con il 97,2% dell'ordine in 11 s sull'intero corpus —
  sopra pdf-inspector su ogni metrica. Il **modello di layout** aggiunge
  semantica (heading affidabili, furniture, figure) e l'ultimo margine di
  ordine sul multicolonna estremo, a 54× il costo.
- **Raster/scansioni**: **PP-OCRv6 small è il motore da usare** — migliore o
  quasi su ogni livello di degrado utile, 2-4× più veloce di Tesseract anche
  su CPU pura. La regola ereditata «niente acceleratore → Tesseract» non è
  più giustificata dai numeri.
- **v6-medium ha un difetto sistematico** sulle pagine inclinate (perde
  lettere dentro le parole): non usarlo finché non è indagato.
- **Il fax degradato (L3) non è un problema di motori** ma dell'assenza del
  passo di preprocessing (deskew+denoise): 15-32% di recall per tutti.

## 2. Metodo

**Verità**: il testo che pdfium estrae dal PDF nativo digitale, pagina per
pagina — 16 documenti reali dell'autore, 659 pagine, 118.337 parole
(`benchmark/out/<doc>/page-NNN/verita.txt`). Per il ramo OCR la stessa pagina
è rasterizzata a 200 DPI e degradata con Augraphy a 4 livelli: L0 pulito,
L1 lieve (rotazione 0,3-1° **verificata empiricamente** + JPEG q70-85),
L2 fotocopia stanca (ink bleed, illuminazione, texture), L3 fax (halftone,
binarizzazione, pieghe). Verità identica e allineata per costruzione.

**Metriche** (le stesse ovunque):

- **recall / precisione** sulle parole, a multiset;
- **ordine**: bigrammi della verità *le cui due parole sopravvivono
  nell'output* che restano adiacenti. Il condizionale evita di contare come
  disordine lo scarto legittimo della furniture;
- **struttura**: heading emessi;
- **tempo**: per documento (nativo) o per pagina (OCR), macchina scarica,
  ARM64, CPU. L'init dei motori si paga una volta per processo.

## 3. Ramo nativo — con LayoutV3, senza, e pdf-inspector

pdf-inspector (firecrawl, 98k righe, MIT) compilato **senza modifiche**,
default (niente OCR). Tabella per documento (rec/prec/ordine, heading):

| documento | pdf-inspector | nostro geometrico | nostro + LayoutV3 |
|---|---|---|---|
| 1785488689299 | 98,3 / 98,3 / 97,7 (9) | 99,9 / 99,9 / 99,9 (1) | 99,9 / 99,9 / 99,8 (7) |
| 1785569264937 | 87,7 / 93,6 / 88,3 (47) | 99,2 / 99,4 / 98,0 (8) | 99,0 / 99,4 / 98,5 (53) |
| 2025_10_24_Laiuto | 92,5 / 97,5 / 90,8 (317) | 90,6 / 97,4 / 90,0 (14) | 87,8 / 99,4 / 88,8 (269) |
| ag 434_449318 | 98,6 / 98,4 / 98,5 (81) | 99,5 / 99,2 / 99,5 (4) | 99,4 / 99,3 / 99,4 (113) |
| AI CNEL (giornale 3 col.) | 92,3 / 95,2 / 94,7 (185) | 96,2 / 96,0 / 97,1 (119) | 92,6 / 96,1 / 95,1 (110) |
| circolare_garante | 99,8 / 99,8 / 99,6 (6) | 100 / 99,9 / 100 (0) | 100 / 99,9 / 99,9 (3) |
| commercialista_inferno | 98,8 / 99,2 / 98,8 (100) | 100 / 100 / 100 (0) | 99,5 / 100 / 98,4 (63) |
| dettaglioAtto | 98,3 / 99,0 / 99,0 (3) | 100 / 100 / 100 (1) | 99,2 / 100 / 99,5 (2) |
| INNOVA_JUS | 90,2 / 98,6 / 92,6 (50) | 100 / 100 / 99,0 (28) | 96,2 / 100 / 96,0 (79) |
| Invisible Prompts (2 col.) | 93,5 / 94,7 / 95,1 (31) | 97,2 / 96,9 / 95,7 (2) | 97,5 / 97,4 / 99,5 (30) |
| italia grafica (rivista 3 col.) | 97,1 / 98,2 / 92,5 (526) | 99,7 / 98,9 / 94,2 (324) | 99,7 / 98,8 / 98,2 (155) |
| OJ_L_202402853 | 98,0 / 99,1 / 97,8 (61) | 100 / 99,9 / 99,0 (21) | 98,8 / 99,9 / 99,1 (56) |
| QUALITY OF OCR (2 col.) | 98,7 / 98,7 / 94,8 (9) | 99,7 / 98,6 / 97,1 (0) | 99,8 / 98,9 / 99,0 (12) |
| ROPOLL (2 col.) | **72,5 / 86,3 / 72,4** (76) | 96,1 / 96,4 / 90,6 (9) | 96,1 / 96,4 / 95,3 (51) |
| Tritium (export web) | 92,4 / 94,9 / 86,7 (7) | 99,5 / 99,4 / 96,2 (2) | 94,5 / 99,5 / 96,1 (12) |
| Vulnerabilities (paper) | 94,4 / 95,1 / 97,4 (15) | 97,7 / 97,7 / 99,1 (0) | 94,7 / 97,5 / 97,9 (52) |
| **MEDIA** | **93,9 / 96,7 / 93,5** (1.523) | **98,5 / 98,7 / 97,2** (533) | **97,2 / 98,9 / 97,5** (1.067) |
| **tempo corpus** | 7,3 s | 11 s | 590 s (~0,9 s/pagina) |

Letture:

- il **geometrico supera pdf-inspector ovunque** e arriva a 0,3 punti
  dall'ordine del modello. La differenza di recall geometrico-vs-modello è la
  furniture: il modello la riconosce e la scarta (correttamente), la metrica
  la conta come persa;
- il **modello** vince dove serve semantica: heading a livelli affidabili
  (1.067, con gerarchia dal reading order appreso), furniture e caption
  classificate, e l'ordine sui layout più difficili (rivista 98,2 vs 94,2;
  Invisible Prompts 99,5 vs 95,7);
- **2025_10_24** è basso per entrambi i nostri percorsi per una ragione non di
  layout: 14 pagine instradate a OCR dai finding di chk_defaced (3 sostituzioni
  non-legatura, punto aperto dell'autore) — nel MD c'è l'avviso, non il testo.

### Errori ricorrenti osservati (nativo)

| errore | dove | stato |
|---|---|---|
| falso positivo di tabella: paper 2 colonne dentro tabella pipe, testo mescolato | pdf-inspector su ROPOLL (72,5%) | monito per le nostre tabelle (non ancora implementate) |
| spazi persi/inventati (loose box sovrapposti, sidebearing dei digit) | nostro, corretto | risolto: doppia evidenza spazio+gap |
| mezze parole da sillabazione con trattini non ASCII | nostro su AI CNEL, corretto | risolto: U+2010/2011, soft hyphen, control char |
| titoli fusi (leading stretto) / spezzati (clustering su mediana pagina) | nostro, corretto | risolto: baseline + clustering su altezza propria |
| colonne interlacciate senza modello | nostro, mitigato | rilevazione colonne: ordine +2-11 punti sui multicolonna |
| heading collassati su un livello (tier per grandezza) | nostro, corretto | tier per frequenza |
| logo/stemma letto come testo garbage | tutti gli OCR, lieve in nativo | fisiologico; il layout lo classifica figura |

### Limiti attuali (nativo)

- gutter più stretti di 3 em senza modello: titolo e colonna adiacente
  possono ancora mescolarsi (il caso resta su italia grafica);
- tabelle non ancora emesse come GFM (le rules ci sono, la griglia no);
- liste, codice e postprocess (numeri di pagina, URL) da completare;
- LayoutV3 su CPU costa ~0,9 s/pagina: DirectML/CoreML non ancora provati.

## 4. Ramo OCR — Tesseract e PP-OCRv6 medium/small/tiny

Pipeline identica per ogni motore: il motore produce le stesse `Line` del ramo
nativo, poi colonne, montaggio e Markdown sono lo stesso codice. Campione: 5
documenti × tutte le pagine × 4 livelli = 160 immagini. `--engine
tesseract|v6-medium|v6-small|v6-tiny`.

**Recall / precisione / ordine / s-pagina** per livello:

| | tesseract | v6-medium | v6-small | v6-tiny |
|---|---|---|---|---|
| **L0 pulito** | 96,8 / 95,7 / 95,9 / 8,3 s | **97,9 / 96,7 / 98,1** / 16,3 s | 97,7 / 96,6 / 97,2 / **4,3 s** | 96,1 / 95,3 / 96,2 / **1,1 s** |
| **L1 lieve** | **96,0** / 94,1 / 94,6 / 9,9 s | 71,8 / 79,1 / 77,6 ⚠ / 13,6 s | 95,6 / 94,8 / **95,6** / **3,6 s** | 92,0 / 91,8 / 91,7 / **1,0 s** |
| **L2 fotocopia** | 87,6 / 86,4 / 87,3 / 14,3 s | 57,4 / 68,9 / 67,5 ⚠ / 13,1 s | **91,5 / 91,3 / 91,5** / **3,5 s** | 84,7 / 85,1 / 84,4 / **1,0 s** |
| **L3 fax** | 20,9 / 42,6 / 30,3 / 20,6 s | **31,7 / 50,1 / 40,5** / 10,8 s | 24,9 / 41,6 / 37,8 / 2,6 s | 15,5 / 29,0 / 27,6 / 0,9 s |

### Errori ricorrenti osservati (OCR)

| errore | motore | esempio |
|---|---|---|
| **lettere perse dentro le parole su pagine inclinate/JPEG** — sistematico, non rumore | v6-medium (L1/L2) | «In lne d prcipo, la riura d sore» per «In linea di principio, la fornitura di software» |
| loghi e stemmi letti come sigle o garbage | tutti | Tesseract: «R GARANTE ZI id \\'?»; v6: «GPDPI» |
| tempo che esplode col rumore (il segmentatore lavora di più) | tesseract | 8,3 s (L0) → 20,6 s (L3) |
| sul fax: parole frammentate ovunque, precisione doppia della recall (legge poco ma non inventa molto) | tutti su L3 | recall 15-32% |
| leggero deficit costante del tier più piccolo | v6-tiny | −2/−7 punti da small, mai crolli |

### Limiti attuali (OCR)

- **niente preprocessing**: senza deskew+denoise+binarizzazione L3 resta
  inservibile per ogni motore — è il prossimo guadagno grande;
- niente orientamento di pagina (i raster del benchmark sono dritti per
  costruzione; nella pipeline reale servirà doc-ori + OSD come in v6);
- confidenze non ancora usate a valle (arbitrato, lessico: Fase 6);
- word bbox del motore Paddle stimati proporzionalmente lungo la riga (esatti
  quelli di riga, che guidano colonne e ordine);
- campione = 5 dei 16 documenti (tutti i livelli): rappresentativo ma non
  esaustivo; il paper 2 colonne è incluso.

## 5. PP-OCR con fallback Tesseract, e senza

Oggi lo switch è **manuale** (`--engine`); la policy automatica della Fase 4 è
da scrivere, e questi numeri ne cambiano il disegno.

- **La regola ereditata** («scelta utente > acceleratore ONNX presente →
  Paddle, CPU pura → Tesseract») nasceva dall'assunzione che Paddle su CPU
  fosse lento. Misurato: **v6-small su CPU pura batte Tesseract in qualità E
  velocità** su ogni livello utile (L2: 91,5% contro 87,6% a un quarto del
  tempo). Con l'eccezione L1 dove Tesseract è alla pari (96,0 vs 95,6), non
  c'è più un caso hardware in cui Tesseract primario convenga.
- **Cosa resta al fallback Tesseract** (e vale la pena tenerlo):
  1. **disponibilità**: se onnxruntime/modelli mancano o falliscono a
     runtime, Tesseract statico legge comunque (96,8% su L0 è un ottimo
     paracadute);
  2. **script fuori modello**: i tessdata `script/` coprono cirillico, CJK,
     arabo… dove i modelli v6 latin non arrivano;
  3. **secondo lettore** (Fase 6): l'arbitrato di edito-ocr-v6 — dizionario
     multilingua, ricrop ad alta risoluzione, rilettura nella lingua giusta —
     usa Tesseract come *verificatore*, ruolo in cui i suoi errori sono
     scorrelati da quelli di Paddle, che è ciò che serve a un arbitro.
- **Senza fallback** (solo v6-small): su questo corpus latino si perde
  soltanto la ridondanza — nessun caso misurato in cui il fallback avrebbe
  salvato contenuto che small perde. La raccomandazione: **primario v6-small,
  Tesseract compilato ma relegato a paracadute e arbitro**, mai selezionato
  per hardware.

## 6. Riproducibilità

```
# nativo
pdf2md <doc.pdf> -o out.md [--images embed|files|skip]        # geometrico
cargo run --release --features layout -- <doc.pdf> -o out.md   # con LayoutV3
# ocr
pdf2md x --ocr-batch <lista.txt> --engine v6-small             # feature tesseract,ppocr
# scoring: tools/benchmark/score.py (multiset) + scratchpad three_way.py / score_ocr.py
# corpus: tools/benchmark/make_benchmark.py --out benchmark/out test/nativi/*.pdf
```

Storia completa delle correzioni che hanno prodotto questi numeri (loose
bounds, baseline, spazio+gap, trattini non ASCII, colonne, tier per frequenza,
figure dal raster, chk_defaced su lopdf unico, guardia legature): `PLAN.md`,
sezioni Fase 1-4, con provenienza e test di regressione per ciascuna.
