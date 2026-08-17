//! Compact Markdown projection for semantic-search results.
//!
//! This module is intentionally independent of the MCP transport and of the
//! embedding implementation. The search pipeline supplies already-ranked
//! symbols and exact documentation text; this layer only decides how that
//! information is made legible to an agent.
//!
//! A semantic result has four deliberately different public-to-the-renderer
//! states:
//!
//! * [`SemanticStatus::Ready`] — the covered corpus was searched and there are
//!   matches;
//! * [`SemanticStatus::Building`] — the rows are a real ranking over partial
//!   coverage, not a claim about the whole corpus;
//! * [`SemanticStatus::Unavailable`] — no semantic search was performed; and
//! * [`SemanticStatus::NoMatch`] — the complete searched corpus had no match.
//!
//! The renderer never invents a reason a symbol matched. Documentation is
//! caller-supplied evidence, clipped at a character boundary, and emitted
//! verbatim in a fenced text block. The only prose this module creates is
//! status, identity, and formatting metadata.

use std::collections::HashSet;

/// Why semantic search was unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SemanticUnavailable {
    /// No embedding model was installed in this build.
    NoEmbedder,
    /// There was no corpus to search.
    EmptyCorpus,
    /// The installed model failed while embedding the query.
    ModelFailed,
}

/// What a semantic result is entitled to claim about its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SemanticStatus {
    /// The complete in-scope corpus was searched and at least one row exists.
    Ready,
    /// Only `covered` of `total` in-scope packages were searched.
    Building { covered: u32, total: u32 },
    /// No rows were searched because the semantic facet could not run.
    Unavailable(SemanticUnavailable),
    /// The complete in-scope corpus was searched and produced no rows.
    NoMatch,
}

/// Exact documentation evidence attached to one semantic hit.
///
/// `text` is a prefix of the producer's documentation, never a summary. The
/// clipping marker lives outside the fenced block so the bytes inside the
/// block remain an exact slice of the source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DocumentationEvidence {
    text: String,
    clipped: bool,
}

impl DocumentationEvidence {
    /// Take at most `max_chars` Unicode scalar values from `text`.
    ///
    /// The cut is made on a `char` boundary. No ellipsis is appended: doing so
    /// would make the displayed evidence cease to be exact. A zero limit is
    /// valid and produces an empty, clipped excerpt for non-empty input.
    pub(crate) fn from_documentation(text: &str, max_chars: usize) -> Self {
        let end = text
            .char_indices()
            .nth(max_chars)
            .map_or(text.len(), |(index, _)| index);

        Self {
            text: text[..end].to_owned(),
            clipped: end < text.len(),
        }
    }

    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    pub(crate) fn is_clipped(&self) -> bool {
        self.clipped
    }
}

/// One ranked semantic hit.
///
/// `key` is the canonical stable symbol key and is retained in full. `score`
/// is optional because rank is the useful default agent-facing signal; raw
/// model scores are available only when a caller explicitly opts in at render
/// time.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SemanticHit {
    pub(crate) key: String,
    pub(crate) symbol: String,
    pub(crate) kind: String,
    pub(crate) evidence: Option<DocumentationEvidence>,
    pub(crate) score: Option<f32>,
}

impl SemanticHit {
    pub(crate) fn new(
        key: impl Into<String>,
        symbol: impl Into<String>,
        kind: impl Into<String>,
    ) -> Self {
        Self {
            key: key.into(),
            symbol: symbol.into(),
            kind: kind.into(),
            evidence: None,
            score: None,
        }
    }

    pub(crate) fn with_evidence(mut self, evidence: DocumentationEvidence) -> Self {
        self.evidence = Some(evidence);
        self
    }

    pub(crate) fn with_score(mut self, score: f32) -> Self {
        self.score = Some(score);
        self
    }
}

/// An optional applied semantic-search facet, such as `kind=fn`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SemanticFacet {
    pub(crate) name: String,
    pub(crate) value: String,
}

impl SemanticFacet {
    pub(crate) fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            value: value.into(),
        }
    }
}

/// Internal semantic result passed to the Markdown projection.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SemanticResult {
    pub(crate) query: String,
    pub(crate) status: SemanticStatus,
    pub(crate) hits: Vec<SemanticHit>,
    pub(crate) facets: Vec<SemanticFacet>,
}

impl SemanticResult {
    /// Construct a result while maintaining the useful empty-result invariant:
    /// a complete `Ready` result with no rows is rendered as `NoMatch`, and an
    /// explicitly empty result with rows is rendered as `Ready`.
    pub(crate) fn new(
        query: impl Into<String>,
        status: SemanticStatus,
        hits: Vec<SemanticHit>,
    ) -> Self {
        let status = match (status, hits.is_empty()) {
            (SemanticStatus::Ready, true) => SemanticStatus::NoMatch,
            (SemanticStatus::NoMatch, false) => SemanticStatus::Ready,
            (status, _) => status,
        };

        Self {
            query: query.into(),
            status,
            hits,
            facets: Vec::new(),
        }
    }

