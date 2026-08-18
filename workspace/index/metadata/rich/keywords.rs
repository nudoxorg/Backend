//! Keyword extraction: prose/identifier tokenization, README weighting.

use std::collections::{HashMap, HashSet};

use smol_str::SmolStr;

use crate::ecosystem::search::SearchNorms;
use crate::metadata::heuristics::{Specifics, Synonyms, normalize_keyword};

use super::types::ExtractionInput;

/// Identifier-specific stopwords (code structure words with no semantic meaning
/// in search context). These are LOCAL to `rich`: they supplement the shared
/// English + ecosystem stopwords for the identifier extraction path only.
const IDENT_STOPWORDS: &[&str] = &[
    "add", "app", "as", "bench", "benches", "build", "check", "clone", "close", "convert", "copy",
    "core", "count", "create", "debug", "default", "delete", "drop", "enum", "eq", "err", "false",
    "find", "fn", "format", "from", "get", "has", "hash", "helper", "impl", "index", "init",
    "inner", "insert", "into", "is", "iter", "len", "lib", "main", "make", "misc", "mock", "mod",
    "mut", "new", "none", "ok", "open", "ord", "outer", "parse", "partial", "pos", "ptr", "pub",
    "raw", "read", "recv", "ref", "remove", "run", "search", "self", "send", "set", "size", "some",
    "start", "stop", "str", "struct", "test", "tests", "to", "true", "try", "update", "util",
    "utils", "validate", "with", "write",
];

/// README section headers to skip (lowercased, trimmed).
const SKIP_SECTIONS: &[&str] = &[
    "license",
    "licensing",
    "contributing",
    "contribution",
    "contributions",
    "installation",
    "install",
    "installing",
    "changelog",
    "change log",
    "credits",
    "acknowledgements",
    "acknowledgement",
    "acknowledgments",
    "author",
    "authors",
    "todo",
    "code of conduct",
    "building",
    "build",
    "setup",
    "msrv",
    "minimum supported rust",
    "semantic versioning",
    "copyright",
    "sponsor",
    "sponsors",
    "sponsoring",
];

/// Well-known category slugs used for inference when none declared.
const KNOWN_CATEGORIES: &[&str] = &[
    "async-io",
    "parser",
    "web-programming",
    "command-line-utilities",
    "encoding",
    "compression",
    "cryptography",
    "data-structures",
    "algorithms",
    "science",
    "embedded",
    "wasm",
    "graphics",
    "database",
    "network-programming",
];

pub(crate) fn is_stopword(w: &str, norms: &SearchNorms) -> bool {
    norms.is_stopword(w)
}

fn is_ident_stopword(w: &str, norms: &SearchNorms) -> bool {
    IDENT_STOPWORDS.binary_search(&w).is_ok() || norms.is_stopword(w)
}

/// Split a prose string into word candidates, normalize each, and yield
/// `(normalized, raw_weight)` pairs where `raw_weight` is the caller-supplied
/// source weight. Stopword filtering is delegated to `norms` (R3).
pub(crate) fn prose_words<'a>(
    text: &'a str,
    weight: f32,
    norms: &'a SearchNorms,
) -> impl Iterator<Item = (SmolStr, f32)> + 'a {
    text.split(|c: char| {
        c.is_ascii_whitespace()
            || matches!(
                c,
                '.' | ',' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | ';' | '!' | '?'
            )
    })
    .filter(|w| w.len() >= 2)
    .map(normalize_keyword)
    .filter(|w| !w.is_empty() && w.len() >= 2)
    .filter(move |w| !is_stopword(w.as_str(), norms))
    .map(move |w| (w, weight))
}

/// Split an identifier into its component words (snake_case split, camelCase
/// split), strip prefixes/suffixes, and normalize. Stopword filtering uses
/// `norms` (R3).
pub(crate) fn ident_words(ident: &str, weight: f32, norms: &SearchNorms) -> Vec<(SmolStr, f32)> {
    // Convert camelCase → snake_case first by inserting underscores before
    // uppercase runs.
    let snake = camel_to_snake(ident);

    // Strip common method prefixes.
    let prefixes = [
        "get_", "set_", "is_", "as_", "to_", "try_", "into_", "from_", "with_", "new_",
    ];
    let mut s: &str = snake.as_str();
    for p in &prefixes {
        if let Some(rest) = s.strip_prefix(p) {
            s = rest;
            break;
        }
    }

    // Strip common suffixes.
    let suffixes = ["_ref", "_mut", "_iter", "_t", "_str"];
    for suf in &suffixes {
        if let Some(rest) = s.strip_suffix(suf) {
            s = rest;
            break;
        }
    }

    s.split('_')
        .filter(|w| w.len() >= 2)
        .map(normalize_keyword)
        .filter(|w| !w.is_empty() && w.len() >= 2)
        .filter(|w| !is_ident_stopword(w.as_str(), norms))
        .map(|w| (w, weight))
        .collect()
}

