//! Bridge from proptest's retained law failures to the byte corpora.
//!
//! A `cc <fingerprint>` line is a proptest RNG seed plus a shrink comment.
//! It is not a decoder input. Copying the fingerprint into `corpus/` would
//! look like coverage and teach the fuzzer nothing about the wire grammar.

/// One minimized case retained by `tests/laws`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LawsRegression {
    /// Proptest RNG fingerprint. This is not wire input.
    pub fingerprint: String,
    /// Human shrink comment recorded beside the fingerprint.
    pub shrink: String,
}

/// Parses `cc <64 hex> # shrinks to ...` lines.
///
/// Comment-only lines and malformed rows are skipped so a header cannot
/// become a false seed.
#[must_use]
pub fn retained_law_cases(text: &str) -> Vec<LawsRegression> {
    text.lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("cc ")?;
            let (fingerprint, shrink) = rest.split_once(" # shrinks to ")?;
            if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return None;
            }
            Some(LawsRegression {
                fingerprint: fingerprint.to_string(),
                shrink: shrink.to_string(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_keeps_only_well_formed_regressions() {
        let text = "\
# header
cc e0ca73ee68505497160735a0903c1f59b5125a3370936343f79c96ac2971a3c8 # shrinks to raw = [RawOp { kind: 3 }]
not a seed
cc short # shrinks to raw = []
";
        let cases = retained_law_cases(text);
        assert_eq!(cases.len(), 1);
        assert!(cases[0].shrink.contains("RawOp"));
    }
}
