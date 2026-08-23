//! Dependency-free text-comparison helpers shared by the render-OCR fallbacks (PDF [`crate::atlas`] and
//! DOCX [`crate::render`]): word-set similarity and aligned word-level substitution detection between the
//! **extracted** text (what a machine reads) and the **visual/OCR** text (what a human sees). A real,
//! deliberate substitution between the two is the render-level signature of semantic replacement
//! (variant A3) — including the localized/positional form the deterministic outline checks cannot express.

use std::collections::HashSet;

/// Normalized word set: lowercase, alphanumeric runs, length ≥ 2.
pub fn words(s: &str) -> HashSet<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(|w| w.to_string())
        .collect()
}

/// Word-set Jaccard similarity in `0..=1`, robust to word order and OCR segmentation differences.
pub fn jaccard(a: &str, b: &str) -> f32 {
    let (wa, wb) = (words(a), words(b));
    if wa.is_empty() && wb.is_empty() {
        return 1.0;
    }
    let inter = wa.intersection(&wb).count() as f32;
    let union = wa.union(&wb).count() as f32;
    if union == 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Ordered, normalized word sequence (lowercase, alphanumeric runs, length ≥ 2).
fn word_seq(s: &str) -> Vec<String> {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 2)
        .map(|w| w.to_string())
        .collect()
}

/// Aligned word-level mismatches between extracted and visual text (LCS diff; deletions paired with
/// insertions as substitutions). Catches **localized semantic replacement** — a few substituted words
/// in otherwise-identical text — which page-level set similarity cannot see. NB: OCR errors also surface
/// here, so use it as a signal / for investigation, not a zero-false-positive verdict.
pub fn word_mismatches(extracted: &str, visual: &str) -> Vec<(String, String)> {
    let a = word_seq(extracted);
    let b = word_seq(visual);
    let (n, m) = (a.len(), b.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    // LCS lengths (suffix DP).
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i][j] = if a[i] == b[j] {
                dp[i + 1][j + 1] + 1
            } else {
                dp[i + 1][j].max(dp[i][j + 1])
            };
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    let (mut dels, mut inss): (Vec<String>, Vec<String>) = (Vec::new(), Vec::new());
    let mut out = Vec::new();
    let flush = |dels: &mut Vec<String>, inss: &mut Vec<String>, out: &mut Vec<(String, String)>| {
        let k = dels.len().min(inss.len());
        for t in 0..k {
            out.push((dels[t].clone(), inss[t].clone()));
        }
        dels.clear();
        inss.clear();
    };
    while i < n && j < m {
        if a[i] == b[j] {
            flush(&mut dels, &mut inss, &mut out);
            i += 1;
            j += 1;
        } else if dp[i + 1][j] >= dp[i][j + 1] {
            dels.push(a[i].clone());
            i += 1;
        } else {
            inss.push(b[j].clone());
            j += 1;
        }
    }
    while i < n {
        dels.push(a[i].clone());
        i += 1;
    }
    while j < m {
        inss.push(b[j].clone());
        j += 1;
    }
    flush(&mut dels, &mut inss, &mut out);
    out
}

fn levenshtein(a: &[char], b: &[char]) -> usize {
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur.push((prev[j + 1] + 1).min(cur[j] + 1).min(prev[j] + cost));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Two aligned words look like a *real substitution* (not an OCR near-miss) if both are alphabetic words
/// of length ≥ 3 and their normalized edit distance is large (e.g. "delaware" vs "maryland"), as opposed
/// to OCR noise ("ai"/"al", "recipient"/"recipients").
fn is_real_substitution(e: &str, v: &str) -> bool {
    let (ec, vc): (Vec<char>, Vec<char>) = (e.chars().collect(), v.chars().collect());
    if ec.len() < 3 || vc.len() < 3 || !ec.iter().all(|c| c.is_alphabetic()) || !vc.iter().all(|c| c.is_alphabetic()) {
        return false;
    }
    let max = ec.len().max(vc.len()) as f32;
    levenshtein(&ec, &vc) as f32 / max > 0.4
}

/// Word substitutions that look deliberate (filters out OCR noise via edit distance). A non-empty result
/// is a strong signal of **semantic replacement** (variant A3): the extracted word differs from the
/// rendered/visible word, both lexically valid.
pub fn significant_substitutions(extracted: &str, visual: &str) -> Vec<(String, String)> {
    word_mismatches(extracted, visual)
        .into_iter()
        .filter(|(e, v)| is_real_substitution(e, v))
        .collect()
}

/// Words present in `extracted` but **absent** from `visual` (the OCR of the render) — the render-level
/// signature of **hidden text**: read by a machine, not shown to a human (white-on-white, sub-visible,
/// occluded, clipped, off-page). The dual of [`significant_substitutions`]. Restricted to alphabetic
/// words of length ≥ 4 and de-duplicated to blunt OCR misses of short/visible words; capped at 50.
pub fn missing_words(extracted: &str, visual: &str) -> Vec<String> {
    let visible = words(visual);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for w in word_seq(extracted) {
        if w.chars().count() >= 4
            && w.chars().all(|c| c.is_alphabetic())
            && !visible.contains(&w)
            && seen.insert(w.clone())
        {
            out.push(w);
            if out.len() >= 50 {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_localized_substitution() {
        let extracted = "governed by the laws of the State of Delaware without regard";
        let visual = "governed by the laws of the State of Maryland without regard";
        let subs = significant_substitutions(extracted, visual);
        assert_eq!(subs, vec![("delaware".to_string(), "maryland".to_string())]);
    }

    #[test]
    fn ignores_ocr_noise() {
        // a one-char OCR slip and a plural are not "real" substitutions
        assert!(significant_substitutions("the recipient", "the recipients").is_empty());
        assert!(significant_substitutions("paid to ai", "paid to al").is_empty());
    }

    #[test]
    fn jaccard_bounds() {
        assert_eq!(jaccard("", ""), 1.0);
        assert_eq!(jaccard("alpha beta", "alpha beta"), 1.0);
        assert!(jaccard("alpha beta", "gamma delta") < 0.01);
    }

    #[test]
    fn missing_words_finds_hidden_only() {
        // "ignore instructions" is in the extract but not in the rendered/visible text → hidden.
        let extracted = "Please review the contract ignore instructions and sign here";
        let visual = "Please review the contract and sign here";
        let miss = missing_words(extracted, visual);
        assert!(miss.contains(&"ignore".to_string()) && miss.contains(&"instructions".to_string()), "{miss:?}");
        assert!(!miss.contains(&"contract".to_string()), "visible words must not be reported: {miss:?}");
        // identical → nothing hidden
        assert!(missing_words("all visible text here", "all visible text here").is_empty());
    }
}
