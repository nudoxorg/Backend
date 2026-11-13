//! Read-only native Stylo queries over the exact selected auxiliary root.
//!
//! Under the current admission bounds, a single hit cannot exhaust the
//! minimum 128 KiB output budget: it contains at most three native records of
//! 32 KiB each, one at-most-4096-byte host path (at most 8192 JSON bytes), a
//! bounded basis (<8192 bytes), a cursor (<=2048 bytes), and less than 1024
//! bytes of remaining framing/source-token overhead. This is under 118 KiB.
//! `OutputLimit` remains a defensive terminal if future bounds change; it is
//! not evidence that a valid current one-hit page can reach that branch.
use super::super::{BuiltinModelError, BuiltinWorkspaceRelation};
use backend_engine::{
    WorkspaceSnapshot,
    builtin::{
        MAX_STYLO_RELATION_ROW_BYTES, StyloAnalysisRecord, StyloAnalysisRelation,
        StyloAnalysisRootSchema, StyloAttemptOutcome, StyloProjectedRow, StyloRecordKind,
        stylo_analysis_relation, stylo_analysis_root_pointer_key, stylo_attempt_key,
        stylo_record_key, stylo_rows_digest, stylo_selection_key,
    },
};
use backend_library::stylo_query::*;
use backend_semantic::stylo_analysis::{
    StyloRule, StyloSelector, StyloSelectorComponentKind, StyloSourceRole,
};
use backend_version::{Relation, Schema, SchemaIdentity};

