//! Font registry: build it from a directory (e.g. the system fonts) into a JSON index holding each
//! font's **official cmap**, and identify a suspect font against the registry.
//!
//! The index is the "ground truth" the scanner compares document-embedded fonts against: a font that
//! claims a known identity but carries a different cmap is the classic tampering signal.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::font::{self, ParsedFont};

/// Canonical outline lookup for one family: codepoint → {hashes across styles}, and hash → codepoint.
type OutlineIndex = (HashMap<u32, HashSet<u64>>, HashMap<u64, u32>);

/// A registry entry (one font face).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontEntry {
    pub path: String,
    pub collection_index: u32,
    pub family: Option<String>,
    pub subfamily: Option<String>,
    pub full_name: Option<String>,
    pub postscript_name: Option<String>,
    pub version: Option<String>,
    pub copyright: Option<String>,
    pub manufacturer: Option<String>,
    pub num_glyphs: u16,
    pub units_per_em: u16,
    pub file_sha256: String,
    pub cmap_sha256: String,
    /// Number of mapped codepoints (always present, even when the cmap is omitted).
    pub cmap_len: usize,
    /// Official cmap: (codepoint, glyph_id) pairs. Omitted with `--slim`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmap: Option<Vec<(u32, u16)>>,
    /// Per-codepoint Latin outline hashes (codepoint, FNV-1a). The ground truth for the canonical
    /// tamper check; always stored (small, even with `--slim`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outlines: Vec<(u32, u64)>,
}

