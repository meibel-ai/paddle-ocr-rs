//! Content-stream tokenizer and per-page signal extraction.
//!
//! The page content is walked with a minimal PDF tokenizer instead of a byte
//! search: strings, hex strings, names, comments and inline-image data are
//! consumed as units, so an operator name that happens to appear inside a
//! string is never counted as an operator.
//!
//! Only as much graphics state as the signals need is tracked:
//!
//! * the **CTM**, to measure how much of the page an image covers — a placed
//!   image is the unit square mapped through the CTM, so its area is simply
//!   `|a·d − b·c|`, no bounding-box arithmetic needed;
//! * the **text rendering mode**, to tell a painted glyph from the invisible
//!   `3 Tr` layer of a searchable scan (both are part of the state saved by
//!   `q` and restored by `Q`).

use std::collections::HashSet;

use lopdf::{Dictionary, Document, Object, ObjectId};

use super::PageSignals;

/// Row-vector affine matrix `[a b c d e f]`.
type Matrix = [f64; 6];

const IDENTITY: Matrix = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// US Letter, the fallback when a page declares no usable `/MediaBox`.
const DEFAULT_MEDIA_BOX: [f64; 4] = [0.0, 0.0, 612.0, 792.0];

/// `/Parent` hops allowed while looking for an inherited `/MediaBox`.
const MAX_PARENT_HOPS: usize = 8;

/// Text is invisible in rendering mode 3 (neither filled nor stroked) and 7
/// (clip only) — the modes a searchable-scan layer and hidden text both use.
fn is_invisible(render_mode: i64) -> bool {
    render_mode == 3 || render_mode == 7
}

/// `a` applied first, then `b`.
fn mul(a: Matrix, b: Matrix) -> Matrix {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

/// Area the unit square covers once mapped through `m`.
fn unit_area(m: &Matrix) -> f64 {
    (m[0] * m[3] - m[1] * m[2]).abs()
}

/// The part of the graphics state that `q` saves and `Q` restores.
#[derive(Clone, Copy)]
struct GraphicsState {
    ctm: Matrix,
    render_mode: i64,
}

impl Default for GraphicsState {
    fn default() -> Self {
        GraphicsState { ctm: IDENTITY, render_mode: 0 }
    }
}

/// A token of a content stream. Array delimiters are not emitted: the strings
/// inside a `TJ` array reach the operand buffer directly, which is all the
/// caller needs.
enum Token<'a> {
    Number(f64),
    Name(&'a [u8]),
    /// A shown string. `hex` distinguishes `<41 42>` from `(AB)`.
    Str { raw: &'a [u8], hex: bool },
    Operator(&'a [u8]),
}

fn is_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

fn is_delimiter(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

struct Lexer<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn new(data: &'a [u8]) -> Self {
        Lexer { data, pos: 0 }
    }

    fn skip_whitespace_and_comments(&mut self) {
        while self.pos < self.data.len() {
            let b = self.data[self.pos];
            if is_whitespace(b) {
                self.pos += 1;
            } else if b == b'%' {
                while self.pos < self.data.len() && !matches!(self.data[self.pos], b'\n' | b'\r') {
                    self.pos += 1;
                }
            } else {
                return;
            }
        }
    }

    /// Consume a literal string, honouring nesting and backslash escapes, and
    /// return its raw inner bytes (escapes left in place: the caller only
    /// counts bytes and ASCII letters, so expanding them would buy nothing).
    fn literal_string(&mut self) -> &'a [u8] {
        let start = self.pos;
        let mut depth = 1usize;
        while self.pos < self.data.len() {
            match self.data[self.pos] {
                b'\\' => self.pos += 1, // skip the escaped byte along with the backslash
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        let raw = &self.data[start..self.pos];
                        self.pos += 1;
                        return raw;
                    }
                }
                _ => {}
            }
            self.pos += 1;
        }
        &self.data[start..]
    }

    fn next(&mut self) -> Option<Token<'a>> {
        self.skip_whitespace_and_comments();
        let b = *self.data.get(self.pos)?;
        match b {
            b'(' => {
                self.pos += 1;
                Some(Token::Str { raw: self.literal_string(), hex: false })
            }
            b'<' => {
                // `<<` opens a dictionary: step over it and let its contents be
                // tokenized normally; a lone `<` opens a hex string.
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    return self.next();
                }
                self.pos += 1;
                let start = self.pos;
                while self.pos < self.data.len() && self.data[self.pos] != b'>' {
                    self.pos += 1;
                }
                let raw = &self.data[start..self.pos];
                self.pos = (self.pos + 1).min(self.data.len());
                Some(Token::Str { raw, hex: true })
            }
            b'/' => {
                self.pos += 1;
                let start = self.pos;
                while self.pos < self.data.len()
                    && !is_whitespace(self.data[self.pos])
                    && !is_delimiter(self.data[self.pos])
                {
                    self.pos += 1;
                }
                Some(Token::Name(&self.data[start..self.pos]))
            }
            b'>' | b']' | b'[' | b'{' | b'}' | b')' => {
                self.pos += 1;
                self.next()
            }
            _ => {
                let start = self.pos;
                while self.pos < self.data.len()
                    && !is_whitespace(self.data[self.pos])
                    && !is_delimiter(self.data[self.pos])
                {
                    self.pos += 1;
                }
                if self.pos == start {
                    self.pos += 1; // never stall on an unexpected byte
                    return self.next();
                }
                let token = &self.data[start..self.pos];
                match token[0] {
                    b'0'..=b'9' | b'+' | b'-' | b'.' => Some(Token::Number(parse_number(token))),
                    _ => Some(Token::Operator(token)),
                }
            }
        }
    }

    /// Skip an inline image (`BI … ID <binary> EI`). The binary payload is not
    /// valid token soup, so it is stepped over by looking for a delimited `EI`.
    fn skip_inline_image(&mut self) {
        while let Some(token) = self.next() {
            if matches!(token, Token::Operator(op) if op == b"ID") {
                break;
            }
        }
        self.pos += 1; // the single whitespace byte that follows `ID`
        while self.pos + 1 < self.data.len() {
            let at_boundary = self.pos == 0 || is_whitespace(self.data[self.pos - 1]);
            if at_boundary && &self.data[self.pos..self.pos + 2] == b"EI" {
                let after = self.data.get(self.pos + 2);
                if after.is_none_or(|&b| is_whitespace(b) || is_delimiter(b)) {
                    self.pos += 2;
                    return;
                }
            }
            self.pos += 1;
        }
        self.pos = self.data.len();
    }
}

