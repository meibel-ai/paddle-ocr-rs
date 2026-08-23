//! Validate the in-process webview render: `cargo run --features render-wry --example render_docx -- in.docx out.png`
//! Builds the self-contained HTML from the DOCX, renders it in a Tauri webview (embedded fonts applied),
//! and captures the window to a PNG — the input for the render-OCR comparison.

use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let input = PathBuf::from(args.next().expect("usage: render_docx <in.docx> <out.png>"));
    let output = PathBuf::from(args.next().expect("usage: render_docx <in.docx> <out.png>"));
    let html = chk_defaced::docx_html::docx_to_html(&input)?;
    let opts = chk_defaced::render::RenderOptions::default();
    chk_defaced::render::render_html_to_png(&html, &output, &opts)?;
    eprintln!("rendered → {}", output.display());
    Ok(())
}
