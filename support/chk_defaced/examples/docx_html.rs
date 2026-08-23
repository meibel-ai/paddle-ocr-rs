//! Render-input check: `cargo run --example docx_html -- in.docx out.html`
//! Emits the self-contained HTML (embedded fonts as base64 `@font-face`) for visual/render-OCR
//! validation in any browser/webview.

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(args.next().expect("usage: docx_html <in.docx> <out.html>"));
    let output = PathBuf::from(args.next().expect("usage: docx_html <in.docx> <out.html>"));
    let html = chk_defaced::docx_html::docx_to_html(&input)?;
    std::fs::write(&output, &html)?;
    eprintln!("wrote {} bytes → {}", html.len(), output.display());
    Ok(())
}
