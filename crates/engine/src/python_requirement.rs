//! Full-grammar parsing for statically declared Python dependencies.

use pep508_rs::marker::{MarkerTree, MarkerTreeKind, MarkerValueExtra};
use pep508_rs::{MarkerWarningKind, Requirement, VerbatimUrl};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Parsed facts used to project a Python requirement into the package graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ParsedPythonRequirement {
    /// PEP 503 normalized target distribution name.
    pub(crate) package_name: String,
    /// The marker cannot apply with no requested extra.
    pub(crate) optional_by_extra: bool,
    /// Exactly one non-wildcard equality specifier, when the declaration pins a release.
    pub(crate) pinned_version: Option<String>,
}

/// Why a requirement cannot safely contribute to a complete package graph.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PythonRequirementParseError {
    /// The complete input was not accepted by the PEP 508 parser.
    InvalidRequirement,
    /// The parser accepted syntax with marker warnings, so its semantics are not trusted.
    UnsupportedMarkerSemantics,
    /// A conservative parser or normalized-marker work budget was exceeded.
    MarkerComplexityLimitExceeded,
}

const MAX_REQUIREMENT_BYTES: usize = 8192;
const MAX_OPEN_PARENTHESES: usize = 64;
const MAX_PREPARSE_MARKER_BOOLEAN_TOKENS: usize = 12;
const MAX_MARKER_DAG_NODES: usize = 4096;
const MAX_MARKER_DAG_EDGES: usize = 16_384;

