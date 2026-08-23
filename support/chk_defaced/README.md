# chk_defaced

**Detect tampered documents at the PDF/DOCX/HTML → LLM ingestion boundary.** Two dual abuses exploit the
gap between what a human *sees* and what a machine *extracts* — the text that downstream pipelines
(search, RAG, LLM ingestion, e-discovery) actually consume:

- **Font defacement** — the extracted text *differs* from the glyphs drawn (a page renders "Maryland" but
  extracts "Delaware"), via a tampered `ToUnicode` map, Private-Use-Area remapping, or semantic glyph
  replacement.
- **Invisible / cloaked text** — text *present* in the extraction (read by an LLM/RAG) but **not visible
  to a human**: white-on-white, sub-visible size, invisible render mode, off-page or occluded — the
  classic **prompt-injection** and anti-ATS keyword-stuffing vector.

> Background and motivation — Dario Finardi,
> [**What You See Is Not What Your AI Reads**](https://dariofinardi.it/what-you-see-is-not-what-your-ai-reads-c3fed388d3bc):
> *"Font remapping is an old trick. AI document pipelines just made it dangerous again."*
> The document **lies**, and every component downstream faithfully passes the lie along. The attack
> family is named and demonstrated by the **LegalQuants RED TEAM** ("Noroboto"). See
> [Background & references](#background--references) for the full reading list.

`chk_defaced` **scans `.pdf` / `.docx` / `.html`** and returns a self-contained, interoperable report so
a consumer can **evaluate the source**, not just spot the tampering. Each report carries:

- an explicit, machine-readable **verdict** — `assessment: { ok, defaced, hidden_text, max_severity }`;
- the **findings** (rule, severity, category, message, location) with the **suspect text** quoted and,
  for PDF, its **page** number;
- the document's **provenance** — `metadata` read from the container: author, `last_modified_by`,
  authoring tool, producer, company, revision, dates (an unusual producer or a mismatched last editor is
  itself a lead).

The core checks are **deterministic** — outline-level semantic-replacement detection (variant A3) and a
local-background **painter model** for invisible text — and run with **no OCR and no headless browser**.
An optional **OCR tier** (feature-flagged) renders the document and reads it back to *confirm* a doubt
and *recover* what was altered or hidden (see [OCR fallback](#ocr-fallback-optional) and
[Invisible / cloaked text](#invisible--cloaked-text-prompt-injection-vector)). A separate
`build-registry` command indexes the official cmaps of your system fonts as the ground truth for the
canonical tamper check.

The core is **pure Rust** and **offline** by default; OCR/rendering are opt-in.

---

## Build

```sh
cargo build --release
```

No native libraries are required (font parsing, PDF/ZIP/XML/HTML are all pure-Rust crates).

## Usage

### 1. Build the system-font cmap index

```sh
# Defaults to the OS font directory (e.g. C:\Windows\Fonts) and writes fonts-index.json
chk_defaced build-registry --out fonts-index.json

# Smaller index (identity + hashes only, no full cmaps)
chk_defaced build-registry --slim --out fonts-index.json
```

Each entry records the font identity (family, subfamily, PostScript name, version, copyright,
manufacturer), `num_glyphs`, `units_per_em`, the **file SHA-256**, the **cmap SHA-256**, the full
**cmap** as `(codepoint, glyph_id)` pairs, and per-codepoint **Latin outline hashes** (kept even with
`--slim`) — the ground truth for the deterministic canonical tamper check below.

### 2. Scan documents

```sh
# Human-readable report; pass the index to identify modified fonts
chk_defaced scan contract.pdf report.docx page.html --registry fonts-index.json

# Machine-readable
chk_defaced scan contract.pdf --registry fonts-index.json --format-out json
```

The process exits non-zero if any finding is **High** or worse (handy as a CI gate).

**The report is a complete, interoperable envelope** — a verdict, the source's provenance, and the
findings — so a consumer can *evaluate the source*, not just spot the tampering:

```jsonc
{
  "file": "contract.docx", "format": "docx", "fonts_examined": 22,
  "assessment": { "ok": false, "defaced": true, "hidden_text": false, "max_severity": "High" },
  "metadata":   { "title": "Mutual Non-Disclosure Agreement", "author": "Jeremy Pomeroy",
                  "last_modified_by": "Andrew Miller", "creator_tool": "Microsoft Office Word",
                  "revision": "13", "created": "2026-05-18T09:33:00Z", "modified": "2026-05-18T21:04:00Z" },
  "findings":   [ /* rule, severity, category, message, location, confidence */ ],
  "phrases":    [ /* extracted → presumed → ocr */ ],
  "verdict":    "Confirmed"
}
```

- **`assessment`** — the explicit document-level verdict, computed from the findings so you don't have to:
  `ok` (no High+), `defaced` (High+ font tampering), `hidden_text` (Medium+ invisible/cloaked text),
  `max_severity`. The human output prints it as `Verdict: KO — DEFACED (font tampering)` / `OK …`.
- **`metadata`** — provenance read from the container (PDF `/Info`: `Creator`/`Producer`/dates/version;
  DOCX `core.xml` + `app.xml`: `author`/`last_modified_by`/`Application`/`Company`/`revision`/dates). An
  unusual producer, or a `last_modified_by` that differs from the author on a tampered contract, is itself
  a lead. Absent fields are omitted.

When a **semantic replacement** is detected, the report lists the **affected sentences** (only those a
substitution changes, for easy side-by-side comparison), each with the `extracted` text (what an
extractor / RAG reads) and the `presumed` rendered text (substitutions applied). With `--ocr` on a PDF
(an `ocr-atlas` build), an `ocr` ground-truth reading is added — it resolves which of the two is actually
on the page:

```
  Affected phrases (extracted → presumed → ocr):
    read:     MUTUAL NON-DISCLOSURE AGREEMENT
    presumed: MUTUAR ROR-MISCROSURE AGREEMERT
    ocr:      MUTUAL NON-DISCLOSURE AGREEMENT
```

In `--format-out json` this is the `phrases` array (objects with `extracted`, `presumed`, and optionally
`ocr`). The presumed reconstruction is best-effort (the drawn letter is inferred by frequency); the OCR
field — when present — is the ground truth.

**Verdict (two independent opinions).** A deterministic glyph collision is a high-recall *structural*
signal; on its own it cannot tell whether the rigged glyph is actually used on the visible text. So the
report carries a separate `verdict` that a **mandatory render-level OCR** sets when `--ocr` runs on a
PDF (`ocr-atlas` build):

| Verdict | Meaning | Effect |
|---|---|---|
| **Confirmed** | OCR finds a real rendered↔extracted word substitution (e.g. `'delaware'→'maryland'`) | stays High; a `PDF.RENDER_DIVERGENCE_CONFIRMED` finding lists the real swaps |
| **Refuted** | the rendered page matches the extracted text — a loaded-but-unfired rig | the `*.GLYPH_SEMANTIC_REPLACEMENT` findings are downgraded to `Info` (document not flagged) |
| **Unconfirmed** | OCR was not run, or was inconclusive | the deterministic High findings stand (run `--ocr` to confirm/refute) |

The verdict can also be reached **deterministically, without OCR**, when you pass a `--registry`: the
**canonical check** compares each embedded glyph's outline to the registry's canonical outline for that
codepoint. If a glyph matches a *different* canonical letter, the font is lying — that sets `Confirmed`
and the correct substitution direction (a `PDF.CANONICAL_TAMPER_CONFIRMED` finding lists the swaps),
no OCR needed. When the embedded font's family is not in the registry (or a glyph matches nothing), the
check is inconclusive → the render-level OCR fallback (`--ocr`) resolves the doubt.

With an `ocr-specimen` build, add **`--ocr`** to escalate to specimen-OCR on documents where the
deterministic pass finds no semantic replacement (the no-anchor case). It is skipped automatically on
already-flagged files, degrades gracefully if Tesseract/tessdata is unavailable, and takes `--ocr-lang`
(default `ita+eng`):

```sh
chk_defaced scan contract.pdf --ocr                 # ita+eng
chk_defaced scan vertrag.pdf  --ocr --ocr-lang deu+eng
```

---

## What it detects (v1)

| Rule | Severity | What it means |
|---|---|---|
| `PDF.GLYPH_SEMANTIC_REPLACEMENT` / `DOCX.GLYPH_SEMANTIC_REPLACEMENT` | High | **Semantic replacement (variant A3).** An embedded glyph that *draws* one letter is mapped from a *different* extracted character — the reader sees one word, extraction yields another, both lexically valid. Detected **deterministically** by cross-referencing each glyph's outline against the letters that reach it (no OCR, no rendering). |
| `UNICODE.PUA` | High | Private Use Area characters in the extracted text — unreadable without the font cmap (typical obfuscation). |
| `UNICODE.ZERO_WIDTH` / `UNICODE.BIDI_OVERRIDE` | Medium | Invisible / direction-reversing characters in the extracted text. |
| `PDF.TOUNICODE_GARBLED` | High | A PDF `ToUnicode` map sends codes to the PUA or to invisible/zero-width codepoints: glyphs render fine, extraction yields gibberish. |
| `PDF.CUSTOM_DIFFERENCES` | Info | Custom `/Encoding /Differences` — legitimate, but the vector for glyph↔codepoint redirects. |
| `PDF.MANY_SUBSETS` | Medium | One family split into many subset objects (possible per-page/per-run dynamic subsetting). |
| `FONT.PUA_CMAP` | High | A non-symbol embedded font whose internal cmap is mostly PUA — likely obfuscation. |
| `PDF.CANONICAL_TAMPER_CONFIRMED` / `DOCX.CANONICAL_TAMPER_CONFIRMED` | High | Deterministic confirmation (with `--registry`): an embedded glyph's outline matches a *different* canonical letter — the tamper and its direction, no OCR. |
| `PDF.RENDER_DIVERGENCE_CONFIRMED` / `DOCX.RENDER_DIVERGENCE_CONFIRMED` | High | `--ocr` render confirms the rendered text diverges from the extracted (lists the real swaps, e.g. `'delaware'→'maryland'`). |
| **Invisible / cloaked text** — [details](#invisible--cloaked-text-prompt-injection-vector) | | |
| `DOCX.INVISIBLE_TEXT_COLOR` / `DOCX.TINY_TEXT` | High | A near-white (guarded by `w:shd`/`w:highlight`) or sub-visible (< 4 pt) run — the finding **quotes the injected text**. |
| `PDF.INVISIBLE_TEXT_COLOR` / `PDF.TINY_TEXT` / `PDF.INVISIBLE_RENDER_MODE` | Medium (candidate) | Painter-model signals: text ≈ its local background, sub-visible size (< 1.5 pt), or invisible render mode (`Tr 3/7`) — confirmed to High by `--ocr`. |
| `PDF.OCR_TEXT_LAYER` | Info | **Searchable scan, not hidden content.** A page whose images cover ≥ 70 % of it and whose text is ≥ 70 % invisible *and* spread across the page is a scanned page with an invisible OCR text layer — invisible **by design** so the scan is searchable. Its invisible render-mode text is reclassified from `PDF.INVISIBLE_RENDER_MODE` to this benign note, so a plain scanned+OCR PDF is not flagged as hiding text. |
| `PDF.HIDDEN_TEXT_CONFIRMED` / `DOCX.HIDDEN_TEXT_CONFIRMED` | High | `--ocr` render confirms words present in the extract but **absent from the render** — and **lists the recovered hidden words**. |
| `DOCX.HIDDEN_VANISH` | Medium | `w:vanish` runs: text present in the extract but not visible. |
| `DOCX.FONT_UNREADABLE` | Low | An obfuscated `.odttf` embedded font that could not be de-obfuscated/parsed. |
| `HTML.HIDDEN_STYLE` | Medium | CSS cloaking (`display:none`, `opacity:0`, off-canvas, `font-size:0`, …) on text. |
| `HTML.CSS_CONTENT_INJECTION` | Low | `content:` declarations inject visible text absent from the extracted DOM. |
| `HTML.{REMOTE,DATAURI}_WEBFONT`, `HTML.FONTFACE_COUNT` | Info | `@font-face` inventory; local fonts are verified, remote/data ones are only noted. |

DOCX obfuscated embedded fonts (`word/fonts/*.odttf`) are de-obfuscated with the OOXML `fontKey` GUID
before parsing.

---

## Character coverage (v1.0)

The semantic-replacement outline cross-reference (`*.GLYPH_SEMANTIC_REPLACEMENT`) is **scoped to the
Latin script** in v1.0. A character is a candidate iff `char::is_alphabetic()` **and** its Unicode
script is Latin; everything else is simply not analyzed (no findings, no false positives).

**✅ Checked — Latin-script letters** (case-folded to lowercase):

| Block | Examples |
|---|---|
| Basic Latin | `a–z`, `A–Z` |
| Latin-1 Supplement | `à á â ã ä å æ ç è é ê ë ì í î ï ñ ò ó ô õ ö ø ù ú û ü ý ÿ`, `ð þ ß` |
| Latin Extended-A (Central/Eastern Europe) | `ā ă ą ć č ď đ ē ę ě ğ ı ł ń ň ő œ ŕ ř ś š ť ů ű ź ż ž` |
| Latin Extended-B · IPA · Extended Additional (incl. Vietnamese) | `ƀ ɖ ɡ ơ ư ạ ẽ ị ọ ụ …` |

**🚫 Excluded — not analyzed (out of v1.0 scope):**

| Class | Examples |
|---|---|
| Greek / Coptic | `α β γ δ ε … ω` |
| Cyrillic | `а б в г д … я`, `ӏ` |
| Arabic (+ presentation forms) | `ا ب ت ث …`, `ﺁ ﺂ ﺃ …` |
| Hebrew | `א ב ג ד …` |
| CJK & Kana & Hangul | `中 文`, `あ い`, `ア イ`, `한 글` |
| Armenian, Georgian, Devanagari, Thai, … | `ա բ`, `ა ბ`, `अ आ`, `ก ข` |
| Non-letters | digits `0–9`, punctuation, math/currency symbols, emoji, whitespace |

**Analyzed but never flagged as tampering** — within the Latin set, a glyph shared between two
codepoints is *legitimate glyph-sharing*, not a finding, when they are: the same letter (or case
variant); a **TR39 confusable** pair (homoglyphs) or the `i`/`l` pair; or **compatibility-equivalent**
via NFKD — typographic ligatures (`ﬀ ﬁ ﬂ ﬃ ﬄ` → their letters) and precomposed↔decomposed accents.

**PUA/garbled checks** additionally skip fonts that map glyphs to the Private Use Area *legitimately*:
math fonts (Computer Modern / AMS — `CMEX CMSY CMMI MSAM MSBM`, `stmary`, `wasy`) and symbol/dingbat
fonts (`Symbol`, `Wingdings`, `Webdings`, `Marlett`, …).

Finally, Latin letters that fold to the **same ASCII base** (`ð`/`đ`/`ɖ` → `d`, `ø` → `o`) are
visually-confusable variants of one letter, so a shared glyph between them is not flagged either; a
genuine swap (`m` vs `d`) folds differently and stays flagged. The deterministic scan is **0 false
positives** across the whole 32-document evaluation corpus.

---

## OCR fallback (optional)

The deterministic outline checks catch semantic replacement (A3) without rendering. For the residual
cases they cannot reach — fonts whose outlines are unreadable (Type3, bare CFF), or attacks that only
survive a visual comparison — there is an **opt-in OCR-atlas** escalation: it rasterizes the page with
`pdfium`, OCRs it with **Tesseract 5.5 + Leptonica 1.85**, and compares what is *seen* with what is
*extracted*. It is OFF by default and lives behind feature flags:

```sh
# Pulls the native OCR backend (tesseract5-rs) + pdfium rasterization
cargo build --release --features ocr-atlas
```

The native Tesseract build is described in `TESSERACT-BUILD-HOWTO.md`. Language models
(`*.traineddata`) live in the per-arch cache `…/tesseract-rs/<arch>/<mode>/tessdata/`.

### Specimen-OCR (`ocr-specimen`) — closing the no-anchor gap

The outline cross-reference needs the *true* letter to appear somewhere un-tampered (an "honest anchor")
to form a collision. A **fully custom font that remaps every code 1:1, with no honest occurrence**, forms
no collision and slips through. The **specimen-OCR** escalation fixes that: it rasterizes each embedded
glyph **straight from its outline** (pure-Rust `tiny-skia`, no pdfium), OCRs it to recover *what the glyph
actually draws*, and compares that to the character the document claims to extract — an independent ground
truth that needs no anchor.

```sh
cargo run --example specimen --features ocr-specimen -- contract.pdf report.docx
```

It is **high-recall** (validated: 5/5 planted no-anchor lies caught). Two filters keep precision in check:
ligature codepoints (which draw several letters, so single-letter OCR can never match) are excluded, and a
**per-word OCR confidence gate** drops shaky reads — genuine drawn letters score ≥80 while the look-alike
confusions (`c/e`, `s/f`, `ı/l`, `a/d`) score ≤47. Together these took a real LaTeX paper from **8 false
positives to 0** with the true positives unchanged. The residual cost is recall: a glyph whose isolated
shape OCRs below the gate (e.g. a double-story `g`) is skipped, not guessed. Still an opt-in escalation for
the cases the deterministic detectors can't decide — **not** the default path.

**OCR language** — the example tools default to **`ita+eng`** and read the **`OCR_LANG`** environment
variable (Tesseract's `+`-joined form). Install the `*.traineddata` you need in the tessdata dir, then:

```sh
# default: Italian + English
cargo run --release --features ocr-atlas --example atlas -- contract.pdf

# override (any installed languages, e.g. German + English)
OCR_LANG="deu+eng" cargo run --release --features ocr-atlas --example atlas -- vertrag.pdf
```

### DOCX render-OCR (`render-ocr`) — catching the *positional* attack

A DOCX can carry the semantic replacement **per position**: the word *Delaware* is split across several
single-glyph embedded font subsets, each of which redraws its one letter, so the visible word becomes
*Maryland* while the extracted `w:t` text still reads *Delaware*. A global "char → char" substitution map
cannot express that — only **what a real engine actually paints** can recover it. And the engine has to be
a faithful one: DOCX→PDF converters that re-shape glyphs (Skia, Typst) *launder* the attack back to clean
text; a browser/webview, applying each embedded `@font-face` cmap verbatim, does not.

The **render-OCR** fallback does exactly that, entirely in-process: it rebuilds the DOCX as a
self-contained HTML page (each `.odttf` de-obfuscated and inlined as a base64 `@font-face`, every run
mapped to its font), renders it in a **Tauri webview** (`wry`/`tao` → WebView2 / WKWebView / WebKitGTK),
captures the off-screen window, OCRs it, and compares the rendered text with the extracted text. A real,
deliberate word substitution between the two yields a **Confirmed** verdict; a render that matches the
extract **Refutes** the collision (the rig is present but unused); anything in between stays
**Unconfirmed**.

```sh
# Pulls the in-process webview (wry/tao) + the native OCR backend
cargo build --release --features render-ocr

# --ocr now also confirms/refutes DOCX collisions by rendering + OCR
chk_defaced scan contract.docx --ocr --ocr-lang eng
```

```text
DOCX.RENDER_DIVERGENCE_CONFIRMED — OCR of the webview render confirms the rendered text
diverges from the extracted text: 'delaware'→'maryland'
Verdict: Confirmed
```

No external process and **no network**: the HTML is fed to the webview over an in-memory custom protocol
(no temp file, **no TCP port** — so a busy port can never block a scan), the window is rendered off-screen
and captured by handle, and the verdict comes from OCR alone. It is cross-platform (the same code drives
WebView2 on Windows, WKWebView on macOS, WebKitGTK on Linux). Note that the deterministic `presumed` text
shown for a *positional* attack is intentionally approximate — its global map over-applies to honest
words; the `DOCX.RENDER_DIVERGENCE_CONFIRMED` finding and the per-phrase `ocr:` line are the authoritative
output.

The tool is **defensive**: OCR returns plain glyph text only — nothing is ever fed to a model, and
there is no network access at scan time.

---

## Invisible / cloaked text (prompt-injection vector)

Font defacement makes the *extracted* text differ from the *drawn* glyph. The **inverse** abuse is text
that is present in the extraction (so an LLM/RAG reads it) but **not visible to a human** — used for
prompt injection (*"ignore all previous instructions…"*), ATS keyword-stuffing and hidden clauses. This
is a distinct threat model, handled in `visibility.rs` (`Category::HiddenContent`). DOCX checks read
explicit run properties (reliable → `High`); the PDF checks come from an approximate page model, so they
are **candidates** (`Medium` — "confirm with render-OCR", which raises a confirmed one to `High`):

| Vector | PDF | DOCX |
|---|---|---|
| **Hidden attribute** | — | `w:vanish` → `DOCX.HIDDEN_VANISH` |
| **Text colour ≈ background** | `PDF.INVISIBLE_TEXT_COLOR` — a **painter model** (graphics-state stack, CTM, painted fills as z-ordered backgrounds) compares the run's colour to its *actual local background* | `DOCX.INVISIBLE_TEXT_COLOR` (near-white, guarded by `w:shd`/`w:highlight`) |
| **Sub-visible font size** | `PDF.TINY_TEXT` (effective height < 1.5 pt) | `DOCX.TINY_TEXT` (< 4 pt) |
| **Invisible render mode** | `PDF.INVISIBLE_RENDER_MODE` (Tr 3/7) | — |
| **Off-page / clipped** | *left to render-OCR — see below* | — |
| **CSS cloaking** | — (HTML: `HTML.HIDDEN_STYLE`) | — |

**The colour check compares against the *actual* local background, not an assumed white page.** The
painter model records painted rectangles/images (colour + bbox, in z-order); each text run is judged
against the top-most fill under it. So white text on a **coloured header is visible → not flagged**,
while white-on-white *and* blue-on-blue camouflage are. Full-page fills are ignored (a whole-page colour
matching the text is a geometry artifact, not a box behind the text). On the real corpus this correctly
surfaces genuine invisible text — e.g. the white production stamps embedded on every page of some US
public-law PDFs — as `Medium` candidates (true, if usually benign; the render-OCR / a human judges
intent).

**Off-page / clipped detection is deliberately *not* deterministic.** An accurate on-page position needs
a complete nested-CTM + Form-XObject + clip-path geometry engine; a partial interpreter mis-places text
on real PDFs (it produced spurious off-page candidates). Since pdfium renders only the visible page, the
render-OCR pass catches off-page / clipped reliably instead.

**Render-OCR confirmation (`--ocr`).** When a candidate exists, the render-OCR pass renders the document
(pdfium for PDF, the webview for DOCX — the DOCX render applies each run's colour and size, so hidden
runs stay hidden) and computes the **complementary diff**: words present in the extracted text but
**absent** from the OCR of the render are invisible to a human. A non-trivial set of such words →
`PDF.HIDDEN_TEXT_CONFIRMED` / `DOCX.HIDDEN_TEXT_CONFIRMED` (`High`), and the finding **lists the recovered
hidden words** — so the report shows *what* was hidden, not just that something was. This works
regardless of *how* the text was hidden (colour, size, render mode, occlusion, clip, off-page), because a
faithful renderer composes the page exactly as a human sees it. (NB: OCR misses of genuinely visible
words are the residual false-positive risk, bounded by the ≥4-alphabetic-word threshold.)

---

## Honest limitations (and roadmap)

This v1 is deliberately **conservative** to avoid false positives:

- A font embedded in a PDF/DOCX is almost always a **subset**: its name table and internal cmap may be
  legitimately stripped, and glyph ids are renumbered. So a bare cmap-hash mismatch against the
  registry does **not** prove tampering — `chk_defaced` does **not** flag it. Semantic replacement is
  instead caught by the **outline cross-reference** (`*.GLYPH_SEMANTIC_REPLACEMENT`): the same drawn
  outline reachable from two different extracted letters is the tell, checked deterministically for both
  PDF and DOCX. To keep that precise on real documents, a collision is **not** flagged when the two
  letters are legitimate glyph-sharing rather than tampering: TR39 confusables, **different scripts**
  (Latin `b` / Greek `β` / Cyrillic `в`), or **compatibility-equivalent** encodings (Arabic base ↔
  presentation forms, via NFKD). The ToUnicode arm of the check runs only for **Type0/CID** fonts, where
  a content-stream code is actually a glyph id (a simple font routes code → glyph through `/Encoding`).
- **v1.0 is scoped to the Latin script.** The outline cross-reference only considers Latin letters
  (basic + accented + extended — all European languages); other scripts (Greek/Cyrillic/Arabic/CJK) are
  not analyzed, so they neither raise findings nor false positives.
- **False-positive rate is measured, not assumed.** On a 32-document corpus of real public PDFs/DOCX
  (US + EU government, legal, academic, corporate), the deterministic scan is **0 false positives** —
  down from 92 before the filters above. A genuinely-custom ToUnicode attack on a *simple* font is
  intentionally left to the [OCR fallback](#ocr-fallback-optional) rather than guessed at
  deterministically.
- The residual cases the outline check cannot reach (unreadable outlines, visual-only attacks) are
  covered by the optional [OCR fallback](#ocr-fallback-optional).
- A DOCX can place the substitution **per position** (the same letter drawn differently in different
  single-glyph font subsets), which a global char→char map cannot represent. That case is confirmed by
  the [DOCX render-OCR](#docx-render-ocr-render-ocr--catching-the-positional-attack) fallback, which
  renders the embedded fonts in a real webview and OCRs the result.
- HTML uses a **static** pass only; the authoritative ground truth (rendered DOM, computed styles,
  downloaded webfonts, screenshot OCR) needs a headless browser. The DOCX render path above is the first
  step toward that; the same webview render for arbitrary HTML is a later phase.
- The tool is **defensive**: OCR returns plain glyph text only and nothing is ever fed to a model. No
  network access at scan time.

## Performance

The deterministic scan (no OCR, no rendering) is **CPU-only and fast**. Measured across the full
**32-document clean corpus** (PDF + DOCX) on a Snapdragon X Elite (Windows on ARM64), `--release`:

| | Time |
|---|---|
| Median document | **~64 ms** |
| Typical PDF / subset-font DOCX | **~35–90 ms** |
| Largest document (IRS Publication 17, 200+ pages) | **~0.66 s** |
| Corpus mean | **~117 ms / document** |

Full-document run (32 clean + 8 tampered fixtures): **0 false positives**, **8/8** tampered detected,
**0** parse errors. OCR, when enabled as a fallback, is the only heavy step — and it fires *only* on the
documents that carry a collision signal: the two A3 *semantic-replacement* fixtures take **~105 s** (PDF,
pdfium-atlas render + OCR) and **~113 s** (DOCX, webview render + OCR), while the PUA / garbled-`ToUnicode`
attacks are already caught deterministically and skip rendering entirely (~0.1–0.5 s). The deterministic
checks are what run on every document by default.

## Evaluation corpus

The false-positive figures above are measured on a **32-document corpus** of free, publicly-available
documents (US + EU; companies, education, legal, public administration), chosen to stress the detector:
multiple languages, font producers (LaTeX/Computer Modern, MS Word, InDesign, Acrobat), ligature/math
fonts, subset vs full embedded fonts, and both PDF + DOCX. Only *known-clean* documents are collected
(the false-positive set); the defaced/positive set is generated synthetically.

| Region · Category | Documents |
|---|---|
| **US · public administration** | IRS forms: `f1040`, `fw9`, `fw4`, `f941`, `f1099msc`, Publication 17 |
| **US · legal** | govinfo public laws `PLAW-117publ58` (IIJA), `PLAW-116publ136` (CARES); US Code Title 17 |
| **US · education** | arXiv: `1706.03762` (Transformer), `1810.04805` (BERT), `1512.03385` (ResNet), `1409.1556` (VGG) |
| **US · companies** | Berkshire Hathaway shareholder letters 2020 / 2021 / 2022 |
| **EU · legal** | EU Charter of Fundamental Rights — **EN / FR / DE / IT / ES**; Spanish gazette BOE-A-2018-16673 |
| **EU · public administration** | EU Commission White Paper on AI (2020); Banca d'Italia *Relazione annuale* 2022 (IT) |
| **EU · companies** | Unilever Annual Report 2020; SAP Annual Report 2019 |
| **EU · education** | IZA (Bonn) discussion papers `dp15000`, `dp16000`; BIS (Basel) working paper `work1000` |
| **misc · DOCX** | calibre demo; filesamples `sample2`, `sample3` (Word-format coverage) |

Total: **32 documents, ~41 MB.** Reproducible and extensible via `corpus/fetch.ps1` (idempotent;
verifies magic bytes + SHA-256, writes `corpus/manifest.csv`). Sources are government, open-access
research, official gazettes and published corporate reports. (Note: EUR-Lex blocks scripted download —
the EU legal entries use the European Parliament Charter endpoint and national gazettes instead.)

## Background & references

`chk_defaced` is the detection tool for the **"Noroboto"** attack family — documents whose fonts make
the rendered text diverge from the AI-extracted text.

**The attack disclosures — LegalQuants RED TEAM** ([legalquants.com](https://www.legalquants.com)):

- **[Noroboto and the PDF that lied twice](https://www.legalquants.com/blog/noroboto-and-the-pdf-that-lied-twice)**
  — *Alexios vdSK*, May 2026. Names and demonstrates the attack: *"A PDF stores drawing commands, not
  text."* A page that renders **"$1,400,000"** can carry a `/ToUnicode` CMap that extracts **"$400…"**.
  The piece deliberately withheld the *"surgical (partial-targeted) and replacement variants … until a
  mitigation system is broadly deployed at the ingestion layer"* — which is exactly the layer this tool
  sits at.
- **[The Contract That Could Get You Fired](https://www.legalquants.com/blog/the-contract-that-could-get-you-fired)**
  — *Alexios vdSK & Iris Ng*, June 2026. The Cyrillic-homoglyph variant: Latin letters in **"Delaware"**
  swapped for look-alike Cyrillic so byte-level search never matches — *"the text is only what its bytes
  say it is, and a search engine compares bytes."* This is precisely the case in our `replaced.*` test
  fixtures, and why `chk_defaced` filters real homoglyphs via Unicode TR39 skeletons.

**The motivation write-up — Dario Finardi** (*Medium*):

- **[What You See Is Not What Your AI Reads](https://dariofinardi.it/what-you-see-is-not-what-your-ai-reads-c3fed388d3bc)**
  (June 2026). Lays out the attack "below the prompt-injection layer" and three scenarios that map
  directly onto what this tool flags:

  | Scenario | What it does | `chk_defaced` rules |
  |---|---|---|
  | **Full obfuscation** | whole text mapped to garbage codepoints | `UNICODE.PUA`, `PDF.TOUNICODE_GARBLED`, `FONT.PUA_CMAP` |
  | **Partial / targeted remapping** | a few numbers, names or clauses swapped, both readings lexically valid | `PDF.GLYPH_SEMANTIC_REPLACEMENT`, `DOCX.GLYPH_SEMANTIC_REPLACEMENT` |
  | **Per-subset manipulation** | different font subsets carry different maps across pages | `PDF.MANY_SUBSETS` |

  Its recipe — *compare embedded mappings against the canonical font, flag divergences and re-read those
  spans visually, treat fully custom fonts as high-risk, and use classical OCR (Tesseract/PaddleOCR)
  rather than vision-language models* — is exactly the design implemented here.
- **[$35 Million. Destroyed by a Scanner.](https://dariofinardi.it/35-million-destroyed-by-a-scanner-53866e98c465)**
  (June 2026). Companion piece: a PDF is a **layered container** (content, markup, navigation,
  structure), not a photograph — so what a pipeline extracts is not what a human sees.

Independent research on the same attack class:

- **[Invisible Prompts, Visible Threats: Malicious Font Injection in External Resources for LLMs](https://arxiv.org/abs/2505.16957)**
  — quantifies the threat: hidden adversarial prompts in malicious fonts reach up to ~70% attack
  success on PDFs (this is the `2505.16957v1.pdf` used in our test corpus).
- **[Poisoned Typeface](https://layerxsecurity.com/blog/poisoned-typeface-a-simple-font-rendering-poisons-every-ai-assistant-and-only-microsoft-cares/)**
  (LayerX) and [BleepingComputer's coverage](https://www.bleepingcomputer.com/news/security/new-font-rendering-trick-hides-malicious-commands-from-ai-tools/)
  — the browser/HTML variant: a custom font acts as a cipher key so the rendered text and the DOM text
  diverge.

## License

Licensed under the **GNU Affero General Public License, version 3** (`AGPL-3.0-only`) — see
[`LICENSE`](LICENSE). Copyright © 2026 **Dario Finardi**.

The AGPL is a strong copyleft: if you run a modified version to provide a service over a network, you
must offer that version's complete source to its users. Redistributions and derivative works must remain
under the AGPL-3.0 and carry a verbatim copy of the license text.

## Contact

For more information — including commercial licensing or integration support — get in touch via
[**jugaad.digital**](https://jugaad.digital) or reach out on
[**LinkedIn** (Dario Finardi)](https://www.linkedin.com/in/dfinardi/).
