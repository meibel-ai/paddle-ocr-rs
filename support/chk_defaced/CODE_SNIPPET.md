# chk_defaced — code snippets (for the article)

Ready-to-paste snippets that show the **real** API of the crate. (Note: the API is
`scan::scan_path` returning a `Report`, not a `Scanner`/`scan_file` object — the snippets
below compile against the published crate.)

---

### 1. Show me the code — the whole check is one call

*Caption: “I've now built that fix. It's called `chk_defaced`, a published Rust crate — and screening a
document is a single deterministic call: no LLM, no network, no headless browser.”*

```rust
use chk_defaced::scan;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let report = scan::scan_path(Path::new("contract.pdf"), None)?;

    // Explicit, machine-readable verdict, computed from the findings.
    if let Some(v) = report.assessment {
        if v.defaced     { println!("⚠  DEFACED — the extracted text differs from the glyphs drawn."); }
        if v.hidden_text { println!("⚠  HIDDEN TEXT — read by the machine, invisible to a human."); }
        if v.ok          { println!("✓  Clean — nothing above the flag threshold."); }
    }
    Ok(())
}
```

---

### 2. As an ingestion gate — refuse before you extract

*Caption: “Wire it in front of your pipeline. Any `High`-or-worse finding means the extracted text can't
be trusted — stop before it reaches the index or the model.”*

```rust
use chk_defaced::{scan, finding::Severity};
use std::path::Path;

fn is_safe_to_ingest(path: &Path) -> anyhow::Result<bool> {
    let report = scan::scan_path(path, None)?;
    Ok(report.max_severity() < Some(Severity::High)) // block High / Critical
}
```

---

### 3. What did it find? — the findings list

*Caption: “Every anomaly is a typed finding: a rule, a severity, a category, a message and where it is.”*

```rust
for f in &report.findings {
    println!("[{:?}] {:<32} {}  ({})", f.severity, f.rule, f.message, f.location);
}
// [High]  DOCX.GLYPH_SEMANTIC_REPLACEMENT  a glyph that draws 'd' is mapped from 'm' …  (embedded font)
// [High]  DOCX.INVISIBLE_TEXT_COLOR        ~62 char(s) near-white … e.g. "Ignore all previous …"  (runs)
```

---

### 4. Evaluate the source — provenance the container declares

*Caption: “Beyond the verdict, the report reads the document's provenance — so a mismatched last editor on
a ‘final' contract is a lead in itself.”*

```rust
if let Some(m) = &report.metadata {
    println!("author:          {:?}", m.author);           // Some("Jeremy Pomeroy")
    println!("last modified by:{:?}", m.last_modified_by);  // Some("Andrew Miller")  ← different person
    println!("authoring tool:  {:?}", m.creator_tool);      // Some("Microsoft Office Word")
    println!("producer:        {:?}", m.producer);          // Some("pdfTeX-1.40.25")  (PDF)
    println!("revision:        {:?}", m.revision);          // Some("13")
}
```

---

### 5. Which page, and what was hidden — recovered text

*Caption: “For defacement it shows the extracted vs. presumed text and the page; for hidden text the OCR
tier recovers the actual injected words.”*

```rust
for p in &report.phrases {
    println!("[page {:?}] read: {}\n           rendered: {}", p.page, p.extracted, p.presumed);
}
// A HIDDEN_TEXT_CONFIRMED finding then lists the recovered words, e.g.:
//   "render-OCR confirms 9 word(s) in the extract but not visible: ignore all previous instructions …"
```

---

### 6. From the command line — machine-readable report

*Caption: “Same thing without writing Rust — pipe the JSON into your tooling.”*

```sh
chk_defaced scan contract.pdf --format-out json
```

```jsonc
{
  "file": "contract.docx", "format": "docx",
  "assessment": { "ok": false, "defaced": true, "hidden_text": false, "max_severity": "High" },
  "metadata":   { "author": "Jeremy Pomeroy", "last_modified_by": "Andrew Miller",
                  "creator_tool": "Microsoft Office Word", "revision": "13" },
  "findings":   [ { "rule": "DOCX.GLYPH_SEMANTIC_REPLACEMENT", "severity": "High", … } ],
  "verdict":    "Confirmed"
}
```

---

### 7. (Optional) The core idea in one sentence of code

*Caption: “Detection is deterministic: hash each glyph's outline and see which letters reach it. One
outline reached from two different letters is a font that lies.”*

```rust
// Conceptual — the real code lives in pdf_glyph.rs / docx_glyph.rs
// outline_hash  ->  { 'm' : 5, 'd' : 2 }   // the 'm'-shape is also extracted as 'd'  → replacement
```
