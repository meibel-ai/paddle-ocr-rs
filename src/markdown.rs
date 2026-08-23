//! Blocks in, Markdown out.
//!
//! Everything upstream has already decided what each piece of the page is and
//! when it is read; this module only has to write it down. What it adds is the
//! typography a PDF loses on the way in — a paragraph is one paragraph even
//! though it was drawn as eight lines, and a word broken across a line break
//! is one word again.
//!
//! Figures are cropped from the rendered page, not lifted out as image
//! objects, because a figure is as often drawn as it is placed: a chart, a
//! diagram or a logo in vector art all come out the same way.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use base64::Engine as _;

use crate::assemble::Block;
use crate::native::Raster;
use crate::region::RegionKind;

/// Resolution figures are cropped at. Enough to stay readable when a reader
/// opens the Markdown, without turning every figure into a megabyte.
pub const FIGURE_DPI: f32 = 150.0;

/// JPEG quality for embedded figures. A data URI is read inline, so size
/// matters more than the last few percent of fidelity.
const EMBED_QUALITY: u8 = 82;

/// What to do with the figures on a page.
#[derive(Debug, Clone)]
pub enum Images {
    /// Leave them out; the caption still says one was there.
    Skip,
    /// Inline, as a `data:` URI — one self-contained file, larger.
    Embed,
    /// Write them next to the Markdown and link to them by relative path.
    Files { dir: PathBuf, prefix: String },
}

impl Default for Images {
    fn default() -> Self {
        Images::Embed
    }
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub images: Images,
    /// Emit the running heads and feet the layout marked as furniture.
    pub keep_furniture: bool,
}

/// Writes the Markdown of a document, page after page.
///
/// It is a type and not a function because figures have to be numbered across
/// the whole document, and because the file mode writes as it goes.
pub struct Writer {
    options: Options,
    figures: usize,
}

impl Writer {
    pub fn new(options: Options) -> Self {
        Writer { options, figures: 0 }
    }

    /// The Markdown of one page. `raster` is what figures are cropped from;
    /// without it a figure becomes a note that one was there.
    pub fn page(&mut self, blocks: &[Block], raster: Option<&Raster>) -> String {
        let mut out = String::new();
        for block in blocks {
            if !block.kind.is_content() && !self.options.keep_furniture {
                continue;
            }
            let piece = match block.kind {
                RegionKind::Figure => self.figure(block, raster),
                RegionKind::Title => heading(1, &paragraph(block)),
                RegionKind::Heading => heading(2, &paragraph(block)),
                RegionKind::Caption => format!("*{}*", paragraph(block)),
                // A table still reads as its lines until the grid is built
                // from the rules the page draws (Phase 3, next step).
                RegionKind::Table => block.text(),
                RegionKind::Formula => block.text(),
                _ => paragraph(block),
            };
            if !piece.trim().is_empty() {
                out.push_str(piece.trim_end());
                out.push_str("\n\n");
            }
        }
        out
    }

    /// A figure: cropped, written or embedded, and linked.
    fn figure(&mut self, block: &Block, raster: Option<&Raster>) -> String {
        self.figures += 1;
        let alt = {
            let caption = block.text();
            let caption = caption.trim();
            if caption.is_empty() {
                format!("figure {}", self.figures)
            } else {
                caption.replace(['\n', '[', ']'], " ")
            }
        };
        let Some(image) = raster.and_then(|raster| raster.crop(block.bbox)) else {
            return format!("*[{alt}]*");
        };
        match &self.options.images {
            Images::Skip => format!("*[{alt}]*"),
            Images::Embed => match encode_jpeg(&image) {
                Some(bytes) => {
                    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
                    format!("![{alt}](data:image/jpeg;base64,{data})")
                }
                None => format!("*[{alt}]*"),
            },
            Images::Files { dir, prefix } => {
                let name = format!("{prefix}-{:03}.png", self.figures);
                match write_png(&image, &dir.join(&name)) {
                    Ok(()) => format!("![{alt}]({name})"),
                    Err(error) => {
                        // Never silent: a figure that could not be written is
                        // said so in the document itself.
                        format!("*[{alt} — non scritta: {error}]*")
                    }
                }
            }
        }
    }
}

fn encode_jpeg(image: &image::RgbImage) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut encoder =
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, EMBED_QUALITY);
    encoder.encode_image(image).ok()?;
    Some(bytes)
}

fn write_png(image: &image::RgbImage, path: &Path) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    image.save(path).map_err(std::io::Error::other)
}

fn heading(level: usize, text: &str) -> String {
    format!("{} {}", "#".repeat(level), text)
}

/// A block's lines joined back into the paragraph they were drawn as.
fn paragraph(block: &Block) -> String {
    let lines: Vec<String> = block.lines.iter().map(crate::native::Line::text).collect();
    join_lines(&lines)
}

