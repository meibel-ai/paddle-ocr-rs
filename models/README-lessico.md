# Materiale lessicale e modelli aggiuntivi (preparato il 2026-08-23)

Materiale procurato in anticipo per le Fasi 4-6 di `PLAN.md` (oracolo
lessicale dell'arbitrato + formule). Fonti e licenze verificate al download.

## hunspell/ — dizionari NON-GPL (utilizzabili senza vincoli copyleft forti)

Da `github.com/LibreOffice/dictionaries` (master). Dove il dizionario è
multi-licenza si sceglie l'opzione indicata.

| file          | lingua     | licenza scelta | note                          |
|---------------|------------|----------------|-------------------------------|
| `en_US.*`     | inglese    | MIT AND BSD    | SCOWL                         |
| `fr_FR.*`     | francese   | MPL-2.0        | Dicollecte, `fr_FR/dictionaries/fr` upstream |
| `es_ES.*`     | spagnolo   | MPL-1.1        | tri-licenza GPL-3/LGPL-3/MPL-1.1 |
| `pt_PT.*`     | portoghese | MPL-1.1        | tri-licenza GPL-2/LGPL-2.1/MPL-1.1 |

## hunspell-gpl/ — QUARANTENA licenze GPL (decisione da prendere)

**Non esistono dizionari hunspell non-GPL per italiano e tedesco** (verificato
2026-08-23: it_IT è GPL-3 puro; de_DE_frami è GPL-2/GPL-3). Scaricati qui in
cartella separata per confronto qualitativo in sviluppo. Il precedente in
edito-ocr-v6 (DECISIONS.md D15): ammessi come *dati* per uso via rete; per un
prodotto distribuito on-premise la questione si riapre.

| file       | licenza        |
|------------|----------------|
| `it_IT.*`  | GPL-3.0        |
| `de_DE.*`  | GPL-2/GPL-3 (variante frami) |

## morph/ — la risposta per italiano e tedesco: lessici full-form CC (2026-08-24)

La ricerca approfondita ha sciolto il nodo: esistono lessici di forme flesse
**non-GPL** per entrambe le lingue, ed è confermato che morph-it è
**dual-licensed** (si sceglie il ramo Creative Commons).

| file | forme | fonte | licenza scelta |
|---|---|---|---|
| `ita.forms` | 404.639 | Morph-it! 0.48 (Baroni & Zanchetta, Unibo) | **CC BY-SA 2.0** (dual con GPL2+: si usa il ramo CC) |
| `deu.forms` | 1.933.138 | DEMorphy `german-morph-dictionaries` (DuyguA) | **CC BY-SA 4.0** |
| `morph-it_048_utf8.txt` | sorgente | mirror github giodegas/morphit-lemmatizer | come sopra |

Correlato: `languagetool-org/german-pos-dict` (la stessa genealogia Morphy) è
anch'esso CC BY-SA 4.0, ma distribuisce solo binari Morfologik — DEMorphy dà
il testo in chiaro, per questo è la fonte scelta.

Divisione dei compiti nel `Lexicon` (`src/lexicon.rs`): i `.forms` rispondono
solo all'**esistenza** (entrano con rango infinito), le `wordlists/` ordinate
per frequenza restano l'unica fonte del **rilevamento lingua** — così due
milioni di forme rare non diluiscono la statistica. È la stessa separazione
hunspell/wordfreq che edito-ocr-v6 aveva misurato.

Nota CC BY-SA: attribuzione + share-alike valgono sui *dati* se ridistribuiti
(anche modificati); non impongono nulla al codice che li consulta. Per un
prodotto on-premise è una condizione molto più leggera della GPL-3 dei
dizionari hunspell it/de, che restano in quarantena.

## wordlists/ — l'ALTERNATIVA NON-GPL: Apache-2.0

Da `github.com/tesseract-ocr/langdata_lstm` (main), **Apache License 2.0**.
Una parola per riga, **ordinate per frequenza discendente** — quindi coprono
due usi:

1. **Oracolo di esistenza** per italiano e tedesco al posto dei dizionari GPL
   (nessuna morfologia hunspell, ma sono liste di forme flesse reali da
   corpus). Da confrontare in Fase 6 contro `hunspell-gpl/` sul corpus di
   correzioni: se la resa è vicina, si eliminano i GPL.
2. **Rilevamento lingua di pagina**: il rango nella lista è una stima di
   frequenza — sostituisce lo zipf medio di wordfreq usato da edito-ocr-v6
   (`arbitrate.py:252-290`, `punteggi_lingua`).

| file            | forme   |
|-----------------|---------|
| `ita.wordlist`  | 185.740 |
| `deu.wordlist`  |  89.075 |
| `eng.wordlist`  | ~ (scaricata) |
| `fra.wordlist`  | ~       |
| `spa.wordlist`  | ~       |
| `por.wordlist`  | ~       |

## formula/ — PP-FormulaNet_plus-M (ONNX)

Da HuggingFace `jinzhenj/PP-FormulaNet_plus-M_onnx` (terze parti, come in
edito-ocr-v6): `inference.onnx` (~594 MB) + `inference.yml`. Formule → LaTeX
(UniMERNet). In v6 era spento di default; qui disponibile per la Fase 6 se si
decide di non ripiegare sul crop-immagine.

## Il resto di models/

V. `README-edge-models.md` (nota originale di Edge): `paddleocr/` (v6
tiny/small/medium + layout + orientamento + tabelle + cls + latin),
`tesseract/tessdata/` (6 lingue EU + osd + script).