fn error(message: impl std::fmt::Display) -> BuiltinModelError {
    BuiltinModelError(format!("native CSS query: {message}"))
}
fn native_var_matches(query: &str, kind: &str, name: &str) -> bool {
    kind == "var" && query.strip_prefix("--") == Some(name)
}
fn native_declaration_matches(query: &str, serialized_name: &str) -> bool {
    !serialized_name.contains('\\') && serialized_name == query
}
fn gaps() -> Vec<StyloScopeGap> {
    vec![
        StyloScopeGap::AuthoredSourcePartial,
        StyloScopeGap::AuthoredReferenceSitesUnavailable,
        StyloScopeGap::OrdinaryVariableReferencesUnavailable,
        StyloScopeGap::NativeDeclarationNameDecodeUnavailable,
        StyloScopeGap::PreferenceStabilityUnknown,
        StyloScopeGap::ImportsNotResolved,
        StyloScopeGap::AncestorRuleContextNotReturned,
        StyloScopeGap::FrameworkNotInterpreted,
        StyloScopeGap::DomAndCascadeNotAttempted,
        StyloScopeGap::ProviderUnknown,
    ]
}
fn failure(reason: backend_engine::builtin::StyloUnavailable) -> StyloFailure {
    use backend_engine::builtin::StyloUnavailable as U;
    match reason {
        U::ReadUnavailable => StyloFailure::ReadUnavailable,
        U::SelectionUnavailable => StyloFailure::SelectionUnavailable,
        U::InvalidUtf8 => StyloFailure::InvalidUtf8,
        U::Limit => StyloFailure::Limit,
        U::ParserUnavailable => StyloFailure::ParserUnavailable,
        U::UnsupportedHardLimit => StyloFailure::UnsupportedHardLimit,
        U::Deadline => StyloFailure::Deadline,
        U::WorkerFailure => StyloFailure::WorkerFailure,
        U::PreferencesChanged => StyloFailure::PreferencesChanged,
    }
}
struct Work {
    rows: usize,
    bytes: usize,
    max_rows: usize,
    max_bytes: usize,
}
impl Work {
    fn charge(&mut self, row: &StyloAnalysisRecord) -> Result<(), BuiltinModelError> {
        let mut bytes = Vec::new();
        StyloAnalysisRelation::encode_value(row, &mut bytes);
        self.rows = self
            .rows
            .checked_add(1)
            .ok_or_else(|| error("row budget overflow"))?;
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or_else(|| error("byte budget overflow"))?;
        if self.rows > self.max_rows || self.bytes > self.max_bytes {
            return Err(error("analysis work limit exceeded"));
        }
        Ok(())
    }
    fn can_scan_candidate(&self) -> bool {
        self.max_rows.saturating_sub(self.rows) >= 5
            && self.max_bytes.saturating_sub(self.bytes) >= 5 * MAX_STYLO_RELATION_ROW_BYTES
    }
}
pub(super) type RelationHandle = backend_engine::workspace::WorkspaceRelationHandle<StyloAnalysisRelation>;
pub(super) fn selected_native_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<RelationHandle>, BuiltinModelError> {
    let relation = stylo_analysis_relation(snapshot).map_err(error)?;
    if let Some(relation) = &relation {
        let schema = SchemaIdentity::new(
            StyloAnalysisRootSchema::DOMAIN,
            StyloAnalysisRootSchema::TYPE,
            StyloAnalysisRootSchema::VERSION,
        );
        let key = stylo_analysis_root_pointer_key();
        let pointer = snapshot
            .closure()
            .control_manifest()
            .objects()
            .iter()
            .find(|object| object.schema() == schema && object.key() == &key)
            .ok_or_else(|| error("native relation pointer is not selected"))?;
        if !snapshot
            .closure()
            .contains(pointer.id())
            .map_err(|err| error(format!("{err:?}")))?
        {
            return Err(error("native relation pointer is outside selected closure"));
        }
        let root = relation.root_object().map_err(error)?;
        if !snapshot
            .closure()
            .contains(root.id())
            .map_err(|err| error(format!("{err:?}")))?
        {
            return Err(error("native relation root is outside selected closure"));
        }
    }
    Ok(relation)
}
fn lookup(
    relation: &RelationHandle,
    key: &[u8; 32],
    work: &mut Work,
) -> Result<Option<StyloAnalysisRecord>, BuiltinModelError> {
    let row = relation.lookup(key).map_err(error)?;
    if let Some(value) = &row {
        work.charge(value)?;
    } else {
        work.rows = work
            .rows
            .checked_add(1)
            .ok_or_else(|| error("row budget overflow"))?;
        if work.rows > work.max_rows {
            return Err(error("analysis work limit exceeded"));
        }
    }
    Ok(row)
}
struct Selected {
    project: [u8; 32],
    producer: [u8; 32],
    artifact: [u8; 32],
    adapter: [u8; 32],
}
fn native_key(
    selection: &Selected,
    host: &StyloAnalysisRecord,
    path: &[u32],
    kind: StyloRecordKind,
    ordinal: u32,
) -> Result<[u8; 32], BuiltinModelError> {
    let StyloAnalysisRecord::Host {
        project,
        path: file,
        content: Some(content),
        source: Some(source),
        analysis_basis: Some(analysis_basis),
        ..
    } = host
    else {
        return Err(error("native fact has no authenticated host basis"));
    };
    if *project != selection.project {
        return Err(error("native host has another project owner"));
    }
    stylo_record_key(
        *project,
        file,
        *content,
        *source,
        selection.producer,
        selection.artifact,
        selection.adapter,
        *analysis_basis,
        path,
        kind,
        ordinal,
        0,
    )
    .map_err(error)
}
fn rule(
    relation: &RelationHandle,
    selection: &Selected,
    host: &StyloAnalysisRecord,
    path: &[u32],
    host_key: [u8; 32],
    work: &mut Work,
) -> Result<StyloRule, BuiltinModelError> {
    let key = native_key(selection, host, path, StyloRecordKind::Rule, 0)?;
    match lookup(relation, &key, work)? {
        Some(StyloAnalysisRecord::Fact {
            host,
            fact: StyloProjectedRow::Rule(rule),
            ..
        }) if host == host_key && rule.path == path => Ok(rule),
        _ => Err(error("native rule ownership is unavailable")),
    }
}
fn selector(
    relation: &RelationHandle,
    selection: &Selected,
    host: &StyloAnalysisRecord,
    path: &[u32],
    ordinal: u32,
    host_key: [u8; 32],
    work: &mut Work,
) -> Result<StyloSelector, BuiltinModelError> {
    let key = native_key(selection, host, path, StyloRecordKind::Selector, ordinal)?;
    match lookup(relation, &key, work)? {
        Some(StyloAnalysisRecord::Fact {
            host,
            fact: StyloProjectedRow::Selector(selector),
            ..
        }) if host == host_key && selector.rule_path == path && selector.ordinal == ordinal => {
            Ok(selector)
        }
        _ => Err(error("native selector ownership is unavailable")),
    }
}
/// Classify the exact attempt linked to a selected relation row. Missing
/// historical evidence remains unknown; mismatched linkage is an error.
pub(super) fn classify_attempt(
    project: [u8; 32],
    selected_row: Option<&StyloAnalysisRecord>,
    attempt: Option<StyloAnalysisRecord>,
) -> Result<(StyloAttemptState, StyloWorkerCapability), BuiltinModelError> {
    let selection_digest = selected_row
        .map(|row| stylo_rows_digest(&[(stylo_selection_key(project), row)]))
        .transpose()
        .map_err(error)?;
    let result = match attempt {
        None => (
            if selected_row.is_some() {
                StyloAttemptState::UnknownLegacy
            } else {
                StyloAttemptState::NoSelectedAnalysis
            },
            StyloWorkerCapability::Unknown,
        ),
        Some(StyloAnalysisRecord::Attempt {
            project: owner,
            outcome,
            worker_artifact,
            served_selection,
            ..
        }) if owner == project && served_selection == selection_digest => {
            let capability = match worker_artifact {
                backend_engine::builtin::StyloAttemptArtifact::Admitted(id) => {
                    StyloWorkerCapability::Admitted(id)
                }
                backend_engine::builtin::StyloAttemptArtifact::NoPermit => {
                    StyloWorkerCapability::NoPermit
                }
                backend_engine::builtin::StyloAttemptArtifact::Unknown => {
                    StyloWorkerCapability::Unknown
                }
            };
            let state = match (outcome, selected_row.is_some()) {
                (StyloAttemptOutcome::Observed, true) => StyloAttemptState::LatestObserved,
                (StyloAttemptOutcome::Unavailable(reason), true) => {
                    StyloAttemptState::RetainedAfterUnavailable {
                        reason: failure(reason),
                    }
                }
                (StyloAttemptOutcome::Unavailable(reason), false) => {
                    StyloAttemptState::Unavailable {
                        reason: failure(reason),
                    }
                }
                _ => return Err(error("invalid latest native attempt")),
            };
            (state, capability)
        }
        _ => return Err(error("native attempt does not own selected facts")),
    };
    Ok(result)
}

