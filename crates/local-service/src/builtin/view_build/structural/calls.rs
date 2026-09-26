//! Syntactic call edges inferred from declaration excerpts.
//!
//! These relations are used when no complete semantic publication supplies
//! compiler-proven `Calls` links. Comment, string, and declarator scanning
//! stay here; file plans and type indexes stay with the structural projection.

use super::super::super::{BuiltinModelError, IndexedSources};
use super::FileContainment;
use backend_engine::{DeclarationKind, RowId};
use std::collections::{BTreeMap, BTreeSet};

fn structural_callable(kind: DeclarationKind) -> bool {
    matches!(
        kind,
        DeclarationKind::Function | DeclarationKind::Method | DeclarationKind::Constructor
    )
}

fn structural_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn structural_ident_boundary_before(excerpt: &str, at: usize) -> bool {
    at == 0 || !structural_ident_byte(excerpt.as_bytes()[at - 1])
}

fn structural_call_open_paren(excerpt: &str, after_name: usize) -> bool {
    excerpt[after_name..]
        .chars()
        .next()
        .is_some_and(|ch| ch == '(')
}

fn structural_fn_declarator_before(excerpt: &str, name_at: usize) -> bool {
    let prefix = excerpt[..name_at].trim_end();
    prefix.ends_with("fn") && prefix.len() >= 2 && {
        let fn_at = prefix.len() - 2;
        fn_at == 0 || !structural_ident_byte(prefix.as_bytes()[fn_at - 1])
    }
}

/// Returns the first `{` that opens the declaration body, skipping comments
/// and string/char literals so a signature like `fn parse_config(` is never
/// scanned as a call site.
fn structural_body_start(excerpt: &str) -> Option<usize> {
    let bytes = excerpt.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = index.saturating_add(2).min(bytes.len());
            }
            b'"' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = index.saturating_add(2).min(bytes.len());
                        continue;
                    }
                    if bytes[index] == b'"' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            b'\'' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = index.saturating_add(2).min(bytes.len());
                        continue;
                    }
                    if bytes[index] == b'\'' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            b'{' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