impl FontEntry {
    fn from_parsed(path: &Path, f: &ParsedFont, include_cmap: bool) -> Self {
        FontEntry {
            path: path.display().to_string(),
            collection_index: f.collection_index,
            family: f.family.clone(),
            subfamily: f.subfamily.clone(),
            full_name: f.full_name.clone(),
            postscript_name: f.postscript_name.clone(),
            version: f.version.clone(),
            copyright: f.copyright.clone(),
            manufacturer: f.manufacturer.clone(),
            num_glyphs: f.num_glyphs,
            units_per_em: f.units_per_em,
            file_sha256: f.file_sha256.clone(),
            cmap_sha256: f.cmap_sha256.clone(),
            cmap_len: f.cmap.len(),
            cmap: if include_cmap {
                Some(f.cmap.iter().map(|(cp, g)| (*cp, *g)).collect())
            } else {
                None
            },
            outlines: f.outlines.iter().map(|(cp, h)| (*cp, *h)).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FontRegistry {
    pub tool: String,
    pub source_dir: String,
    pub count: usize,
    /// Font files that could not be parsed (non-SFNT formats such as `.fon`, or corrupt).
    pub skipped: Vec<String>,
    pub fonts: Vec<FontEntry>,
}

/// Result of identifying a suspect font against the registry.
#[derive(Debug, Clone, PartialEq)]
pub enum Identification {
    /// File SHA-256 identical to a known one: the font is pristine.
    Pristine,
    /// Known identity (same family/postscript) but a **different cmap** → binary modified.
    KnownButCmapModified,
    /// Known identity and consistent cmap, but a different binary (legitimate subset or re-encoding).
    KnownVariant,
    /// No match → forged/unknown font (must be checked visually/by OCR).
    Unidentified,
}

impl FontRegistry {
    /// Build the registry by recursively scanning `dir` for font files.
    pub fn build_from_dir(dir: &Path, include_cmap: bool) -> Result<Self> {
        let mut fonts = Vec::new();
        let mut skipped = Vec::new();
        for entry in WalkDir::new(dir).follow_links(false).into_iter().filter_map(|e| e.ok()) {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !font::is_font_ext(ext) {
                continue;
            }
            match font::parse_file(p) {
                Ok(faces) if !faces.is_empty() => {
                    for f in &faces {
                        fonts.push(FontEntry::from_parsed(p, f, include_cmap));
                    }
                }
                _ => skipped.push(p.display().to_string()),
            }
        }
        Ok(FontRegistry {
            tool: format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION")),
            source_dir: dir.display().to_string(),
            count: fonts.len(),
            skipped,
            fonts,
        })
    }

    pub fn save_json(&self, path: &Path, pretty: bool) -> Result<()> {
        let data = if pretty {
            serde_json::to_vec_pretty(self)?
        } else {
            serde_json::to_vec(self)?
        };
        std::fs::write(path, data).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn load_json(path: &Path) -> Result<Self> {
        let data = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        Ok(serde_json::from_slice(&data)?)
    }

    /// Index by file SHA and by identity (family/postscript, lowercased) for fast lookup.
    fn index(&self) -> (BTreeMap<&str, &FontEntry>, BTreeMap<String, Vec<&FontEntry>>) {
        let mut by_sha = BTreeMap::new();
        let mut by_id: BTreeMap<String, Vec<&FontEntry>> = BTreeMap::new();
        for e in &self.fonts {
            by_sha.insert(e.file_sha256.as_str(), e);
            for id in [&e.postscript_name, &e.family, &e.full_name].into_iter().flatten() {
                by_id.entry(id.to_lowercase()).or_default().push(e);
            }
        }
        (by_sha, by_id)
    }

    /// Aggregate the outline hashes of every registry font in `family` (case-insensitive): codepoint →
    /// {hashes seen across weights/styles}, and hash → codepoint (reverse). `None` if the family has no
    /// stored outlines.
    fn outline_index_for(&self, family: &str) -> Option<OutlineIndex> {
        let fam = family.to_lowercase();
        let mut cp_hashes: HashMap<u32, HashSet<u64>> = HashMap::new();
        let mut hash_cp: HashMap<u64, u32> = HashMap::new();
        for e in &self.fonts {
            if e.family.as_deref().map(|f| f.to_lowercase()).as_deref() != Some(fam.as_str()) {
                continue;
            }
            for (cp, h) in &e.outlines {
                cp_hashes.entry(*cp).or_default().insert(*h);
                hash_cp.entry(*h).or_insert(*cp);
            }
        }
        (!cp_hashes.is_empty()).then_some((cp_hashes, hash_cp))
    }

    /// **Canonical (outline-vs-registry) tamper check** — deterministic, no OCR. For an embedded font
    /// whose family is in the registry, returns `(extracted_char, true_drawn_char)` for every codepoint
    /// whose embedded glyph draws a **different canonical letter** than the codepoint claims (the font
    /// lies, with the correct direction). Empty when the family is absent or every glyph is honest/
    /// modified-but-unmatched (the latter is "doubt" → resolve with OCR).
    pub fn canonical_substitutions(&self, f: &ParsedFont) -> Vec<(char, char)> {
        let Some(family) = f.family.as_deref() else { return Vec::new() };
        let Some((cp_hashes, hash_cp)) = self.outline_index_for(family) else { return Vec::new() };
        let mut out = Vec::new();
        for (cp, h) in &f.outlines {
            if cp_hashes.get(cp).map(|s| s.contains(h)).unwrap_or(false) {
                continue; // honest: matches the canonical glyph for the same codepoint
            }
            if let Some(&drawn) = hash_cp.get(h) {
                if drawn != *cp {
                    if let (Some(ec), Some(dc)) = (char::from_u32(*cp), char::from_u32(drawn)) {
                        out.push((ec, dc)); // tampered: draws a different canonical letter
                    }
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// Identify a suspect font against the registry.
    pub fn identify(&self, f: &ParsedFont) -> Identification {
        let (by_sha, by_id) = self.index();
        if by_sha.contains_key(f.file_sha256.as_str()) {
            return Identification::Pristine;
        }
        let ids = [&f.postscript_name, &f.family, &f.full_name];
        for id in ids.into_iter().flatten() {
            if let Some(cands) = by_id.get(&id.to_lowercase()) {
                // Known identity: does the cmap match the official one?
                if cands.iter().any(|c| c.cmap_sha256 == f.cmap_sha256) {
                    return Identification::KnownVariant;
                }
                return Identification::KnownButCmapModified;
            }
        }
        Identification::Unidentified
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::ParsedFont;

    fn pf(family: &str, outlines: &[(u32, u64)]) -> ParsedFont {
        ParsedFont {
            family: Some(family.into()),
            subfamily: None,
            full_name: None,
            postscript_name: None,
            version: None,
            copyright: None,
            manufacturer: None,
            num_glyphs: 0,
            units_per_em: 1000,
            cmap: BTreeMap::new(),
            outlines: outlines.iter().copied().collect(),
            cmap_sha256: String::new(),
            file_sha256: String::new(),
            collection_index: 0,
        }
    }

    #[test]
    fn canonical_detects_tamper_and_direction() {
        // Canonical "Arial": outline hashes a=10, b=20, c=30.
        let canon = FontEntry::from_parsed(Path::new("arial.ttf"), &pf("Arial", &[(0x61, 10), (0x62, 20), (0x63, 30)]), false);
        let reg = FontRegistry {
            tool: String::new(),
            source_dir: String::new(),
            count: 1,
            skipped: vec![],
            fonts: vec![canon],
        };
        // Embedded "Arial" subset: 'a' honest (10); 'b' TAMPERED — draws c's glyph (30).
        assert_eq!(reg.canonical_substitutions(&pf("Arial", &[(0x61, 10), (0x62, 30)])), vec![('b', 'c')]);
        // All honest → no findings.
        assert!(reg.canonical_substitutions(&pf("Arial", &[(0x61, 10), (0x62, 20)])).is_empty());
        // Unknown family → inconclusive (empty → caller falls back to OCR).
        assert!(reg.canonical_substitutions(&pf("Mystery", &[(0x62, 30)])).is_empty());
        // Modified glyph that matches no canonical letter → inconclusive, not a false tamper.
        assert!(reg.canonical_substitutions(&pf("Arial", &[(0x62, 99)])).is_empty());
    }
}
