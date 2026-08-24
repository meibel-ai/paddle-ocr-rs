//! Dump the character boxes pdfium hands us, around a piece of text.
//!
//! `cargo run --example chars -- file.pdf 1 "Panel of"`
//!
//! The boxes are where this branch's bugs live, and they are not what a PDF
//! viewer shows: pdfium reports a glyph's *advance* box, invents spaces of its
//! own with no advance at all, and lets an overhanging glyph such as `f` run
//! past its own advance. Every word-splitting constant in `native::text` was
//! set from a run of this.

use pdf_extractor_2_md::native::bind_pdfium;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("pdf");
    let page_number: usize = args.next().expect("page").parse()?;
    let needle = args.next().unwrap_or_default();

    let library = bind_pdfium()?;
    let document = library.load_pdf_from_file(&path, None)?;
    let page = document.pages().get(page_number as u16 - 1)?;
    let text = page.text()?;
    let all_chars = text.chars();
    let chars: Vec<_> = all_chars.iter().collect();
    let all: String = chars.iter().filter_map(|c| c.unicode_char()).collect();
    let start = all
        .find(&needle)
        .map(|byte| all[..byte].chars().count().saturating_sub(10))
        .unwrap_or(0);

    for character in chars.iter().skip(start) {
        let value = character.unicode_char().unwrap_or('?');
        let loose = character.loose_bounds().ok();
        let tight = character.tight_bounds().ok();
        println!(
            "{value:?} size={:.2} loose=[{}] tight=[{}]",
            character.scaled_font_size().value,
            loose
                .map(|b| format!("{:.2}..{:.2}", b.left().value, b.right().value))
                .unwrap_or_else(|| "err".into()),
            tight
                .map(|b| format!("{:.2}..{:.2}", b.left().value, b.right().value))
                .unwrap_or_else(|| "err".into()),
        );
    }
    Ok(())
}