/// Returns whether `excerpt` contains a syntactic call to `callee`.
///
/// Only the declaration body is scanned. Occurrences inside line or block
/// comments, string literals, and char literals are ignored, and a function
/// declarator such as `fn parse_config(` is never treated as a call.
pub(in crate::builtin::view_build) fn structural_excerpt_calls(
    excerpt: &str,
    callee: &str,
) -> bool {
    if callee.is_empty() {
        return false;
    }
    let scan_from = structural_body_start(excerpt).unwrap_or(0);
    let body = &excerpt[scan_from..];
    let callee_len = callee.len();
    let mut index = 0;
    while index < body.len() {
        match body.as_bytes()[index] {
            b'/' if body.as_bytes().get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < body.len() && body.as_bytes()[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if body.as_bytes().get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < body.len()
                    && !(body.as_bytes()[index] == b'*' && body.as_bytes()[index + 1] == b'/')
                {
                    index += 1;
                }
                index = index.saturating_add(2).min(body.len());
            }
            b'"' => {
                index += 1;
                while index < body.len() {
                    if body.as_bytes()[index] == b'\\' {
                        index = index.saturating_add(2).min(body.len());
                        continue;
                    }
                    if body.as_bytes()[index] == b'"' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            b'\'' => {
                index += 1;
                while index < body.len() {
                    if body.as_bytes()[index] == b'\\' {
                        index = index.saturating_add(2).min(body.len());
                        continue;
                    }
                    if body.as_bytes()[index] == b'\'' {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
            }
            _ if body[index..].starts_with(callee)
                && structural_ident_boundary_before(body, index)
                && structural_call_open_paren(body, index + callee_len)
                && !structural_fn_declarator_before(body, index) =>
            {
                return true;
            }
            _ => index += 1,
        }
    }
    false
}

/// Same-file call edges inferred from bounded declaration excerpts when no
/// complete semantic publication supplies compiler-proven `Calls` links.
pub(crate) fn structural_call_graph_relations(
    view: &backend_engine::ViewRoot,
    sources: &IndexedSources,
    package: backend_engine::PackageKey,
    source_id: RowId,
    include_incoming: bool,
) -> Result<Option<Vec<backend_engine::GraphRelation>>, BuiltinModelError> {
    let source_row = view.row(source_id).ok_or_else(|| {
        BuiltinModelError("structural call graph source is absent from the view".to_owned())
    })?;
    let source_label = source_row.label.as_str();
    let project = sources
        .projects
        .values()
        .find(|project| project.package == package)
        .ok_or_else(|| {
            BuiltinModelError(
                "structural call graph package is absent from indexed sources".to_owned(),
            )
        })?;
    let project_key = project.package.to_bytes();
    let mut coordinate_ids = BTreeMap::<String, RowId>::new();
    for row in view.rows() {
        if row.package == Some(package) {
            coordinate_ids.insert(row.label.clone(), row.id);
        }
    }
    for (_, record) in &sources.files {
        let file = record
            .file_fields()
            .ok_or_else(|| BuiltinModelError("expected structural source file".to_owned()))?;
        if file.project != project_key {
            continue;
        }
        let containment =
            FileContainment::new(&project.label, file.path, file.project, file.declarations);
        let file_contains_source = file
            .declarations
            .iter()
            .any(|declaration| containment.coordinate(declaration) == source_label);
        if !file_contains_source {
            continue;
        }
        let mut relations = BTreeSet::new();
        for caller in file.declarations.iter() {
            if !structural_callable(caller.kind()) {
                continue;
            }
            let excerpt = caller.source_excerpt().text().ok_or_else(|| {
                BuiltinModelError(
                    "structural call graph caller omitted a source excerpt".to_owned(),
                )
            })?;
            let caller_coordinate = containment.coordinate(caller);
            let caller_id = coordinate_ids.get(&caller_coordinate).ok_or_else(|| {
                BuiltinModelError(
                    "structural call graph caller is absent from the published view".to_owned(),
                )
            })?;
            for callee in file.declarations.iter() {
                if caller.name() == callee.name() || !structural_callable(callee.kind()) {
                    continue;
                }
                if !structural_excerpt_calls(excerpt, callee.name()) {
                    continue;
                }
                let callee_coordinate = containment.coordinate(callee);
                let callee_id = coordinate_ids.get(&callee_coordinate).ok_or_else(|| {
                    BuiltinModelError(
                        "structural call graph callee is absent from the published view".to_owned(),
                    )
                })?;
                relations.insert(backend_engine::GraphRelation::new(
                    *caller_id,
                    *callee_id,
                    backend_library::SemanticLinkKind::Calls,
                ));
            }
        }
        let relations = relations
            .into_iter()
            .filter(|relation| {
                if include_incoming {
                    relation.from == source_id || relation.to == source_id
                } else {
                    relation.from == source_id
                }
            })
            .collect::<Vec<_>>();
        if relations.is_empty() {
            return Ok(None);
        }
        if relations.len() > usize::from(backend_engine::QueryLimit::MAX) {
            return Err(BuiltinModelError(
                "structural call graph exceeds the bounded result contract".to_owned(),
            ));
        }
        return Ok(Some(relations));
    }
    Ok(None)
}

/// Incoming call sites for one declaration when semantic references are absent.
pub(crate) fn structural_reference_facts(
    view: &backend_engine::ViewRoot,
    sources: &IndexedSources,
    target: &str,
) -> Result<Vec<backend_engine::ReferenceFact>, BuiltinModelError> {
    let target_row = view
        .rows()
        .iter()
        .find(|row| row.label == target)
        .ok_or_else(|| {
            BuiltinModelError("structural references target is absent from the view".to_owned())
        })?;
    let RowId::Symbol(target_symbol) = target_row.id else {
        return Err(BuiltinModelError(
            "structural references target is not a declaration row".to_owned(),
        ));
    };
    let Some(package) = target_row.package else {
        return Err(BuiltinModelError(
            "structural references target is not attributed to a package".to_owned(),
        ));
    };
    let Some(relations) =
        structural_call_graph_relations(view, sources, package, target_row.id, true)?
    else {
        return Ok(Vec::new());
    };
    let target_name = target
        .rsplit("::")
        .next()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            BuiltinModelError("structural references target has no declaration name".to_owned())
        })?;
    let target_identity = structural_symbol_identity(target_symbol);
    let mut facts = Vec::new();
    for relation in relations {
        if relation.to != target_row.id
            || relation.relation != backend_library::SemanticLinkKind::Calls
        {
            continue;
        }
        let site_row = view.row(relation.from).ok_or_else(|| {
            BuiltinModelError("structural references site is absent from the view".to_owned())
        })?;
        let RowId::Symbol(site_symbol) = site_row.id else {
            return Err(BuiltinModelError(
                "structural references site is not a declaration row".to_owned(),
            ));
        };
        let (start, end) = site_row
            .excerpt
            .text()
            .and_then(|excerpt| structural_call_span(excerpt, target_name))
            .map(|(start, end)| {
                (
                    u32::try_from(start).unwrap_or(u32::MAX),
                    u32::try_from(end).unwrap_or(u32::MAX),
                )
            })
            .unwrap_or((0, target_name.len().min(u32::MAX as usize) as u32));
        let source = match site_row.source.captured() {
            Some(location) => Some(backend_engine::SemanticSourceSpan {
                file: backend_engine::ProductText::new(location.path()).map_err(|error| {
                    BuiltinModelError(format!("structural references path: {error:?}"))
                })?,
                start,
                end,
            }),
            None => None,
        };
        facts.push(backend_engine::ReferenceFact {
            site: site_symbol,
            target: backend_engine::SemanticLinkTarget::Local {
                declaration: target_identity,
            },
            relation: backend_library::SemanticLinkKind::Calls,
            evidence: backend_engine::SemanticLinkEvidence {
                confidence: backend_library::SemanticConfidence::Syntactic,
                source,
            },
        });
        if facts.len() > backend_engine::MAX_PRODUCT_ROWS {
            return Err(BuiltinModelError(
                "structural references exceed the bounded result contract".to_owned(),
            ));
        }
    }
    Ok(facts)
}

fn structural_symbol_identity(
    symbol: backend_engine::SymbolKey,
) -> backend_engine::SemanticDeclarationIdentity {
    let bytes = symbol.as_bytes();
    let mut family = [0_u8; 16];
    let mut variant = [0_u8; 16];
    family[..bytes.len().min(16)].copy_from_slice(&bytes[..bytes.len().min(16)]);
    if bytes.len() > 16 {
        variant[..16].copy_from_slice(&bytes[bytes.len() - 16..]);
    }
    backend_engine::SemanticDeclarationIdentity { family, variant }
}

fn structural_call_span(excerpt: &str, callee: &str) -> Option<(usize, usize)> {
    let scan_from = structural_body_start(excerpt).unwrap_or(0);
    let body = &excerpt[scan_from..];
    let needle = format!("{callee}(");
    let relative = body.find(&needle)?;
    let start = scan_from + relative;
    Some((start, start + needle.len() - 1))
}