fn parse_number(token: &[u8]) -> f64 {
    std::str::from_utf8(token).ok().and_then(|s| s.parse().ok()).unwrap_or(0.0)
}

/// Tracks which distinct ASCII alphanumerics appear in the shown text. A pure
/// image page carries none; a page of real prose carries dozens — the signal
/// that separates a scan from a page whose few glyphs are decorative.
#[derive(Default)]
struct AlnumSet(u128);

impl AlnumSet {
    fn insert_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            let slot = match b {
                b'0'..=b'9' => b - b'0',
                b'a'..=b'z' => b - b'a' + 10,
                b'A'..=b'Z' => b - b'A' + 36,
                _ => continue,
            };
            self.0 |= 1u128 << slot;
        }
    }

    fn len(&self) -> usize {
        self.0.count_ones() as usize
    }
}

/// Follow a reference to the object it names, if it is one.
fn resolve<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Object> {
    doc.dereference(object).ok().map(|(_, object)| object)
}

/// The dictionary of an object. An XObject is a *stream*, and its dictionary
/// is the stream's — `as_dict` alone silently misses every image on the page.
fn dictionary_of(object: &Object) -> Option<&Dictionary> {
    match object {
        Object::Dictionary(dictionary) => Some(dictionary),
        Object::Stream(stream) => Some(&stream.dict),
        _ => None,
    }
}

/// The `/MediaBox` of a page, following `/Parent` when the page inherits it.
fn media_box(doc: &Document, page_id: ObjectId) -> [f64; 4] {
    let mut id = page_id;
    for _ in 0..MAX_PARENT_HOPS {
        let Ok(dict) = doc.get_dictionary(id) else { break };
        if let Some(Object::Array(values)) = dict.get(b"MediaBox").ok().and_then(|o| resolve(doc, o))
        {
            let mut boxed = [0.0; 4];
            let parsed = values.iter().take(4).enumerate().all(|(i, value)| {
                match value.as_float() {
                    Ok(v) => {
                        boxed[i] = v as f64;
                        true
                    }
                    Err(_) => false,
                }
            });
            if parsed && values.len() >= 4 {
                return boxed;
            }
        }
        match dict.get(b"Parent").and_then(|o| o.as_reference()) {
            Ok(parent) => id = parent,
            Err(_) => break,
        }
    }
    DEFAULT_MEDIA_BOX
}

