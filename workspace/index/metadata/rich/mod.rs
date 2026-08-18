//! Rich metadata extraction — ingest inputs → weighted keywords, quality.
//!
//! Turns a package's ingest inputs ([`ExtractionInput`]) into weighted keywords
//! and a quality score ([`RichMetadata`]). Only workspace items used are
//! [`crate::metadata::heuristics`] (normalize_keyword, Synonyms, Specifics) and
//! [`crate::ecosystem::search::SearchNorms`] (per-ecosystem stopwords).

mod keywords;
mod quality;
mod score;
mod types;

#[cfg(test)]
mod tests;

pub use score::{Score, ScoreAdjustment};
pub use types::{ExtractionInput, RichMetadata, SearchFacets};

use std::collections::HashMap;

use smol_str::SmolStr;

use crate::ecosystem::search::SearchNorms;
use crate::metadata::heuristics::{Specifics, Synonyms, normalize_keyword};

use keywords::{
    apply_synonyms_and_specifics, combine_weights, derive_categories, ident_words, is_stopword,
    prose_words, readme_relevant_text,
};
use quality::compute_quality;

/// Extract rich metadata from the given input.
///
/// `norms` is the per-ecosystem [`SearchNorms`] whose `is_stopword` governs
/// prose/identifier filtering. Pass `crate::ecosystem::spec(lang).search_norms()` at
/// the call site (R3).
pub fn extract(
    input: &ExtractionInput<'_>,
    norms: &'static SearchNorms,
    synonyms: Option<&Synonyms>,
    specifics: Option<&Specifics>,
) -> RichMetadata {
    let mut bag: HashMap<SmolStr, f32> = HashMap::new();

    /// Insert or combine into `bag`.
    macro_rules! add {
        ($kw:expr, $w:expr) => {{
            let kw: SmolStr = $kw;
            let w: f32 = $w;
            if !kw.is_empty() && kw.len() >= 2 {
                let entry = bag.entry(kw).or_insert(0.0);
                *entry = combine_weights(*entry, w);
            }
        }};
    }

    // 1. Manifest keywords (weight 1.0).
    for kw in input.manifest_keywords {
        let norm = normalize_keyword(kw.as_str());
        if !norm.is_empty() {
            add!(norm, 1.0);
        }
    }

    // 2. Manifest categories (weight 0.7).
    for cat in input.manifest_categories {
        let norm = normalize_keyword(cat.as_str());
        if !norm.is_empty() {
            add!(norm, 0.7);
        }
    }

    // 3. Package name parts (weight 0.6, hidden — kept if len > 2 and not obviously generic).
    let name_skip = ["rs", "impl", "internal", "shared"];
    for part in input.name.split(|c: char| !c.is_ascii_alphanumeric()) {
        if part.len() <= 2 || name_skip.contains(&part) {
            continue;
        }
        let norm = normalize_keyword(part);
        if !norm.is_empty() && norm.len() >= 2 && !is_stopword(norm.as_str(), norms) {
            add!(norm, 0.6);
        }
    }

    // 4. Description words (weight 0.6).
    if let Some(desc) = input.description {
        for (kw, w) in prose_words(desc, 0.6, norms) {
            add!(kw, w);
        }
    }

    // 5. README words (weight 0.3, skip boilerplate sections).
    if let Some(readme) = input.readme {
        for (text, section_w) in readme_relevant_text(readme) {
            for (kw, w) in prose_words(text, 0.3 * section_w, norms) {
                add!(kw, w);
            }
        }
    }

    // 6. Source identifiers (weight 0.25).
    for ident in input.identifiers {
        for (kw, w) in ident_words(ident.as_str(), 0.25, norms) {
            add!(kw, w);
        }
    }

    // 7. Dependency keywords as `dep:name` (weight 0.2, invisible).
    for dep in input.dependencies {
        let dep_kw = SmolStr::from(format!("dep:{dep}"));
        add!(dep_kw, 0.2);
    }

    // Apply synonyms + specifics.
    apply_synonyms_and_specifics(&mut bag, synonyms, specifics);

    // Remove dep: keywords from visible output and any remaining stopwords.
    bag.retain(|k, _| !k.starts_with("dep:") && !is_stopword(k.as_str(), norms));

    // Sort descending by weight, cap at 20.
    let mut keywords: Vec<(f32, SmolStr)> = bag.into_iter().map(|(k, w)| (w, k)).collect();
    keywords.sort_by(|a, b| b.0.total_cmp(&a.0));
    keywords.truncate(20);

    // Normalize so the top keyword = 1.0.
    if let Some(&(top_w, _)) = keywords.first()
        && top_w > 0.0
    {
        for (w, _) in &mut keywords {
            *w = (*w / top_w).clamp(0.0, 1.0);
        }
    }

    // Categories: from manifest, else infer from keywords.
    let categories = derive_categories(input, &keywords);

    let quality = compute_quality(input);

    // Dependency slugs: lowercase + trim, deduplicated, sorted for determinism.
    // No `normalize_keyword` — names must round-trip exactly for the reverse-dep join.
    let mut dependencies: Vec<SmolStr> = input
        .dependencies
        .iter()
        .map(|d| SmolStr::from(d.trim().to_ascii_lowercase()))
        .filter(|d| !d.is_empty())
        .collect();
    dependencies.sort_unstable();
    dependencies.dedup();

    RichMetadata {
        keywords,
        categories,
        quality,
        dependencies,
    }
}