    pub(crate) fn ready(query: impl Into<String>, hits: Vec<SemanticHit>) -> Self {
        Self::new(query, SemanticStatus::Ready, hits)
    }

    pub(crate) fn building(
        query: impl Into<String>,
        covered: u32,
        total: u32,
        hits: Vec<SemanticHit>,
    ) -> Self {
        Self::new(query, SemanticStatus::Building { covered, total }, hits)
    }

    pub(crate) fn unavailable(query: impl Into<String>, reason: SemanticUnavailable) -> Self {
        Self::new(query, SemanticStatus::Unavailable(reason), Vec::new())
    }

    pub(crate) fn no_match(query: impl Into<String>) -> Self {
        Self::new(query, SemanticStatus::NoMatch, Vec::new())
    }

    pub(crate) fn with_facets(mut self, facets: impl IntoIterator<Item = SemanticFacet>) -> Self {
        self.facets = facets.into_iter().collect();
        self
    }
}

/// Rendering switches. Scores are deliberately opt-in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct SemanticRenderOptions {
    pub(crate) include_scores: bool,
}

/// Render a semantic result using the compact, score-free default projection.
pub(crate) fn render_semantic_markdown(result: &SemanticResult) -> String {
    render_semantic_markdown_with(result, SemanticRenderOptions::default())
}

/// Render a semantic result, optionally exposing finite raw scores.
pub(crate) fn render_semantic_markdown_with(
    result: &SemanticResult,
    options: SemanticRenderOptions,
) -> String {
    let visible_hits = visible_unique_hits(result);
    let mut output = String::new();

    output.push_str("## Semantic search\n\nquery: ");
    output.push_str(&inline_code(&single_line(&result.query)));
    output.push('\n');

    match result.status {
        SemanticStatus::Ready => {
            output.push_str("status: ready · ");
            output.push_str(&match_count(visible_hits.len()));
            output.push('\n');
        }
        SemanticStatus::Building { covered, total } => {
            output.push_str(&format!(
                "status: building · {covered}/{total} packages indexed · {}\n",
                if visible_hits.is_empty() {
                    "no matches in indexed coverage".to_owned()
                } else {
                    match_count(visible_hits.len())
                }
            ));
        }
        SemanticStatus::Unavailable(reason) => {
            output.push_str("status: unavailable · ");
            output.push_str(unavailable_label(reason));
            output.push('\n');
        }
        SemanticStatus::NoMatch => output.push_str("status: no matches\n"),
    }

    if !result.facets.is_empty() {
        output.push_str("facets: ");
        for (index, facet) in result.facets.iter().enumerate() {
            if index != 0 {
                output.push_str(", ");
            }
            output.push_str(&inline_code(&single_line(&facet.name)));
            output.push('=');
            output.push_str(&inline_code(&single_line(&facet.value)));
        }
        output.push('\n');
    }

    // An unavailable or complete-empty result must not accidentally display
    // stale rows supplied by a buggy caller. Building rows are safe to show,
    // but remain explicitly caveated by the status line above.
    if matches!(
        result.status,
        SemanticStatus::Unavailable(_) | SemanticStatus::NoMatch
    ) {
        return output;
    }

    if visible_hits.is_empty() {
        return output;
    }

    output.push('\n');
    output.push_str("| # | symbol | kind | key | doc");
    if options.include_scores {
        output.push_str(" | score");
    }
    output.push_str(" |\n|---:|---|---|---|---");
    if options.include_scores {
        output.push_str("|---:");
    }
    output.push_str("|\n");

    for (rank, hit) in visible_hits.iter().enumerate() {
        output.push('|');
        output.push_str(&format!(" {} |", rank + 1));
        output.push(' ');
        output.push_str(&table_cell(&hit.symbol));
        output.push_str(" | ");
        output.push_str(&table_cell(&hit.kind));
        output.push_str(" | ");
        output.push_str(&table_cell(&hit.key));
        output.push_str(" | ");
        output.push_str(match hit.evidence.as_ref() {
            Some(evidence) if evidence.is_clipped() => "clipped",
            Some(_) => "exact",
            None => "—",
        });
        if options.include_scores {
            output.push_str(" | ");
            output.push_str(&format_score(hit.score));
        }
        output.push_str(" |\n");
    }

    let evidence = visible_hits
        .iter()
        .enumerate()
        .filter_map(|(rank, hit)| hit.evidence.as_ref().map(|evidence| (rank + 1, evidence)));
    let evidence: Vec<_> = evidence.collect();
    if !evidence.is_empty() {
        output.push_str("\n### Documentation\n\n");
        for (rank, evidence) in evidence {
            output.push_str(&format!(
                "{}. {}\n\n",
                rank,
                if evidence.is_clipped() {
                    "clipped"
                } else {
                    "exact"
                }
            ));
            output.push_str(&text_fence(evidence.text()));
            output.push_str("\n\n");
        }
    }

    output
}