/// Defend the pinned parser's `PackageName`/`ExtraName` constructor invariants.
/// Its lexer accepts trailing separators that those constructors reject with
/// `expect`. This admission only bounds name tokens; the full parser still
/// validates every requirement, extra list, URL, specifier and marker.
fn has_admitted_name_tokens(input: &str) -> bool {
    let input = input.trim_start();
    let name_end = input
        .bytes()
        .take_while(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.'))
        .count();
    if !crate::registry::valid_pypi_name(&input[..name_end]) {
        return false;
    }
    let remainder = input[name_end..].trim_start();
    let Some(extras) = remainder.strip_prefix('[') else {
        return true;
    };
    let Some((extras, _)) = extras.split_once(']') else {
        return false;
    };
    extras.trim().is_empty()
        || extras
            .split(',')
            .all(|extra| crate::registry::valid_pypi_name(extra.trim()))
}

/// Apply a conservative parser-safety bound without parsing marker syntax.
/// Counts raw opening parentheses and standalone boolean words across the
/// requirement, including URLs and quoted strings. The 12-token ceiling is the
/// currently supported subset while the parser has no incremental construction
/// budget; full normalized-DAG work is separately capped after parsing.
fn has_bounded_marker_complexity(input: &str) -> bool {
    let mut open_parentheses = 0;
    let mut logical_operators = 0;
    let mut token = String::new();

    for character in input.chars() {
        if character.is_ascii_alphanumeric() || character == '_' {
            token.push(character);
            continue;
        }

        if matches!(token.as_str(), "and" | "or") {
            logical_operators += 1;
            if logical_operators > MAX_PREPARSE_MARKER_BOOLEAN_TOKENS {
                return false;
            }
        }
        token.clear();

        if character == '(' {
            open_parentheses += 1;
            if open_parentheses > MAX_OPEN_PARENTHESES {
                return false;
            }
        }
    }

    if matches!(token.as_str(), "and" | "or") {
        logical_operators += 1;
    }

    logical_operators <= MAX_PREPARSE_MARKER_BOOLEAN_TOKENS
}

/// Counts each unique canonical marker node and every outgoing edge.
///
/// This traverses both outcomes of every node, including branches that the
/// later empty-extra applicability walk can short-circuit.
fn marker_dag_within_budget(
    marker: &MarkerTree,
    max_nodes: usize,
    max_edges: usize,
) -> Result<(), PythonRequirementParseError> {
    let mut pending = vec![marker.clone()];
    let mut seen = HashSet::new();
    let mut edge_count = 0;

    while let Some(node) = pending.pop() {
        if !seen.insert(node.clone()) {
            continue;
        }
        if seen.len() > max_nodes {
            return Err(PythonRequirementParseError::MarkerComplexityLimitExceeded);
        }

        let mut record_edge = |edge: MarkerTree| {
            edge_count += 1;
            if edge_count > max_edges {
                return Err(PythonRequirementParseError::MarkerComplexityLimitExceeded);
            }
            pending.push(edge);
            Ok(())
        };

        match node.kind() {
            MarkerTreeKind::True | MarkerTreeKind::False => {}
            MarkerTreeKind::Version(version) => {
                for (_, edge) in version.edges() {
                    record_edge(edge)?;
                }
            }
            MarkerTreeKind::String(string) => {
                for (_, edge) in string.children() {
                    record_edge(edge)?;
                }
            }
            MarkerTreeKind::In(string) => {
                for (_, edge) in string.children() {
                    record_edge(edge)?;
                }
            }
            MarkerTreeKind::Contains(string) => {
                for (_, edge) in string.children() {
                    record_edge(edge)?;
                }
            }
            MarkerTreeKind::Extra(extra) => {
                for (_, edge) in extra.children() {
                    record_edge(edge)?;
                }
            }
        }
    }

    Ok(())
}

/// Checks whether a requirement can apply with no requested extras.
///
/// Environment marker nodes are existentially traversed because this graph
/// does not bind a Python environment. At an extra node, the empty base extra
/// matches only the explicitly empty marker value; named extras are absent.
/// The full canonical marker DAG and its outgoing edges are budgeted first;
/// memoization then keeps the semantic traversal bounded by that DAG.
fn marker_may_apply_without_extra(
    marker: &MarkerTree,
) -> Result<bool, PythonRequirementParseError> {
    marker_may_apply_without_extra_with_budget(marker, MAX_MARKER_DAG_NODES)
}

fn marker_may_apply_without_extra_with_budget(
    marker: &MarkerTree,
    max_nodes: usize,
) -> Result<bool, PythonRequirementParseError> {
    marker_dag_within_budget(marker, max_nodes, MAX_MARKER_DAG_EDGES)?;

    fn any_applicable(
        edges: impl Iterator<Item = MarkerTree>,
        visited: &mut HashMap<MarkerTree, bool>,
    ) -> Result<bool, PythonRequirementParseError> {
        for edge in edges {
            if visit(&edge, visited)? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn visit(
        marker: &MarkerTree,
        visited: &mut HashMap<MarkerTree, bool>,
    ) -> Result<bool, PythonRequirementParseError> {
        if let Some(result) = visited.get(marker) {
            return Ok(*result);
        }

        let result = match marker.kind() {
            MarkerTreeKind::True => true,
            MarkerTreeKind::False => false,
            MarkerTreeKind::Version(version) => {
                any_applicable(version.edges().map(|(_, edge)| edge), visited)?
            }
            MarkerTreeKind::String(string) => {
                any_applicable(string.children().map(|(_, edge)| edge), visited)?
            }
            MarkerTreeKind::In(string) => {
                any_applicable(string.children().map(|(_, edge)| edge), visited)?
            }
            MarkerTreeKind::Contains(string) => {
                any_applicable(string.children().map(|(_, edge)| edge), visited)?
            }
            MarkerTreeKind::Extra(extra) => {
                let matches_empty_base = match extra.name() {
                    MarkerValueExtra::Extra(value) => value.as_ref().is_empty(),
                    MarkerValueExtra::Arbitrary(value) => value.is_empty(),
                };
                visit(&extra.edge(matches_empty_base), visited)?
            }
        };
        visited.insert(marker.clone(), result);
        Ok(result)
    }

    visit(marker, &mut HashMap::new())
}

/// Parses one complete requirement without rewriting its source spelling.
///
/// Callers retain the original string separately for graph identity and display.
/// This adapter returns only a validated normalized target and conservative
/// optional-marker evidence.
pub(crate) fn parse_python_requirement(
    input: &str,
) -> Result<ParsedPythonRequirement, PythonRequirementParseError> {
    if input.is_empty() || input.len() > MAX_REQUIREMENT_BYTES || input.contains(['\n', '\r', '\0'])
    {
        return Err(PythonRequirementParseError::InvalidRequirement);
    }
    if !has_admitted_name_tokens(input) {
        return Err(PythonRequirementParseError::InvalidRequirement);
    }
    if !has_bounded_marker_complexity(input) {
        return Err(PythonRequirementParseError::MarkerComplexityLimitExceeded);
    }

    let mut warnings = Vec::new();
    let parsed = {
        let mut reporter = |kind: MarkerWarningKind, warning: String| {
            warnings.push((kind, warning));
        };
        Requirement::<VerbatimUrl>::parse_reporter(input, Path::new("/"), &mut reporter)
            .map_err(|_| PythonRequirementParseError::InvalidRequirement)?
    };
    if !warnings.is_empty() {
        return Err(PythonRequirementParseError::UnsupportedMarkerSemantics);
    }
    if parsed.marker.is_false() {
        return Err(PythonRequirementParseError::UnsupportedMarkerSemantics);
    }

    let optional_by_extra = !marker_may_apply_without_extra(&parsed.marker)?;

    let pinned_version = match &parsed.version_or_url {
        Some(pep508_rs::VersionOrUrl::VersionSpecifier(specifiers)) => {
            let mut items = specifiers.iter();
            items
                .next()
                .filter(|item| {
                    item.operator().to_string() == "=="
                        && !item.to_string().contains('*')
                        && items.next().is_none()
                })
                .map(|item| item.version().to_string())
        }
        _ => None,
    };

    Ok(ParsedPythonRequirement {
        package_name: parsed.name.as_ref().to_owned(),
        optional_by_extra,
        pinned_version,
    })
}

#[cfg(test)]
mod tests {
    use pep508_rs::marker::MarkerTree;

    use super::{
        MAX_MARKER_DAG_EDGES, MAX_MARKER_DAG_NODES, MAX_OPEN_PARENTHESES,
        MAX_PREPARSE_MARKER_BOOLEAN_TOKENS, PythonRequirementParseError,
        has_bounded_marker_complexity, marker_dag_within_budget,
        marker_may_apply_without_extra_with_budget, parse_python_requirement,
    };

    #[test]
    fn parser_name_constructor_invariants_are_admitted_before_parsing()
    -> Result<(), PythonRequirementParseError> {
        for requirement in [
            "evil.>=1",
            "evil->=1",
            "evil_>=1",
            "evil.$bad>=1",
            "requests[docs.]",
            "requests[docs-]",
            "requests[docs_]",
            "requests[docs, bad.]",
            "requests[docs,]",
        ] {
            assert_eq!(
                parse_python_requirement(requirement),
                Err(PythonRequirementParseError::InvalidRequirement),
                "malformed name or extra: {requirement}",
            );
        }
        let admitted = parse_python_requirement("Requests_Socks[valid_extra,Docs.Test]>=2")?;
        assert_eq!(admitted.package_name, "requests-socks");
        Ok(())
    }

    #[test]
    fn parses_full_requirement_forms_and_preserves_only_conservative_extra_facts()
    -> Result<(), PythonRequirementParseError> {
        let constrained =
            parse_python_requirement("Requests_Socks[socks]>=2.22,<3; python_version >= '3.10'")?;
        assert_eq!(constrained.package_name, "requests-socks");
        assert!(!constrained.optional_by_extra);

        let direct_url = parse_python_requirement(
            "sample-wheel @ https://example.org/packages/sample-wheel-1.0.whl",
        )?;
        assert_eq!(direct_url.package_name, "sample-wheel");
        assert!(!direct_url.optional_by_extra);

        let positive = parse_python_requirement("requests; extra == 'docs'")?;
        assert!(positive.optional_by_extra);

        let guarded_conjunction =
            parse_python_requirement("requests; extra == 'docs' and python_version >= '3.10'")?;
        assert!(guarded_conjunction.optional_by_extra);

        let base_applicable_disjunction =
            parse_python_requirement("requests; extra == 'docs' or python_version >= '3.10'")?;
        assert!(!base_applicable_disjunction.optional_by_extra);

        for marker in ["extra != 'docs'", "extra == ''"] {
            let parsed = parse_python_requirement(&format!("requests; {marker}"))?;
            assert!(!parsed.optional_by_extra, "marker: {marker}");
        }

        Ok(())
    }

    #[test]
    fn rejects_incomplete_or_semantically_warned_requirements() {
        for input in ["requests garbage", "requests ???", "requests @ not a url"] {
            assert_eq!(
                parse_python_requirement(input),
                Err(PythonRequirementParseError::InvalidRequirement),
                "input: {input}"
            );
        }
    }

    #[test]
    fn marker_budget_counts_complete_nodes_and_edges() -> Result<(), &'static str> {
        let marker = MarkerTree::parse_str::<pep508_rs::VerbatimUrl>("extra == 'docs'")
            .map_err(|_| "marker fixture must parse")?;

        assert_eq!(
            marker_dag_within_budget(&marker, 2, MAX_MARKER_DAG_EDGES),
            Err(PythonRequirementParseError::MarkerComplexityLimitExceeded)
        );
        assert_eq!(
            marker_dag_within_budget(&marker, 3, 1),
            Err(PythonRequirementParseError::MarkerComplexityLimitExceeded)
        );
        assert_eq!(marker_dag_within_budget(&marker, 3, 2), Ok(()));

        // The empty-extra semantic walk follows only the false edge, but the
        // complexity budget must still account for both outcomes of the node.
        assert_eq!(
            marker_may_apply_without_extra_with_budget(&marker, 2),
            Err(PythonRequirementParseError::MarkerComplexityLimitExceeded)
        );
        assert_eq!(
            marker_may_apply_without_extra_with_budget(&marker, 3),
            Ok(false)
        );
        Ok(())
    }

    #[test]
    fn marker_budget_rejects_a_larger_normalized_graph_deterministically()
    -> Result<(), &'static str> {
        let marker_text = (0..20)
            .map(|index| format!("extra == 'docs-{index}'"))
            .collect::<Vec<_>>()
            .join(" or ");
        let marker = MarkerTree::parse_str::<pep508_rs::VerbatimUrl>(&marker_text)
            .map_err(|_| "expansive marker fixture must parse")?;

        assert_eq!(
            marker_dag_within_budget(&marker, 8, 128),
            Err(PythonRequirementParseError::MarkerComplexityLimitExceeded)
        );
        assert_eq!(
            marker_dag_within_budget(&marker, MAX_MARKER_DAG_NODES, MAX_MARKER_DAG_EDGES),
            Ok(())
        );
        Ok(())
    }

    #[test]
    fn bounds_marker_nesting_and_preparse_work_explicitly()
    -> Result<(), PythonRequirementParseError> {
        let moderate = format!(
            "requests; {}extra == 'docs'{}",
            "(".repeat(MAX_OPEN_PARENTHESES / 2),
            ")".repeat(MAX_OPEN_PARENTHESES / 2),
        );
        let moderate = parse_python_requirement(&moderate)?;
        assert!(moderate.optional_by_extra);

        let pathological = format!(
            "requests; {}extra == 'docs'{}",
            "(".repeat(MAX_OPEN_PARENTHESES + 1),
            ")".repeat(MAX_OPEN_PARENTHESES + 1),
        );
        assert_eq!(
            parse_python_requirement(&pathological),
            Err(PythonRequirementParseError::MarkerComplexityLimitExceeded)
        );

        // The pre-parser scan is only a safety circuit; it still bounds
        // adversarial syntax before pep508_rs constructs its canonical DAG.
        let too_many_operators = format!(
            "requests; {}",
            vec!["python_version >= '3.8'"; MAX_PREPARSE_MARKER_BOOLEAN_TOKENS + 2].join(" and "),
        );
        assert_eq!(
            parse_python_requirement(&too_many_operators),
            Err(PythonRequirementParseError::MarkerComplexityLimitExceeded)
        );

        let quote_escape_bypass = format!(
            "requests; extra == 'docs\\' and {}",
            vec!["python_version >= '3.8'"; MAX_PREPARSE_MARKER_BOOLEAN_TOKENS + 1].join(" and "),
        );
        assert_eq!(
            parse_python_requirement(&quote_escape_bypass),
            Err(PythonRequirementParseError::MarkerComplexityLimitExceeded)
        );

        let apostrophe_in_url = format!(
            "requests @ https://example.org/a'b ; {}",
            "(".repeat(MAX_OPEN_PARENTHESES + 1),
        );
        assert!(
            !has_bounded_marker_complexity(&apostrophe_in_url),
            "an apostrophe in a direct URL cannot hide marker complexity"
        );

        Ok(())
    }
}