/// Names of the image XObjects reachable from a page's resources.
///
/// Form XObjects are not descended into: their content is invisible to this
/// pass, which only under-counts images on pages that wrap a scan in a form.
/// The pdfium pass of Phase 2 sees through forms and settles those cases.
fn image_xobject_names(doc: &Document, page_id: ObjectId) -> HashSet<Vec<u8>> {
    let mut names = HashSet::new();
    let Ok((own, inherited)) = doc.get_page_resources(page_id) else { return names };
    let dicts = own
        .into_iter()
        .chain(inherited.iter().filter_map(|&id| doc.get_dictionary(id).ok()));
    for resources in dicts {
        let xobjects =
            resources.get(b"XObject").ok().and_then(|o| resolve(doc, o)).and_then(dictionary_of);
        let Some(xobjects) = xobjects else { continue };
        for (name, object) in xobjects.iter() {
            let Some(dict) = resolve(doc, object).and_then(dictionary_of) else { continue };
            if dict.get(b"Subtype").and_then(|o| o.as_name()).is_ok_and(|s| s == b"Image") {
                names.insert(name.to_vec());
            }
        }
    }
    names
}

/// Walk one page's content stream and collect its signals.
pub(super) fn page_signals(doc: &Document, page_id: ObjectId) -> PageSignals {
    let content = doc.get_page_content(page_id);
    let mut signals = PageSignals { content_len: content.len(), ..PageSignals::default() };
    if content.is_empty() {
        return signals;
    }

    let media = media_box(doc, page_id);
    let page_area = ((media[2] - media[0]) * (media[3] - media[1])).abs();
    let images = image_xobject_names(doc, page_id);

    let mut state = GraphicsState::default();
    let mut stack: Vec<GraphicsState> = Vec::new();
    let mut operands: Vec<Token> = Vec::new();
    let mut alnum = AlnumSet::default();
    let mut covered = 0.0f64;

    let mut lexer = Lexer::new(&content);
    while let Some(token) = lexer.next() {
        let Token::Operator(op) = token else {
            operands.push(token);
            continue;
        };
        match op {
            b"q" => stack.push(state),
            b"Q" => state = stack.pop().unwrap_or_default(),
            b"cm" => {
                if let Some(m) = matrix_operands(&operands) {
                    state.ctm = mul(m, state.ctm);
                }
            }
            b"Tf" => signals.font_changes += 1,
            b"Tr" => {
                if let Some(Token::Number(mode)) = operands.last() {
                    state.render_mode = *mode as i64;
                }
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => {
                signals.text_ops += 1;
                let mut shown = 0usize;
                for operand in &operands {
                    if let Token::Str { raw, hex } = operand {
                        if *hex {
                            shown += raw.iter().filter(|b| b.is_ascii_hexdigit()).count() / 2;
                        } else {
                            shown += raw.len();
                            alnum.insert_bytes(raw);
                        }
                    }
                }
                if is_invisible(state.render_mode) {
                    signals.invisible_bytes += shown;
                } else {
                    signals.visible_bytes += shown;
                }
            }
            b"m" | b"l" | b"c" | b"v" | b"y" | b"re" => signals.path_ops += 1,
            b"Do" => {
                if let Some(Token::Name(name)) = operands.last() {
                    if images.contains(*name) {
                        signals.image_count += 1;
                        covered += unit_area(&state.ctm);
                    }
                }
            }
            b"BI" => {
                signals.image_count += 1;
                covered += unit_area(&state.ctm);
                lexer.skip_inline_image();
            }
            _ => {}
        }
        operands.clear();
    }

    signals.distinct_alnum = alnum.len();
    if page_area > 0.0 {
        // Overlapping placements can exceed the page; the ratio is a coverage
        // indicator, not a measure, so clamping keeps it interpretable.
        signals.image_coverage = (covered / page_area).min(1.0) as f32;
    }
    signals
}

/// The six numbers of a `cm` operator, in order.
fn matrix_operands(operands: &[Token]) -> Option<Matrix> {
    let numbers: Vec<f64> = operands
        .iter()
        .filter_map(|t| match t {
            Token::Number(v) => Some(*v),
            _ => None,
        })
        .collect();
    let tail = numbers.len().checked_sub(6)?;
    Some([
        numbers[tail],
        numbers[tail + 1],
        numbers[tail + 2],
        numbers[tail + 3],
        numbers[tail + 4],
        numbers[tail + 5],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn operators(content: &[u8]) -> Vec<String> {
        let mut lexer = Lexer::new(content);
        let mut out = Vec::new();
        while let Some(token) = lexer.next() {
            if let Token::Operator(op) = token {
                out.push(String::from_utf8_lossy(op).into_owned());
            }
        }
        out
    }

    #[test]
    fn operator_inside_a_string_is_not_an_operator() {
        // The bare byte scan this replaced counted the `Tj` inside the string.
        assert_eq!(operators(b"(a Tj b) Tj"), ["Tj"]);
        assert_eq!(operators(b"(escaped \\) Tj) Tj"), ["Tj"]);
    }

    #[test]
    fn comments_and_names_are_skipped() {
        assert_eq!(operators(b"% Tj in a comment\n/Name Tj"), ["Tj"]);
        assert_eq!(operators(b"/F1 12 Tf"), ["Tf"]);
    }

    #[test]
    fn nested_strings_close_at_the_right_parenthesis() {
        assert_eq!(operators(b"((nested) still text) Tj Q"), ["Tj", "Q"]);
    }

    #[test]
    fn inline_image_data_is_stepped_over() {
        let mut lexer = Lexer::new(b"BI /W 2 ID \xff\x00 Tj junk EI Q");
        assert!(matches!(lexer.next(), Some(Token::Operator(op)) if op == b"BI"));
        lexer.skip_inline_image();
        let rest: Vec<String> = {
            let mut out = Vec::new();
            while let Some(token) = lexer.next() {
                if let Token::Operator(op) = token {
                    out.push(String::from_utf8_lossy(op).into_owned());
                }
            }
            out
        };
        assert_eq!(rest, ["Q"], "the Tj inside the image payload must not leak out");
    }

    #[test]
    fn unit_area_measures_the_placed_image() {
        // `200 0 0 100 0 0 cm` places the unit square as a 200x100 rectangle.
        assert_eq!(unit_area(&[200.0, 0.0, 0.0, 100.0, 0.0, 0.0]), 20_000.0);
        // A rotation preserves area.
        let rotated = mul([0.0, 1.0, -1.0, 0.0, 0.0, 0.0], [200.0, 0.0, 0.0, 100.0, 0.0, 0.0]);
        assert_eq!(unit_area(&rotated), 20_000.0);
    }

    /// A one-page document placing `content` over the given page box, with a
    /// single image XObject named `Im1` in its resources.
    fn document_with(content: &[u8], width: i64, height: i64) -> (Document, ObjectId) {
        use lopdf::{dictionary, Stream};

        let mut doc = Document::with_version("1.5");
        let image = doc.add_object(Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Image" },
            vec![0; 4],
        ));
        let resources = doc.add_object(dictionary! { "XObject" => dictionary! { "Im1" => image } });
        let contents = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
        let pages_id = doc.new_object_id();
        let page = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => pages_id,
            "Contents" => contents,
            "Resources" => resources,
            "MediaBox" => vec![0.into(), 0.into(), width.into(), height.into()],
        });
        doc.objects.insert(
            pages_id,
            dictionary! { "Type" => "Pages", "Kids" => vec![page.into()], "Count" => 1 }.into(),
        );
        let catalog = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
        doc.trailer.set("Root", catalog);
        (doc, page)
    }

    #[test]
    fn an_image_is_found_through_its_stream_dictionary() {
        // Image XObjects are *streams*: resolving them as plain dictionaries
        // finds nothing, and every scanned page then reads as an empty page.
        let (doc, page) = document_with(b"q 200 0 0 100 0 0 cm /Im1 Do Q", 200, 100);
        let signals = page_signals(&doc, page);
        assert_eq!(signals.image_count, 1);
        assert_eq!(signals.image_coverage, 1.0, "the image covers the whole page");
    }

    #[test]
    fn a_small_placement_covers_only_its_share_of_the_page() {
        let (doc, page) = document_with(b"q 100 0 0 50 10 10 cm /Im1 Do Q", 200, 100);
        let signals = page_signals(&doc, page);
        assert_eq!(signals.image_coverage, 0.25);
    }

    #[test]
    fn the_render_mode_splits_visible_text_from_an_invisible_layer() {
        let (doc, page) =
            document_with(b"BT 3 Tr (hidden) Tj 0 Tr (shown here) Tj ET", 200, 100);
        let signals = page_signals(&doc, page);
        assert_eq!(signals.invisible_bytes, "hidden".len());
        assert_eq!(signals.visible_bytes, "shown here".len());
        assert_eq!(signals.text_ops, 2);
    }

    #[test]
    fn the_render_mode_is_restored_with_the_graphics_state() {
        // `Tr` belongs to the state `q`/`Q` save: text after the restore is
        // visible again, and counting it as hidden would flag clean documents.
        let (doc, page) = document_with(b"q 3 Tr (a hidden run) Tj Q (a visible run) Tj", 200, 100);
        let signals = page_signals(&doc, page);
        assert_eq!(signals.invisible_bytes, "a hidden run".len());
        assert_eq!(signals.visible_bytes, "a visible run".len());
    }

    #[test]
    fn alnum_set_counts_distinct_letters_only() {
        let mut set = AlnumSet::default();
        set.insert_bytes(b"aaa bbb 111 ...");
        assert_eq!(set.len(), 3);
    }
}