/// Return the first row for each stable key, retaining the search pipeline's
/// ranking order. A duplicate key is one symbol, even if upstream facets
/// emitted it more than once.
fn visible_unique_hits(result: &SemanticResult) -> Vec<&SemanticHit> {
    let mut seen = HashSet::with_capacity(result.hits.len());
    result
        .hits
        .iter()
        .filter(|hit| seen.insert(hit.key.as_str()))
        .collect()
}

fn match_count(count: usize) -> String {
    if count == 1 {
        "1 match".to_owned()
    } else {
        format!("{count} matches")
    }
}

fn unavailable_label(reason: SemanticUnavailable) -> &'static str {
    match reason {
        SemanticUnavailable::NoEmbedder => "no embedder",
        SemanticUnavailable::EmptyCorpus => "empty corpus",
        SemanticUnavailable::ModelFailed => "model failed",
    }
}

fn format_score(score: Option<f32>) -> String {
    match score {
        Some(value) if value.is_finite() => format!("{value:.3}"),
        Some(_) | None => "—".to_owned(),
    }
}

/// Keep a one-line field from opening a new Markdown row. Documentation is
/// handled separately and is never passed through this lossy display helper.
fn single_line(value: &str) -> String {
    value
        .chars()
        .map(|character| match character {
            '\n' | '\r' => '↵',
            character => character,
        })
        .collect()
}

/// Render a compact inline value, choosing a delimiter longer than any run of
/// backticks in the value. This keeps adversarial names, keys, facet values,
/// and queries from terminating their own code span.
fn inline_code(value: &str) -> String {
    if value.is_empty() {
        return "``".to_owned();
    }

    let run = longest_backtick_run(value);
    let delimiter = "`".repeat(if run == 0 { 1 } else { run + 1 });
    let padded = value.starts_with(' ') || value.ends_with(' ');
    if padded {
        format!("{delimiter} {value} {delimiter}")
    } else {
        format!("{delimiter}{value}{delimiter}")
    }
}

/// Escape table fields without changing the exact evidence blocks below them.
fn table_cell(value: &str) -> String {
    let value = single_line(value);
    if value.contains('|') {
        escape_table_text(&value)
    } else {
        inline_code(&value)
    }
}

fn escape_table_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' | '`' | '*' | '_' | '[' | ']' | '<' | '>' | '|' => {
                output.push('\\');
                output.push(character);
            }
            _ => output.push(character),
        }
    }
    output
}

/// Use a fence longer than every backtick run in `value`, so embedded Markdown
/// examples cannot close the evidence block early. The contents between the
/// opening and closing line are otherwise emitted unchanged.
fn text_fence(value: &str) -> String {
    let delimiter = "`".repeat(longest_backtick_run(value).max(3) + 1);
    format!("{delimiter}text\n{value}\n{delimiter}")
}

