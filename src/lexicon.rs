//! The lexical oracle: is this a real word, and what language is this page?
//!
//! Backed by the Apache-2.0 wordlists in `models/wordlists/` (from
//! tesseract-langdata), one word per line, **sorted by descending frequency**.
//! That ordering is what makes one file serve two purposes: membership answers
//! "does this word exist", and the rank stands in for frequency, which is how
//! the page's language is guessed — the same division of labour edito-ocr-v6
//! measured for hunspell/wordfreq (`lessico.py`), with the rank playing
//! wordfreq's part.
//!
//! The hunspell dictionaries stay out for now: for Italian and German only
//! GPL ones exist (quarantined in `models/hunspell-gpl/`, author's call).

use std::collections::HashMap;
use std::path::Path;

/// The languages the pipeline can arbitrate, as tessdata codes.
pub const LANGUAGES: [&str; 6] = ["ita", "eng", "fra", "deu", "spa", "por"];

/// Words consulted per language when guessing a page's language.
const RANKED: usize = 50_000;

/// One language's word set, with the rank of its most frequent words.
struct Wordlist {
    /// Lowercase word → rank (0 = most frequent). Membership for every word,
    /// rank meaningful for the first [`RANKED`].
    words: HashMap<String, u32>,
}

/// All loaded languages.
pub struct Lexicon {
    lists: HashMap<&'static str, Wordlist>,
}

impl Lexicon {
    /// A lexicon built from literal words, for tests across the crate.
    #[cfg(test)]
    pub fn from_test_words(words: HashMap<&'static str, Vec<&str>>) -> Self {
        let lists = words
            .into_iter()
            .map(|(language, list)| {
                let words = list
                    .iter()
                    .enumerate()
                    .map(|(rank, word)| (word.to_string(), rank as u32))
                    .collect();
                // Only the statically known language codes can be keys.
                let language = LANGUAGES
                    .iter()
                    .copied()
                    .find(|&known| known == language)
                    .expect("test language must be a known code");
                (language, Wordlist { words })
            })
            .collect();
        Lexicon { lists }
    }

    /// Load every available wordlist from a directory. A missing language is
    /// skipped, not fatal: the arbiter simply cannot verify that language.
    pub fn load(dir: &Path) -> Self {
        let mut lists = HashMap::new();
        for language in LANGUAGES {
            let file = dir.join(format!("{language}.wordlist"));
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            let words = text
                .lines()
                .enumerate()
                .filter(|(_, word)| !word.trim().is_empty())
                .map(|(rank, word)| (word.trim().to_lowercase(), rank as u32))
                .collect();
            lists.insert(language, Wordlist { words });
        }
        Lexicon { lists }
    }

    pub fn is_empty(&self) -> bool {
        self.lists.is_empty()
    }

    /// Whether `word` exists in any of the given languages.
    pub fn contains(&self, word: &str, languages: &[&str]) -> bool {
        let lowered = word.to_lowercase();
        languages
            .iter()
            .filter_map(|language| self.lists.get(language))
            .any(|list| list.words.contains_key(&lowered))
    }

    /// The language most of `words` belong to, weighted by how common each
    /// word is there — a page of Italian scores Italian far above English even
    /// though half its short words exist in both.
    pub fn detect_language(&self, words: impl Iterator<Item = String>) -> Option<&'static str> {
        let mut scores: HashMap<&'static str, f64> = HashMap::new();
        let mut counted = 0usize;
        for word in words {
            let lowered = word.to_lowercase();
            if lowered.chars().count() < 2 {
                continue;
            }
            counted += 1;
            for (&language, list) in &self.lists {
                if let Some(&rank) = list.words.get(&lowered) {
                    if (rank as usize) < RANKED {
                        // 1/ln(rank): the closest thing to zipf the list offers.
                        *scores.entry(language).or_default() +=
                            1.0 / (2.0 + f64::from(rank)).ln();
                    }
                }
            }
        }
        if counted < 5 {
            return None; // too little text to have an opinion
        }
        scores
            .into_iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(language, _)| language)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_lexicon() -> Lexicon {
        let mut lists = HashMap::new();
        let load = |words: &[&str]| Wordlist {
            words: words
                .iter()
                .enumerate()
                .map(|(rank, word)| (word.to_string(), rank as u32))
                .collect(),
        };
        lists.insert("ita", load(&["di", "che", "della", "fornitura", "linea"]));
        lists.insert("eng", load(&["the", "of", "and", "supply", "line"]));
        Lexicon { lists }
    }

    #[test]
    fn membership_is_case_insensitive_and_per_language() {
        let lexicon = tiny_lexicon();
        assert!(lexicon.contains("Fornitura", &["ita"]));
        assert!(!lexicon.contains("fornitura", &["eng"]));
        assert!(lexicon.contains("fornitura", &["eng", "ita"]));
    }

    #[test]
    fn the_page_language_follows_the_common_words() {
        let lexicon = tiny_lexicon();
        let italian = ["di", "che", "della", "fornitura", "di", "linea"];
        assert_eq!(
            lexicon.detect_language(italian.iter().map(|w| w.to_string())),
            Some("ita")
        );
        let english = ["the", "of", "and", "supply", "the", "line"];
        assert_eq!(
            lexicon.detect_language(english.iter().map(|w| w.to_string())),
            Some("eng")
        );
    }

    #[test]
    fn too_little_text_yields_no_opinion() {
        let lexicon = tiny_lexicon();
        assert_eq!(lexicon.detect_language(["di"].iter().map(|w| w.to_string())), None);
    }
}