/// Query an already owner-selected snapshot. `secret` is the durable product owner authority.
pub(super) fn query(
    snapshot: &WorkspaceSnapshot,
    secret: &[u8; 32],
    request: &StyloQueryRequest,
) -> Result<StyloQueryPage, BuiltinModelError> {
    let name = request.normalized_name().map_err(error)?;
    let project = backend_library::package_key(request.package.as_str()).to_bytes();
    let manifest_root = *snapshot.root().as_bytes();
    let source = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(error)?;
    let project_row = source.lookup(&project).map_err(error)?;
    if !project_row
        .as_ref()
        .and_then(|r| r.project_fields())
        .is_some_and(|p| p.label == request.package.as_str())
    {
        if request.continuation.is_some() {
            return Err(error("continuation expired: project unavailable"));
        }
        return Ok(StyloQueryPage {
            basis: StyloQueryBasis {
                manifest_root,
                native_root: None,
                attempt: StyloAttemptState::ProjectNotSelected,
                worker_capability: StyloWorkerCapability::Unknown,
                host_terminal_summary: StyloHostTerminalSummary::Unknown,
                source_partial: true,
                gaps: gaps(),
                producer: None,
                selection_complete: false,
            },
            hits: Vec::new(),
            terminal: StyloQueryTerminal::Unavailable,
        });
    }
    let Some(relation) = selected_native_relation(snapshot)? else {
        if request.continuation.is_some() {
            return Err(error("continuation expired: native root unavailable"));
        }
        return Ok(StyloQueryPage {
            basis: StyloQueryBasis {
                manifest_root,
                native_root: None,
                attempt: StyloAttemptState::NoSelectedAnalysis,
                worker_capability: StyloWorkerCapability::Unknown,
                host_terminal_summary: StyloHostTerminalSummary::Unknown,
                source_partial: true,
                gaps: gaps(),
                producer: None,
                selection_complete: false,
            },
            hits: Vec::new(),
            terminal: StyloQueryTerminal::Unavailable,
        });
    };
    let native_root = *relation.root().as_bytes();
    let mut work = Work {
        rows: 0,
        bytes: 0,
        max_rows: request.scan_rows as usize,
        max_bytes: request.scan_bytes as usize,
    };
    let selected_row = lookup(&relation, &stylo_selection_key(project), &mut work)?;
    let attempt = lookup(&relation, &stylo_attempt_key(project), &mut work)?;
    let (attempt_state, worker_capability) = classify_attempt(project, selected_row.as_ref(), attempt)?;
    let (selection, complete) = match selected_row {
        Some(StyloAnalysisRecord::Selection {
            project: owner,
            producer,
            worker_artifact,
            adapter,
            policy_complete,
            inventory,
            ..
        }) if owner == project => (
            Some(Selected {
                project,
                producer,
                artifact: worker_artifact,
                adapter,
            }),
            policy_complete
                && matches!(inventory, backend_engine::builtin::StyloInventory::Complete),
        ),
        None => (None, false),
        _ => return Err(error("invalid native selection")),
    };
    let basis = StyloQueryBasis {
        manifest_root,
        native_root: Some(native_root),
        attempt: attempt_state,
        worker_capability,
        host_terminal_summary: StyloHostTerminalSummary::Unknown,
        source_partial: true,
        gaps: gaps(),
        producer: selection.as_ref().map(|s| s.producer),
        selection_complete: complete,
    };
    let Some(selection) = selection else {
        if request.continuation.is_some() {
            return Err(error("continuation expired: native selection unavailable"));
        }
        return Ok(StyloQueryPage {
            basis,
            hits: Vec::new(),
            terminal: StyloQueryTerminal::Unavailable,
        });
    };
    let mut after = None;
    if let Some(cursor) = &request.continuation {
        let claims = cursor.verify(secret).map_err(error)?;
        if claims.manifest_root != manifest_root
            || claims.native_root != native_root
            || claims.project != project
            || claims.role != request.role
            || claims.name != name
            || claims.page_rows != request.page_rows
            || claims.output_bytes != request.output_bytes
            || claims.scan_rows != request.scan_rows
            || claims.scan_bytes != request.scan_bytes
        {
            return Err(error("continuation basis or query changed"));
        }
        after = Some(claims.after);
    }
    let mut hits = Vec::new();
    // Charge the actual basis, hit JSON and terminal; framing and the small
    // possible difference in a later cursor's numeric byte values fit in 256 bytes.
    let mut output_bytes = serde_json::to_vec(&basis).map_err(error)?.len() + 256;
    if output_bytes > request.output_bytes as usize {
        return Err(error("output budget cannot fit query basis"));
    }
    loop {
        if !work.can_scan_candidate() {
            let after = after.ok_or_else(|| error("analysis scan budget too small"))?;
            let continuation = StyloQueryCursor::issue(
                StyloCursorClaims {
                    manifest_root,
                    native_root,
                    project,
                    role: request.role,
                    name: name.to_owned(),
                    page_rows: request.page_rows,
                    output_bytes: request.output_bytes,
                    scan_rows: request.scan_rows,
                    scan_bytes: request.scan_bytes,
                    after,
                },
                secret,
            )
            .map_err(error)?;
            return Ok(StyloQueryPage {
                basis,
                hits,
                terminal: StyloQueryTerminal::More {
                    continuation,
                    work_limited: true,
                },
            });
        }
        let page = relation.page(after.as_ref(), 1).map_err(error)?;
        let Some((key, row)) = page.entries().first() else {
            return Ok(StyloQueryPage {
                basis,
                hits,
                terminal: StyloQueryTerminal::Complete,
            });
        };
        work.charge(row)?;
        // A full one-row page may still be the last page; prove exact end.
        let lookahead = relation.page(Some(key), 1).map_err(error)?;
        let is_last = if let Some((_, next)) = lookahead.entries().first() {
            work.charge(next)?;
            false
        } else {
            true
        };
        let candidate = match row {
            StyloAnalysisRecord::Fact {
                host,
                fact: StyloProjectedRow::SourceOccurrence(site),
                ..
            } if site.decoded == name
                && matches!(
                    (request.role, site.role),
                    (StyloNameRole::Class, StyloSourceRole::ClassName)
                        | (
                            StyloNameRole::CustomProperty,
                            StyloSourceRole::CustomPropertyDeclarationName
                        )
                ) =>
            {
                Some((*host, 0))
            }
            StyloAnalysisRecord::Fact {
                host,
                fact: StyloProjectedRow::Selector(selector),
                ..
            } if request.role == StyloNameRole::Class
                && selector
                    .components
                    .iter()
                    .any(|c| c.kind == StyloSelectorComponentKind::Class && c.name == name) =>
            {
                Some((*host, 1))
            }
            StyloAnalysisRecord::Fact {
                host,
                fact: StyloProjectedRow::RetainedDeclaration(declaration),
                ..
            } if request.role == StyloNameRole::CustomProperty
                && native_declaration_matches(name, &declaration.property) =>
            {
                Some((*host, 2))
            }
            StyloAnalysisRecord::Fact {
                host,
                fact: StyloProjectedRow::Dependency(dependency),
                ..
            } if request.role == StyloNameRole::CustomProperty
                && native_var_matches(name, &dependency.kind, &dependency.name) =>
            {
                Some((*host, 3))
            }
            _ => None,
        };
        let hit = if let Some((host_key, kind)) = candidate {
            match lookup(&relation, &host_key, &mut work)? {
                Some(host @ StyloAnalysisRecord::Host { project: owner, .. })
                    if owner == project =>
                {
                    let StyloAnalysisRecord::Host {
                        path, source_gaps, ..
                    } = &host
                    else {
                        unreachable!()
                    };
                    let StyloAnalysisRecord::Fact { fact, .. } = row else {
                        unreachable!()
                    };
                    Some(match (kind, fact) {
                        (0, StyloProjectedRow::SourceOccurrence(site)) => {
                            let rule = rule(
                                &relation,
                                &selection,
                                &host,
                                &site.rule_path,
                                host_key,
                                &mut work,
                            )?;
                            let selector = site
                                .selector_ordinal
                                .map(|n| {
                                    selector(
                                        &relation,
                                        &selection,
                                        &host,
                                        &site.rule_path,
                                        n,
                                        host_key,
                                        &mut work,
                                    )
                                })
                                .transpose()?;
                            let source = StyloSourceReference::issue(
                                StyloSourceClaims {
                                    manifest_root,
                                    native_root,
                                    project,
                                    host: host_key,
                                    site: *key,
                                },
                                secret,
                            )
                            .map_err(error)?;
                            StyloQueryHit::SourceSite {
                                id: *key,
                                path: path.clone(),
                                site: site.clone(),
                                rule,
                                selector,
                                source,
                                source_gaps: source_gaps.clone(),
                            }
                        }
                        (1, StyloProjectedRow::Selector(value)) => StyloQueryHit::NativeSelector {
                            id: *key,
                            path: path.clone(),
                            rule: rule(
                                &relation,
                                &selection,
                                &host,
                                &value.rule_path,
                                host_key,
                                &mut work,
                            )?,
                            selector: value.clone(),
                        },
                        (2, StyloProjectedRow::RetainedDeclaration(value)) => {
                            StyloQueryHit::NativeDeclaration {
                                id: *key,
                                path: path.clone(),
                                rule: rule(
                                    &relation,
                                    &selection,
                                    &host,
                                    &value.rule_path,
                                    host_key,
                                    &mut work,
                                )?,
                                declaration: value.clone(),
                            }
                        }
                        (3, StyloProjectedRow::Dependency(value)) => {
                            let declaration_key = native_key(
                                &selection,
                                &host,
                                &value.rule_path,
                                StyloRecordKind::RetainedDeclaration,
                                value.declaration_ordinal,
                            )?;
                            let declaration = match lookup(&relation, &declaration_key, &mut work)?
                            {
                                Some(StyloAnalysisRecord::Fact {
                                    host,
                                    fact: StyloProjectedRow::RetainedDeclaration(declaration),
                                    ..
                                }) if host == host_key
                                    && declaration.rule_path == value.rule_path
                                    && declaration.ordinal == value.declaration_ordinal =>
                                {
                                    declaration
                                }
                                _ => {
                                    return Err(error(
                                        "native dependency owner declaration unavailable",
                                    ));
                                }
                            };
                            StyloQueryHit::NativeDependency {
                                id: *key,
                                path: path.clone(),
                                rule: rule(
                                    &relation,
                                    &selection,
                                    &host,
                                    &value.rule_path,
                                    host_key,
                                    &mut work,
                                )?,
                                declaration,
                                dependency: value.clone(),
                            }
                        }
                        _ => return Err(error("native fact changed during read")),
                    })
                }
                Some(StyloAnalysisRecord::Host { .. }) => None,
                _ => return Err(error("native fact host missing")),
            }
        } else {
            None
        };
        if let Some(hit) = hit {
            let size = serde_json::to_vec(&hit)
                .map_err(error)?
                .len()
                .saturating_add(1);
            let prospective_terminal = if is_last {
                StyloQueryTerminal::Complete
            } else {
                let continuation = StyloQueryCursor::issue(
                    StyloCursorClaims {
                        manifest_root,
                        native_root,
                        project,
                        role: request.role,
                        name: name.to_owned(),
                        page_rows: request.page_rows,
                        output_bytes: request.output_bytes,
                        scan_rows: request.scan_rows,
                        scan_bytes: request.scan_bytes,
                        after: *key,
                    },
                    secret,
                )
                .map_err(error)?;
                StyloQueryTerminal::More {
                    continuation,
                    work_limited: false,
                }
            };
            let terminal_bytes = serde_json::to_vec(&prospective_terminal)
                .map_err(error)?
                .len();
            let required = output_bytes
                .saturating_add(size)
                .saturating_add(terminal_bytes);
            if required > request.output_bytes as usize {
                if hits.is_empty() {
                    let required_bytes =
                        u32::try_from(required.saturating_add(512)).unwrap_or(u32::MAX);
                    let continuation = if required_bytes <= 4_194_304 {
                        after
                            .map(|after| {
                                StyloQueryCursor::issue(
                                    StyloCursorClaims {
                                        manifest_root,
                                        native_root,
                                        project,
                                        role: request.role,
                                        name: name.to_owned(),
                                        page_rows: request.page_rows,
                                        output_bytes: required_bytes,
                                        scan_rows: request.scan_rows,
                                        scan_bytes: request.scan_bytes,
                                        after,
                                    },
                                    secret,
                                )
                            })
                            .transpose()
                            .map_err(error)?
                    } else {
                        None
                    };
                    return Ok(StyloQueryPage {
                        basis,
                        hits,
                        terminal: StyloQueryTerminal::OutputLimit {
                            required_bytes,
                            continuation,
                        },
                    });
                }
                let after = after.ok_or_else(|| error("output budget cannot make progress"))?;
                let continuation = StyloQueryCursor::issue(
                    StyloCursorClaims {
                        manifest_root,
                        native_root,
                        project,
                        role: request.role,
                        name: name.to_owned(),
                        page_rows: request.page_rows,
                        output_bytes: request.output_bytes,
                        scan_rows: request.scan_rows,
                        scan_bytes: request.scan_bytes,
                        after,
                    },
                    secret,
                )
                .map_err(error)?;
                return Ok(StyloQueryPage {
                    basis,
                    hits,
                    terminal: StyloQueryTerminal::More {
                        continuation,
                        work_limited: false,
                    },
                });
            }
            output_bytes += size;
            hits.push(hit);
        }
        after = Some(*key);
        if is_last {
            return Ok(StyloQueryPage {
                basis,
                hits,
                terminal: StyloQueryTerminal::Complete,
            });
        }
        if hits.len() == request.page_rows as usize {
            let continuation = StyloQueryCursor::issue(
                StyloCursorClaims {
                    manifest_root,
                    native_root,
                    project,
                    role: request.role,
                    name: name.to_owned(),
                    page_rows: request.page_rows,
                    output_bytes: request.output_bytes,
                    scan_rows: request.scan_rows,
                    scan_bytes: request.scan_bytes,
                    after: *key,
                },
                secret,
            )
            .map_err(error)?;
            return Ok(StyloQueryPage {
                basis,
                hits,
                terminal: StyloQueryTerminal::More {
                    continuation,
                    work_limited: false,
                },
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Selected, native_declaration_matches, native_key, native_var_matches};
    use backend_engine::builtin::{
        StyloAnalysisRecord, StyloRecordKind, StyloTerminal, StyloUnavailable, stylo_record_key,
    };

    #[test]
    fn selected_attempt_distinguishes_observed_retained_unknown_and_bad_link() {
        use backend_engine::builtin::{StyloAnalysisProfile, StyloAttemptArtifact, StyloAttemptOutcome, StyloInventory, StyloWorkerPolicy, stylo_rows_digest, stylo_selection_key};
        use backend_library::stylo_query::{StyloAttemptState, StyloFailure, StyloWorkerCapability};
        let project = [1; 32];
        let selection = StyloAnalysisRecord::Selection { project, source_root: "/project".into(), profile: StyloAnalysisProfile::StyloStandaloneV1, recipe: "fixture".into(), producer: [2; 32], worker_artifact: [3; 32], adapter: [4; 32], discovery: [5; 32], policy_complete: false, max_worker_attempts: None, inventory: StyloInventory::Incomplete, inventory_digest: [6; 32], inventory_count: 0, host_count: 0, host_digest: [7; 32] };
        let digest = stylo_rows_digest(&[(stylo_selection_key(project), &selection)]).unwrap();
        let mut attempt = StyloAnalysisRecord::Attempt { project, source_root: "/project".into(), profile: StyloAnalysisProfile::StyloStandaloneV1, scope_policy: [8; 32], discovery: Some([5; 32]), policy_complete: false, max_worker_attempts: None, inventory: Some(StyloInventory::Incomplete), inventory_digest: Some([6; 32]), inventory_count: Some(0), expected_producer: Some([2; 32]), expected_recipe: Some("fixture".into()), adapter: [4; 32], worker_artifact: StyloAttemptArtifact::Admitted([3; 32]), requested_policy: StyloWorkerPolicy { wall_time_nanos: 1, input_bytes: 1024, stdout_bytes: 1024, stderr_bytes: 64, output_bytes: 2048, memory_bytes: None }, outcome: StyloAttemptOutcome::Observed, served_selection: Some(digest) };
        assert!(attempt.validate().is_ok());
        assert_eq!(super::classify_attempt(project, Some(&selection), Some(attempt.clone())).unwrap(), (StyloAttemptState::LatestObserved, StyloWorkerCapability::Admitted([3; 32])));
        if let StyloAnalysisRecord::Attempt { outcome, worker_artifact, .. } = &mut attempt { *outcome = StyloAttemptOutcome::Unavailable(StyloUnavailable::ParserUnavailable); *worker_artifact = StyloAttemptArtifact::NoPermit; }
        assert!(attempt.validate().is_ok());
        assert_eq!(super::classify_attempt(project, Some(&selection), Some(attempt.clone())).unwrap(), (StyloAttemptState::RetainedAfterUnavailable { reason: StyloFailure::ParserUnavailable }, StyloWorkerCapability::NoPermit));
        assert_eq!(super::classify_attempt(project, Some(&selection), None).unwrap(), (StyloAttemptState::UnknownLegacy, StyloWorkerCapability::Unknown));
        if let StyloAnalysisRecord::Attempt { served_selection, .. } = &mut attempt { *served_selection = Some([9; 32]); }
        assert!(super::classify_attempt(project, Some(&selection), Some(attempt)).is_err());
    }

    #[test]
    fn owner_key_uses_full_analysis_basis_not_parser_configuration() {
        let selection = Selected {
            project: [1; 32],
            producer: [2; 32],
            artifact: [3; 32],
            adapter: [4; 32],
        };
        let host = |basis| StyloAnalysisRecord::Host {
            project: [1; 32],
            path: "site.css".into(),
            content: Some([5; 32]),
            source: Some([6; 32]),
            byte_length: None,
            utf8_bom_removed: None,
            config: None,
            configuration: Some([7; 32]),
            preferences: None,
            preference_stability: None,
            analysis_basis: Some(basis),
            worker_policy: None,
            source_coverage: None,
            source_gaps: Vec::new(),
            source_observer_recipe: None,
            terminal: StyloTerminal::Unavailable(StyloUnavailable::Limit),
            fact_count: 0,
            fact_digest: [0; 32],
        };
        let first = native_key(&selection, &host([8; 32]), &[0], StyloRecordKind::Rule, 0).unwrap();
        let second =
            native_key(&selection, &host([9; 32]), &[0], StyloRecordKind::Rule, 0).unwrap();
        assert_ne!(
            first, second,
            "changed preference/policy basis changes owner key even with the same parser configuration"
        );
        assert_eq!(
            first,
            stylo_record_key(
                [1; 32],
                "site.css",
                [5; 32],
                [6; 32],
                [2; 32],
                [3; 32],
                [4; 32],
                [8; 32],
                &[0],
                StyloRecordKind::Rule,
                0,
                0
            )
            .unwrap()
        );
    }

    #[test]
    fn escaped_serialized_declaration_names_do_not_forge_decoded_identity() {
        assert!(native_declaration_matches("--tone", "--tone"));
        // This serialization denotes decoded `--a:b`, not a literal backslash.
        assert!(!native_declaration_matches("--a:b", "--a\\:b"));
        assert!(!native_declaration_matches("--a\\:b", "--a\\:b"));
    }

    #[test]
    fn variable_names_are_role_aware_and_exact() {
        assert!(native_var_matches("--Tone", "var", "Tone"));
        assert!(!native_var_matches("--tone", "var", "Tone"));
        assert!(!native_var_matches("--Tone", "env", "Tone"));
        assert!(!native_var_matches("--Tone", "attr", "Tone"));
        assert!(!native_var_matches("Tone", "var", "Tone"));
        assert!(!native_var_matches("--OtherTone", "var", "Tone"));
    }
}

/// Reopen the exact owner-sealed authored site under its original selected roots.
/// Raw source is not retained: a changed or missing file is never substituted.
pub(super) fn read_site(
    snapshot: &WorkspaceSnapshot,
    secret: &[u8; 32],
    reference: &StyloSourceReference,
) -> Result<StyloSourceRead, BuiltinModelError> {
    use super::super::ingest::{SelectedSourceReadError, read_stylo_selected_source};
    use backend_compile::{SourceVerificationError, VerifiedSourceText};
    use backend_version::{ContentId, SourceFactDomain};
    let claims = reference.verify(secret).map_err(error)?;
    if *snapshot.root().as_bytes() != claims.manifest_root {
        return Ok(StyloSourceRead::Expired);
    }
    let Some(relation) = selected_native_relation(snapshot)? else {
        return Ok(StyloSourceRead::Expired);
    };
    if *relation.root().as_bytes() != claims.native_root {
        return Ok(StyloSourceRead::Expired);
    }
    let source = snapshot
        .relation::<BuiltinWorkspaceRelation>()
        .map_err(error)?;
    if !source
        .lookup(&claims.project)
        .map_err(error)?
        .as_ref()
        .and_then(|r| r.project_fields())
        .is_some()
    {
        return Ok(StyloSourceRead::Expired);
    }
    let selected = relation
        .lookup(&stylo_selection_key(claims.project))
        .map_err(error)?;
    let Some(StyloAnalysisRecord::Selection {
        project,
        source_root,
        ..
    }) = selected
    else {
        return Ok(StyloSourceRead::Expired);
    };
    if project != claims.project {
        return Err(error("selected source root has another owner"));
    }
    let host = relation.lookup(&claims.host).map_err(error)?;
    let Some(StyloAnalysisRecord::Host {
        project,
        path,
        content: Some(content),
        source: Some(source_identity),
        byte_length: Some(length),
        terminal: backend_engine::builtin::StyloTerminal::Parsed(_),
        ..
    }) = host
    else {
        return Ok(StyloSourceRead::Expired);
    };
    if project != claims.project {
        return Err(error("source host has another project owner"));
    }
    let site = relation.lookup(&claims.site).map_err(error)?;
    let Some(StyloAnalysisRecord::Fact {
        host,
        fact: StyloProjectedRow::SourceOccurrence(site),
        ..
    }) = site
    else {
        return Ok(StyloSourceRead::Expired);
    };
    if host != claims.host {
        return Err(error("authored site belongs to another host"));
    }
    let length = usize::try_from(length).map_err(error)?;
    if length > 4 * 1024 * 1024 {
        return Err(error("captured source exceeds profile budget"));
    }
    let bytes = match read_stylo_selected_source(std::path::Path::new(&source_root), &path, length)
    {
        Ok(bytes) => bytes,
        Err(SelectedSourceReadError::TooLarge) => return Ok(StyloSourceRead::SourceChanged),
        Err(SelectedSourceReadError::Vanished | SelectedSourceReadError::Unavailable) => {
            return Ok(StyloSourceRead::BytesNotRetained);
        }
        Err(SelectedSourceReadError::Invalid) => return Err(error("invalid confined source path")),
    };
    if bytes.len() != length {
        return Ok(StyloSourceRead::SourceChanged);
    }
    let actual_source = ContentId::<SourceFactDomain>::from_canonical_bytes(&bytes);
    if actual_source.as_ref() != &source_identity {
        return Ok(StyloSourceRead::SourceChanged);
    }
    let verified = match VerifiedSourceText::admit_stored(&bytes, content, actual_source) {
        Ok(verified) => verified,
        Err(
            SourceVerificationError::NonUtf8
            | SourceVerificationError::InputRevisionMismatch
            | SourceVerificationError::SourceIdentityMismatch,
        ) => return Ok(StyloSourceRead::SourceChanged),
        Err(SourceVerificationError::InvalidExtent) => return Err(error("invalid source extent")),
    };
    let start = usize::try_from(site.start).map_err(error)?;
    let end = usize::try_from(site.end).map_err(error)?;
    let extent = verified.extent(start..end).map_err(error)?;
    if extent.text() != site.raw {
        return Err(error("authored site bytes do not match admitted source"));
    }
    let line_1 = u32::try_from(bytes[..start].iter().filter(|byte| **byte == b'\n').count() + 1)
        .map_err(error)?;
    Ok(StyloSourceRead::Available {
        served_root: source_root,
        path,
        start: site.start,
        end: site.end,
        line_1,
        text: extent.text().to_owned(),
    })
}

/// Candidate-only impact from one authenticated authored custom-property site.
/// This intentionally uses the same root-selected scan and continuation as the
/// native query; equal names support a possible provider, never a binding.
pub(super) fn impact(
    snapshot: &WorkspaceSnapshot,
    secret: &[u8; 32],
    request: &backend_library::stylo_impact::StyloImpactRequest,
) -> Result<backend_library::stylo_impact::StyloImpactPage, BuiltinModelError> {
    use backend_library::stylo_impact::{
        StyloImpactCursor, StyloImpactPage, StyloImpactRow, StyloImpactStrength,
        StyloImpactTerminal,
    };
    request.admit().map_err(error)?;
    let seed = request.seed.verify(secret).map_err(error)?;
    let project = backend_library::package_key(request.query.package.as_str()).to_bytes();
    if seed.project != project || seed.manifest_root != *snapshot.root().as_bytes() {
        return Err(error(
            "impact seed expired or belongs to another project; reselect the source site",
        ));
    }
    let relation = selected_native_relation(snapshot)?
        .ok_or_else(|| error("impact seed expired: selected CSS root unavailable"))?;
    if seed.native_root != *relation.root().as_bytes() {
        return Err(error(
            "impact seed expired: CSS root changed; reselect the source site",
        ));
    }
    let mut preflight = Work {
        rows: 0,
        bytes: 0,
        max_rows: request.query.scan_rows as usize,
        max_bytes: request.query.scan_bytes as usize,
    };
    let selection = lookup(&relation, &stylo_selection_key(project), &mut preflight)?;
    if !matches!(selection, Some(StyloAnalysisRecord::Selection { project: owner, .. }) if owner == project)
    {
        return Err(error("impact seed has no selected project owner"));
    }
    let host = lookup(&relation, &seed.host, &mut preflight)?;
    if !matches!(host, Some(StyloAnalysisRecord::Host { project: owner, terminal: backend_engine::builtin::StyloTerminal::Parsed(_), .. }) if owner == project)
    {
        return Err(error("impact seed host is not selected and parsed"));
    }
    let site = lookup(&relation, &seed.site, &mut preflight)?;
    if !matches!(site, Some(StyloAnalysisRecord::Fact { host, fact: StyloProjectedRow::SourceOccurrence(ref site), .. }) if host == seed.host && site.role == StyloSourceRole::CustomPropertyDeclarationName && site.decoded == request.query.name)
    {
        return Err(error(
            "impact seed is not the requested authored custom-property declaration",
        ));
    }
    let mut scan = request.query.clone();
    // Reserve framing space for the seed and authenticated impact cursor.
    scan.output_bytes -= backend_library::stylo_impact::STYLO_IMPACT_ENVELOPE_RESERVE;
    scan.scan_rows -= u32::try_from(preflight.rows).map_err(error)?;
    scan.scan_bytes -= u32::try_from(preflight.bytes).map_err(error)?;
    if let Some(cursor) = &request.continuation {
        cursor.verify(&request.seed, secret).map_err(error)?;
        scan.continuation = Some(cursor.scan().clone());
    }
    let page = query(snapshot, secret, &scan)?;
    if page.basis.native_root != Some(seed.native_root)
        || page.basis.manifest_root != seed.manifest_root
    {
        return Err(error("impact selected basis changed"));
    }
    let rows = page
        .hits
        .into_iter()
        .filter_map(|witness| match witness {
            StyloQueryHit::NativeDependency { .. } => Some(StyloImpactRow {
                strength: StyloImpactStrength::Candidate,
                witness,
            }),
            _ => None,
        })
        .collect();
    let wrap =
        |cursor| StyloImpactCursor::issue(request.seed.clone(), cursor, secret).map_err(error);
    let terminal = match page.terminal {
        StyloQueryTerminal::Complete => StyloImpactTerminal::Complete,
        StyloQueryTerminal::Unavailable => StyloImpactTerminal::Unavailable,
        StyloQueryTerminal::More {
            continuation,
            work_limited,
        } => StyloImpactTerminal::More {
            continuation: wrap(continuation)?,
            work_limited,
        },
        StyloQueryTerminal::OutputLimit {
            required_bytes,
            continuation,
        } => {
            let total = required_bytes
                .saturating_add(backend_library::stylo_impact::STYLO_IMPACT_ENVELOPE_RESERVE);
            StyloImpactTerminal::OutputLimit {
                required_bytes: total,
                continuation: if total <= 4_194_304 {
                    continuation.map(wrap).transpose()?
                } else {
                    None
                },
            }
        }
    };
    Ok(StyloImpactPage {
        basis: page.basis,
        seed: request.seed.clone(),
        rows,
        terminal,
    })
}
