# RISULTATI — pdf-extractor-2-md

Stato delle misure al 2026-08-24. Questo documento racconta *come* si è
arrivati ai numeri, oltre ai numeri: ogni soglia e ogni scelta citata qui ha
la sua misura accanto nel codice o in `PLAN.md`.

## 1. Il metodo di misura

Tutte le misure usano la stessa verità: **il testo che pdfium estrae dal PDF
nativo digitale**, pagina per pagina (`benchmark/out/<doc>/page-NNN/verita.txt`,
118.337 parole su 659 pagine di 16 documenti reali dell'autore). Per il ramo
OCR la stessa pagina è rasterizzata a 200 DPI e degradata a 4 livelli con
Augraphy (rotazione 0,3-1° verificata empiricamente, JPEG, fotocopia, fax con
pieghe), così la verità resta perfettamente allineata e gratuita.

Tre metriche, sempre le stesse:

- **recall / precisione** sulle parole, a multiset (quanto contenuto si perde
  o si inventa);
- **ordine**: bigrammi della verità *le cui due parole sopravvivono
  nell'output* che restano adiacenti. Il condizionale è essenziale: senza,
  scartare una testata ripetuta verrebbe contato come disordine, e la metrica
  confonderebbe "ho riordinato male" con "ho scartato la furniture";
- **struttura**: heading emessi (`#`…`######`).

## 2. Ramo nativo — risultati

Tre configurazioni misurate sugli stessi 16 documenti (659 pagine):

| | recall | precisione | ordine | heading | tempo corpus |
|---|---|---|---|---|---|
| pdf-inspector (terzi, senza modifiche) | 93,9% | 96,7% | 93,5% | 1.523 | 7,3 s |
| **nostro, percorso geometrico** | **98,5%** | **98,7%** | **95,7%** | 768 | 11 s |
| **nostro + PP-DocLayoutV3** | 97,2% | **98,9%** | **97,5%** | 1.067 | 590 s |

Letture:

- il percorso **geometrico** (niente ONNX) supera pdf-inspector su tutte le
  metriche. La recall più alta del percorso col modello dipende dalla
  furniture: il modello la riconosce e la scarta (correttamente), la metrica
  la conta come persa;
- il **modello di layout** compra due cose: l'ordine sul multicolonna
  difficile (rivista: 98,2% contro 93,8%) e la semantica (heading affidabili,
  furniture, figure, caption). Costa ~0,9 s/pagina di inferenza CPU — 54×
  l'intero resto della pipeline;
- il caso peggiore di pdf-inspector (ROPOLL, 72,5% di recall) è un **falso
  positivo di tabella**: un paper a due colonne impaginato dentro una tabella
  pipe. Monito per la nostra Fase 3 restante.

### Come ci si è arrivati (le correzioni che hanno spostato i numeri)

1. **Loose bounds, non tight**: i box d'inchiostro inventavano spazi nei
   numeri (`2017/2394` → `201 7/2394`) e perdevano gli apostrofi.
2. **Righe tagliate sulla baseline** (`origin_y`), non sull'overlap dei box:
   i titoli a leading stretto si fondevano.
3. **Doppia evidenza spazio+gap per le parole**: i loose box si sovrappongono
   di ~0,5 pt, uno spazio vero da 0,23 em ne misura 0,07 — tutto `AI CNEL`
   usciva senza spazi.
4. **De-sillabazione anche su trattini non ASCII**: le colonne del Sole 24
   Ore sillabano con un glifo mappato su un carattere di controllo
   (precisione 92,7% → 96,1% su quel documento).
5. **Rilevazione di colonne** (ispirata a pdf-inspector, `src/columns.rs`):
   istogramma di proiezione, gutter = strisce sotto il 10% della striscia più
   affollata, validazione per righe ed estensione. Ordine del percorso
   geometrico 93,4% → 95,7% (AI CNEL +10,7, QUALITY +10,5, rivista +8,6).
   Applicata anche al percorso col modello (orfani + split delle regioni
   attraverso i gutter): **Δ 0,0%** — il modello disegna già una regione per
   colonna; lo split resta come rete di sicurezza.
6. **Heading senza modello** (`src/structure.rs`): outline prima (aggancia
   anche `3 Problem Setup` ↔ `Problem Setup`), poi tier tipografici presi per
   **frequenza** e non per grandezza — per grandezza, una rivista collassava
   537 heading su un unico livello. Da 0 a 768 heading sul percorso veloce.
7. **Figure croppate dal raster** (non estratte come oggetti): un grafico
   vettoriale e una foto escono allo stesso modo. Embed base64 o file PNG
   accanto al MD (`--images embed|files|skip`). Due bug trovati misurando:
   le regioni figura senza testo venivano scartate e le immagini fuori
   regione sparivano — la rivista passava da 40 a 180 figure estratte.

## 3. Integrità del documento (chk_defaced)

Il crate dell'autore gira su ogni documento prima dell'estrazione (parse lopdf
unico, 35-90 ms): font defacing → pagine instradate a OCR con motivo
`garbled`; testo nascosto → avvertimento, mai omissione. Contributi risaliti
al crate: allineamento lopdf 0.44, ingresso doc-level, guardia sulle legature
(su un documento reale eliminava 5 finding High spuri `ﬁ→f`). Un buco vero
chiuso in pipeline: una scansione con layer OCR *garbled* (font
`HiddenHorzOCR`, ToUnicode in PUA) ora viene rimandata ai pixel.

## 4. Ramo OCR — Tesseract contro PP-OCRv6 (medium/small/tiny)

Pipeline identica per tutti i motori: il motore produce le stesse `Line` del
ramo nativo, poi colonne, montaggio e Markdown sono lo stesso codice. Campione:
5 documenti × tutte le pagine × 4 livelli di degrado = 160 immagini, verità
nativa della stessa pagina, macchina scarica, un processo per motore (l'init
si paga una volta).

### I numeri (160 immagini: 40 pagine × 4 livelli, macchina scarica)

**Recall per livello di degrado** (contenuto letto):

| livello | tesseract | v6-medium | v6-small | v6-tiny |
|---|---|---|---|---|
| L0 raster pulito 200 dpi | 96,8% | **97,9%** | 97,7% | 96,1% |
| L1 lieve (rotazione 0,3-1° + JPEG) | **96,0%** | 71,8% ⚠ | 95,6% | 92,0% |
| L2 medio (fotocopia stanca) | 87,6% | 57,4% ⚠ | **91,5%** | 84,7% |
| L3 forte (fax + pieghe) | 20,9% | **31,7%** | 24,9% | 15,5% |

**Ordine** (bigrammi condizionali): stesso quadro — L0 96-98% per tutti,
L1-L2 il v6-small guida (95,6% / 91,5%), L3 inservibile per tutti (28-40%).
La precisione segue la recall entro 1-2 punti per ogni motore.

**Tempi per pagina** (secondi, ARM64, CPU):

| livello | tesseract | v6-medium | v6-small | v6-tiny |
|---|---|---|---|---|
| L0 | 8,3 | 16,3 | 4,3 | 1,1 |
| L1 | 9,9 | 13,6 | 3,6 | 1,0 |
| L2 | 14,3 | 13,1 | 3,5 | 1,0 |
| L3 | 20,6 | 10,8 | 2,6 | 0,9 |

### Le letture che contano

1. **v6-small è il motore giusto per questa pipeline**: migliore o quasi su
   ogni livello utile (L0-L2), 2-4× più veloce di Tesseract e 3-4× di medium.
   A parità di contenuto letto su L1, costa 3,6 s contro i 9,9 di Tesseract.
2. **v6-medium ha un difetto reale sui crop inclinati/compressi**: su L1-L2
   perde lettere *dentro* le parole («In lne d prcipo, la riura d sore» dove
   small legge perfettamente «In linea di principio, la fornitura di
   software»). Non è rumore: è sistematico, e small/tiny non lo mostrano.
   Prima di usare medium va indagato (sospetti: sensibilità del recognizer
   grande ai crop non raddrizzati; il canale Python di edito-ocr-v6 fa il
   crop prospettico e non mostra questo sintomo). Curiosamente medium è il
   *migliore* sul fax L3 — il modello grande regge il rumore estremo, non la
   geometria storta.
3. **Il tempo di Tesseract cresce col degrado** (8,3 → 20,6 s: più rumore =
   più lavoro per il suo segmentatore), quello dei motori ONNX no — anzi
   cala, perché trovano meno righe. In un servizio è una differenza di
   prevedibilità, non solo di media.
4. **L3 è oltre la portata di ogni motore senza preprocessing**: il fax con
   pieghe vuole deskew + denoise + binarizzazione prima dell'OCR (i passi ⓪
   e ①b che edito-ocr-v6 aveva pianificato e mai implementato). Il 20-32% di
   recall non è un motore che fallisce: è l'assenza di quel passo.
5. **Implicazione per la policy di switch (Fase 4)**: la regola ereditata
   «CPU pura → Tesseract» va rivista alla luce dei numeri — su questa
   macchina v6-small *su CPU* batte Tesseract in qualità e velocità insieme.
   Tesseract resta prezioso come secondo lettore (arbitrato, lingue senza
   modello) più che come motore primario.


## 5. Dove siamo e cosa resta

- Fasi 0-3 (nativo → Markdown) complete e misurate; Fase 4 ha i due motori e
  il benchmark; restano lo switch automatico per pagina (policy hardware +
  lingua), le tabelle dalle rules, liste/codice, la fusione per pagina
  (Fase 5) e l'arbitrato (Fase 6).
- Punto aperto dell'autore: i 3 finding non-legatura di chk_defaced su
  `2025_10_24` (14 pagine instradate a OCR).