fn longest_backtick_run(value: &str) -> usize {
    let mut longest = 0;
    let mut current = 0;
    for character in value.chars() {
        if character == '`' {
            current += 1;
            longest = longest.max(current);
        } else {
            current = 0;
        }
    }
    longest
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(key: &str, symbol: &str) -> SemanticHit {
        SemanticHit::new(key, symbol, "fn")
    }

    #[test]
    fn statuses_never_collapse_into_empty_results() {
        let ready = render_semantic_markdown(&SemanticResult::ready(
            "retry requests",
            vec![hit("cargo:req#1", "retry")],
        ));
        let building = render_semantic_markdown(&SemanticResult::building(
            "retry requests",
            2,
            5,
            vec![hit("cargo:req#1", "retry")],
        ));
        let unavailable = render_semantic_markdown(&SemanticResult::unavailable(
            "retry requests",
            SemanticUnavailable::NoEmbedder,
        ));
        let no_match = render_semantic_markdown(&SemanticResult::no_match("retry requests"));

        assert!(ready.contains("status: ready · 1 match"), "{ready}");
        assert!(building.contains("status: building · 2/5 packages indexed · 1 match"));
        assert!(unavailable.contains("status: unavailable · no embedder"));
        assert!(no_match.contains("status: no matches"));
        assert!(!unavailable.contains("status: no matches"));
        assert!(!building.contains("status: no matches"));
        assert!(!unavailable.contains("| # | symbol |"));
    }

    #[test]
    fn unavailable_reasons_remain_distinguishable() {
        let empty_corpus = render_semantic_markdown(&SemanticResult::unavailable(
            "retry",
            SemanticUnavailable::EmptyCorpus,
        ));
        let model_failed = render_semantic_markdown(&SemanticResult::unavailable(
            "retry",
            SemanticUnavailable::ModelFailed,
        ));

        assert!(empty_corpus.contains("status: unavailable · empty corpus"));
        assert!(model_failed.contains("status: unavailable · model failed"));
        assert!(!empty_corpus.contains("no embedder"));
        assert!(!model_failed.contains("empty corpus"));
    }

    #[test]
    fn documentation_preserves_markdown_metacharacters_newlines_and_backticks() {
        let documentation =
            "# Heading\n\nUse | pipes, *stars*, [links](url), and ```rust\nfn x() {}\n```\n終";
        let result = SemanticResult::ready(
            "find parser docs",
            vec![hit("cargo:docs#1", "parser").with_evidence(
                DocumentationEvidence::from_documentation(documentation, 10_000),
            )],
        );
        let rendered = render_semantic_markdown(&result);

        assert!(rendered.contains("| 1 | `parser` | `fn` | `cargo:docs#1` | exact |"));
        assert!(rendered.contains(documentation), "{rendered}");
        assert!(
            rendered.contains("````text"),
            "the embedded triple fence needs protection"
        );
        assert!(rendered.contains("# Heading\n\nUse | pipes"));
        assert!(
            !rendered.contains("because"),
            "the renderer must not fabricate an explanation"
        );
    }

    #[test]
    fn documentation_clips_on_character_boundaries_without_an_ellipsis() {
        let evidence = DocumentationEvidence::from_documentation("éclair — retry", 3);
        assert_eq!(evidence.text(), "écl");
        assert!(evidence.is_clipped());

        let rendered = render_semantic_markdown(&SemanticResult::ready(
            "retry",
            vec![hit("cargo:docs#1", "retry").with_evidence(evidence)],
        ));
        assert!(rendered.contains("| clipped |"));
        assert!(rendered.contains("écl"));
        assert!(!rendered.contains("écl…"));
    }

    #[test]
    fn duplicate_rows_are_deduplicated_by_stable_key_and_keep_rank_order() {
        let result = SemanticResult::ready(
            "retry",
            vec![
                hit("cargo:req#1", "first"),
                hit("cargo:req#1", "duplicate"),
                hit("cargo:req#2", "second"),
            ],
        );
        let rendered = render_semantic_markdown(&result);

        assert!(rendered.contains("status: ready · 2 matches"));
        assert_eq!(rendered.matches("cargo:req#1").count(), 1);
        assert!(rendered.contains("| 1 | `first` |"));
        assert!(!rendered.contains("duplicate"));
    }

    #[test]
    fn empty_complete_results_have_no_empty_table() {
        let rendered = render_semantic_markdown(&SemanticResult::ready("unknown concept", vec![]));

        assert!(rendered.contains("status: no matches"));
        assert!(!rendered.contains("| # | symbol | kind | key | doc |"));
    }

    #[test]
    fn partial_coverage_is_not_reported_as_no_match() {
        let rendered =
            render_semantic_markdown(&SemanticResult::building("backoff", 1, 4, Vec::new()));

        assert!(rendered.contains("status: building · 1/4 packages indexed"));
        assert!(rendered.contains("no matches in indexed coverage"));
        assert!(!rendered.contains("status: no matches"));
    }

    #[test]
    fn facets_are_optional_and_scores_are_opt_in() {
        let result = SemanticResult::ready(
            "retry",
            vec![hit("cargo:req#1", "retry").with_score(0.98765)],
        )
        .with_facets([
            SemanticFacet::new("kind", "fn"),
            SemanticFacet::new("package", "cargo:req"),
        ]);

        let compact = render_semantic_markdown(&result);
        assert!(compact.contains("facets: `kind`=`fn`, `package`=`cargo:req`"));
        assert!(!compact.contains("score"));
        assert!(!compact.contains("0.988"));

        let scored = render_semantic_markdown_with(
            &result,
            SemanticRenderOptions {
                include_scores: true,
            },
        );
        assert!(scored.contains("| score |"));
        assert!(scored.contains("| 0.988 |"));
    }

    #[test]
    fn malformed_scores_are_not_rendered_as_numbers() {
        let result = SemanticResult::ready(
            "retry",
            vec![hit("cargo:req#1", "retry").with_score(f32::NAN)],
        );
        let rendered = render_semantic_markdown_with(
            &result,
            SemanticRenderOptions {
                include_scores: true,
            },
        );

        assert!(rendered.contains("| — |"));
        assert!(!rendered.contains("NaN"));
    }
}