/// Convert camelCase to snake_case using heck.
fn camel_to_snake(s: &str) -> String {
    heck::AsSnakeCase(s).to_string()
}

/// Combine weight from a new source into an existing entry: prefer the larger,
/// add 30 % of the smaller (multi-source agreement bonus).
pub(crate) fn combine_weights(existing: f32, new: f32) -> f32 {
    let hi = existing.max(new);
    let lo = existing.min(new);
    lo.mul_add(0.3, hi)
}

/// Parse README into sections and return `(text, weight)` pairs for relevant
/// sections only.
pub(crate) fn readme_relevant_text(readme: &str) -> Vec<(&str, f32)> {
    let mut sections: Vec<(&str, f32)> = Vec::new();
    let mut current_header: Option<&str> = None;
    let mut section_start = 0usize;
    let mut pos = 0usize;

    for line in readme.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            // Emit the previous section body.
            let body = readme.get(section_start..pos).unwrap_or("").trim();
            let weight = section_weight(current_header);
            if weight > 0.0 && !body.is_empty() {
                sections.push((body, weight));
            }
            // Advance to new section.
            let header = trimmed.trim_start_matches('#').trim();
            current_header = Some(header);
            section_start = pos + line.len(); // No need to + 1, \n is included
        }
        pos += line.len();
    }
    // Emit the last section.
    let body = readme.get(section_start..).unwrap_or("").trim();
    let weight = section_weight(current_header);
    if weight > 0.0 && !body.is_empty() {
        sections.push((body, weight));
    }

    sections
}

/// Return the prose weight for a README section with the given header.
/// Returns 0.0 for sections that should be skipped entirely.
fn section_weight(header: Option<&str>) -> f32 {
    let h = match header {
        None => return 0.3, // preamble before any heading
        Some(h) => h.to_lowercase(),
    };
    let h = h.trim();

    // Skip boilerplate sections.
    for skip in SKIP_SECTIONS {
        if h == *skip || h.contains(skip) {
            return 0.0;
        }
    }

    if matches!(
        h,
        "overview"
            | "about"
            | "summary"
            | "introduction"
            | "description"
            | "synopsis"
            | "what is this"
            | "motivation"
    ) {
        return 0.45;
    }
    if h.starts_with("example")
        || h.starts_with("usage")
        || h.starts_with("feature")
        || h == "features"
    {
        return 0.35;
    }
    if matches!(
        h,
        "getting started"
            | "quick start"
            | "quickstart"
            | "documentation"
            | "how it works"
            | "how to use"
    ) {
        return 0.25;
    }

    0.3
}

pub(crate) fn apply_synonyms_and_specifics(
    bag: &mut HashMap<SmolStr, f32>,
    synonyms: Option<&Synonyms>,
    specifics: Option<&Specifics>,
) {
    if synonyms.is_none() && specifics.is_none() {
        return;
    }

    // Collect (old_key, new_key, new_weight) to avoid borrow issues.
    let mut remap: Vec<(SmolStr, SmolStr, f32)> = Vec::new();
    for (kw, &w) in bag.iter() {
        let mut current = kw.clone();
        let mut current_w = w;

        if let Some(syn) = synonyms {
            let (canonical, syn_w) = syn.normalize(current.as_str(), 3);
            if canonical != current.as_str() {
                current = SmolStr::from(canonical);
                current_w *= syn_w;
            }
        }

        if let Some(sp) = specifics
            && sp.is_bland(current.as_str()).is_some()
        {
            current_w *= 0.3;
        }

        if current != *kw || (current_w - w).abs() > 1e-6 {
            remap.push((kw.clone(), current, current_w));
        }
    }

    for (old, new, new_w) in remap {
        bag.remove(&old);
        let entry = bag.entry(new).or_insert(0.0);
        *entry = combine_weights(*entry, new_w);
    }
}

pub(crate) fn derive_categories(
    input: &ExtractionInput<'_>,
    keywords: &[(f32, SmolStr)],
) -> Vec<(f32, SmolStr)> {
    if !input.manifest_categories.is_empty() {
        return input
            .manifest_categories
            .iter()
            .take(3)
            .map(|c| (1.0f32, normalize_keyword(c.as_str())))
            .filter(|(_, k)| !k.is_empty())
            .collect();
    }

    // Infer from keyword overlap with known categories.
    let kw_set: HashSet<&str> = keywords.iter().map(|(_, k)| k.as_str()).collect();
    let cats: Vec<(f32, SmolStr)> = KNOWN_CATEGORIES
        .iter()
        .filter(|&&cat| kw_set.contains(cat))
        .map(|&cat| (0.5f32, SmolStr::from(cat)))
        .take(3)
        .collect();
    cats
}
