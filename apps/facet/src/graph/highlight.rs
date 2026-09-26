//! Search highlights refer to the original UTF-8 text, even when lowercasing
//! expands a character. GPUI's text runs require original byte boundaries.

use std::ops::Range;

/// The first case-insensitive match, expressed in the original text's bytes.
/// A match inside a lowercase expansion highlights its entire source character.
pub(crate) fn match_range(text: &str, query: &str) -> Option<Range<usize>> {
    if query.is_empty() {
        return None;
    }
    let folded = text.to_lowercase();
    let mut spans = Vec::with_capacity(text.chars().count());
    let mut folded_at = 0;
    for (at, ch) in text.char_indices() {
        let folded_end = folded_at + ch.to_lowercase().map(char::len_utf8).sum::<usize>();
        spans.push((folded_at..folded_end, at..at + ch.len_utf8()));
        folded_at = folded_end;
    }
    let query = query.to_lowercase();
    let start = folded.find(&query)?;
    let end = start + query.len();
    let original_start = spans
        .iter()
        .find(|(folded, _)| folded.contains(&start))?
        .1
        .start;
    let original_end = spans
        .iter()
        .find(|(folded, _)| folded.contains(&(end - 1)))?
        .1
        .end;
    Some(original_start..original_end)
}

#[cfg(test)]
mod tests {
    use super::match_range;

    #[test]
    fn lowercase_expansions_keep_original_text_run_boundaries() {
        assert_eq!(match_range("İtem", "i"), Some(0..2));
        assert_eq!(match_range("İtem", "\u{307}"), Some(0..2));
        assert_eq!(match_range("İtem", "i\u{307}t"), Some(0..3));
        assert_eq!(match_range("İtem", "tem"), Some(2..5));
    }

    #[test]
    fn unicode_prefixes_and_matches_use_original_offsets() {
        assert_eq!(match_range("İ::Σχήμα", "σχ"), Some(4..8));
        assert_eq!(match_range("é::Value", "VALUE"), Some(4..9));
        assert_eq!(match_range("ΟΣ", "ος"), Some(0..4));
        assert_eq!(match_range("Value", ""), None);
        assert_eq!(match_range("Value", "other"), None);
    }

    #[test]
    fn every_nonempty_folded_fragment_produces_valid_original_runs() {
        for text in ["İtem", "İ::Σχήμα", "é::Value", "Straße", "東京::値", "AKZ"] {
            let folded = text.to_lowercase();
            let boundaries: Vec<_> = folded
                .char_indices()
                .map(|(i, _)| i)
                .chain([folded.len()])
                .collect();
            for (n, &start) in boundaries.iter().enumerate() {
                for &end in &boundaries[n + 1..] {
                    let range = match_range(text, &folded[start..end])
                        .expect("a folded fragment must match");
                    assert!(text.is_char_boundary(range.start));
                    assert!(text.is_char_boundary(range.end));
                    assert!(range.start < range.end);
                    assert!(range.end <= text.len());
                }
            }
        }
    }
}