/// Join the lines of a paragraph, undoing the hyphenation the line breaks
/// introduced.
///
/// Only a hyphen followed by a lowercase letter is undone: `anti-` + `orario`
/// was one word, while `Regolamento-` + `Quadro` and `2017-` + `2020` keep
/// theirs, and so does a line ending in a dash before a capital.
pub fn join_lines(lines: &[String]) -> String {
    let mut out = String::new();
    for line in lines {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match out.chars().last() {
            None => out.push_str(line),
            // A hyphen at a line break: drop it when it only split a word,
            // keep it when it belongs to a compound — but never put a space
            // after it, because the break was inside one word either way.
            Some('-') => {
                if starts_lowercase(line) {
                    out.pop();
                }
                out.push_str(line);
            }
            Some(_) => {
                out.push(' ');
                out.push_str(line);
            }
        }
    }
    out
}

fn starts_lowercase(text: &str) -> bool {
    text.chars().next().is_some_and(char::is_lowercase)
}

/// The whole document, with a heading for each page break where it helps a
/// reader keep their place.
pub fn document(pages: &[String]) -> String {
    let mut out = String::new();
    for page in pages.iter().filter(|page| !page.trim().is_empty()) {
        let _ = write!(out, "{}", page);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::native::Line;

    fn block(kind: RegionKind, lines: &[&str]) -> Block {
        Block {
            kind,
            bbox: Rect::new(0.0, 0.0, 100.0, 100.0),
            lines: lines.iter().map(|text| line_of(text)).collect(),
            order: None,
            recovered: false,
        }
    }

    /// A line carrying one word per space-separated token.
    fn line_of(text: &str) -> Line {
        use crate::native::{Style, Word};
        let style = Style { font: "T".into(), size: 10.0, bold: false, monospace: false };
        Line {
            words: text
                .split(' ')
                .map(|word| Word {
                    text: word.to_string(),
                    bbox: Rect::new(0.0, 0.0, 10.0, 10.0),
                    style: style.clone(),
                    visible: true,
                })
                .collect(),
            bbox: Rect::new(0.0, 0.0, 100.0, 10.0),
        }
    }

    #[test]
    fn a_paragraph_drawn_as_lines_comes_back_as_a_paragraph() {
        let text = paragraph(&block(RegionKind::Text, &["Il presente", "regolamento si applica"]));
        assert_eq!(text, "Il presente regolamento si applica");
    }

    #[test]
    fn hyphenation_from_a_line_break_is_undone_but_real_hyphens_are_kept() {
        assert_eq!(join_lines(&["dell'ammi-".into(), "nistrazione".into()]), "dell'amministrazione");
        // A compound keeps its hyphen: the next line starts with a capital.
        assert_eq!(join_lines(&["Regolamento-".into(), "Quadro".into()]), "Regolamento-Quadro");
        // So does a range of years.
        assert_eq!(join_lines(&["2017-".into(), "2020 e oltre".into()]), "2017-2020 e oltre");
    }

    #[test]
    fn headings_and_captions_take_their_markers() {
        let mut writer = Writer::new(Options::default());
        let out = writer.page(
            &[
                block(RegionKind::Title, &["Titolo del documento"]),
                block(RegionKind::Heading, &["Una sezione"]),
                block(RegionKind::Caption, &["Figura 1 — lo schema"]),
            ],
            None,
        );
        assert!(out.contains("# Titolo del documento"));
        assert!(out.contains("## Una sezione"));
        assert!(out.contains("*Figura 1 — lo schema*"));
    }

    #[test]
    fn furniture_is_dropped_unless_it_is_asked_for() {
        let running_head = [block(RegionKind::Furniture, &["GU L del 18.11.2024"])];
        assert!(Writer::new(Options::default()).page(&running_head, None).trim().is_empty());

        let options = Options { keep_furniture: true, ..Options::default() };
        assert!(Writer::new(options).page(&running_head, None).contains("GU L"));
    }

    #[test]
    fn a_figure_without_a_raster_still_says_it_was_there() {
        let mut writer = Writer::new(Options::default());
        let out = writer.page(&[block(RegionKind::Figure, &["Grafico delle vendite"])], None);
        assert_eq!(out.trim(), "*[Grafico delle vendite]*");
    }

    #[test]
    fn figures_are_numbered_across_the_document() {
        let mut writer = Writer::new(Options { images: Images::Skip, ..Options::default() });
        writer.page(&[block(RegionKind::Figure, &[""])], None);
        let second = writer.page(&[block(RegionKind::Figure, &[""])], None);
        assert!(second.contains("figure 2"), "the count does not restart on a new page");
    }
}
