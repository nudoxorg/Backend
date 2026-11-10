//! Experimental SQLite adapter; source capture and coverage are dialect neutral.
use backend_sql_contract::{
    Binding, DiagnosticKind, Dialect, EffectiveSettings, Engine, Extent, IdentifierCoverage,
    MAX_SOURCE_BYTES, NativeDiagnostic, PROTOCOL_VERSION, ProducerBasis, Request, Response,
    SemanticContext, SqliteColumnLineage, SqliteColumnOrigin, SqliteLineageCoverage,
    SqliteRelationAccess, SqliteRelationKind, Syntax,
};
use sha2::{Digest, Sha256};
mod outline;
/// Observe the pinned syntaqlite default parser version/flags and analyzer DQS policy.
pub fn observed_basis() -> ProducerBasis {
    use syntaqlite::util::SqliteFlag;
    let dialect = syntaqlite::sqlite_dialect();
    let flags = dialect.cflags();
    let bits = SqliteFlag::all()
        .iter()
        .filter(|f| flags.has(**f))
        .fold(0u64, |bits, f| bits | (1u64 << (*f as u32)));
    ProducerBasis {
        support_sha256: Some(sqlite_support_fingerprint()),
        engine: Engine::Syntaqlite4e4de76,
        native_revision: "4e4de76312429fb2235ae093ab2125914320d5bd".into(),
        recipe: "syntaqlite-sqlite-fmt-analysis-dqs-default-v1".into(),
        lock_sha256: Sha256::digest(include_bytes!("../Cargo.lock")).into(),
        settings: EffectiveSettings::Sqlite {
            version: dialect.version().as_int(),
            cflags: bits,
            analyzer_dqs_fallback: true,
        },
    }
}
/// Exact adapter, worker, schema, lock and vendored engine source fingerprint.
pub fn sqlite_support_fingerprint() -> [u8; 32] {
    let value = env!("SQLITE_SUPPORT_SHA256").as_bytes();
    let mut digest = [0u8; 32];
    for (index, pair) in value.chunks_exact(2).enumerate() {
        let digit = |byte: u8| match byte {
            b'0'..=b'9' => byte - b'0',
            b'a'..=b'f' => byte - b'a' + 10,
            _ => unreachable!("build fingerprint is lowercase hexadecimal"),
        };
        digest[index] = (digit(pair[0]) << 4) | digit(pair[1]);
    }
    digest
}
/// A bounded caller-proposed SQLite fixture, never an admitted workspace catalog.
/// Names are ordered, nonempty and ASCII-case-normalized; SQLite default DQS is used.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedCatalog {
    pub epoch: u64,
    pub tables: Vec<ProposedTable>,
}
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedTable {
    pub name: String,
    pub columns: Vec<String>,
}
impl ProposedCatalog {
    pub fn encode(&self) -> Option<Vec<u8>> {
        fn valid(name: &str) -> bool {
            !name.is_empty()
                && name.len() <= 256
                && !name.contains('\0')
                && name == name.to_ascii_lowercase()
        }
        if self.tables.len() > 64 || self.tables.windows(2).any(|w| w[0].name >= w[1].name) {
            return None;
        }
        if self.tables.iter().any(|t| {
            !valid(&t.name)
                || t.columns.len() > 128
                || t.columns.iter().any(|c| !valid(c))
                || t.columns.windows(2).any(|w| w[0] >= w[1])
        }) {
            return None;
        }
        let bytes = serde_json::to_vec(self).ok()?;
        (bytes.len() <= backend_sql_contract::MAX_CONTEXT_BYTES).then_some(bytes)
    }
}
fn proposed_analysis(source: &str, payload: &[u8]) -> Option<Binding> {
    use syntaqlite::analysis::{CatalogLayer, DiagnosticMessage, TableDef};
    use syntaqlite::{AnalysisContext, Analyzer, Catalog, sqlite_dialect};
    if payload.len() > backend_sql_contract::MAX_CONTEXT_BYTES {
        return None;
    }
    let spec: ProposedCatalog = serde_json::from_slice(payload).ok()?;
    if spec.encode()?.as_slice() != payload {
        return None;
    }
    let mut catalog = Catalog::new(sqlite_dialect());
    for table in &spec.tables {
        catalog.layer_mut(CatalogLayer::Database).insert_table(
            TableDef::new(&table.name).columns(table.columns.iter().map(String::as_str)),
        );
    }
    let mut analyzer = Analyzer::new();
    let mut context = AnalysisContext::new(&mut catalog);
    let model = analyzer.analyze(source, &mut context);
    let mut diagnostics = Vec::new();
    for diagnostic in model.diagnostics() {
        if diagnostics.len() >= backend_sql_contract::MAX_TOKENS {
            return None;
        }
        let start = diagnostic.start().as_u32() as usize;
        let end = diagnostic.end().as_u32() as usize;
        source.get(start..end)?;
        let kind = match diagnostic.message() {
            DiagnosticMessage::UnknownColumn { .. } => DiagnosticKind::UnknownColumn,
            _ => DiagnosticKind::Other,
        };
        diagnostics.push(NativeDiagnostic {
            kind,
            extent: Some(Extent { start, end }),
        });
    }
    let statement = model.statements().first()?;
    let (native_lineage, native_relations, native_columns) = match statement.lineage() {
        Some(lineage) => {
            let coverage = if lineage.is_complete() {
                SqliteLineageCoverage::NativeComplete
            } else {
                SqliteLineageCoverage::NativePartial
            };
            (
                coverage,
                statement.relations_accessed()?.into_inner(),
                lineage.into_inner(),
            )
        }
        None => (SqliteLineageCoverage::Unavailable, &[][..], &[][..]),
    };
    if native_relations.len() > 512 || native_columns.len() > 512 {
        return None;
    }
    fn bounded_name(value: &str) -> Option<String> {
        (!value.is_empty() && value.len() <= 256 && !value.contains('\0')).then(|| value.to_owned())
    }
    let relations = native_relations
        .iter()
        .map(|r| {
            Some(SqliteRelationAccess {
                name: bounded_name(&r.name)?,
                kind: match r.kind {
                    syntaqlite::analysis::RelationKind::Table => SqliteRelationKind::Table,
                    syntaqlite::analysis::RelationKind::View => SqliteRelationKind::View,
                },
            })
        })
        .collect::<Option<Vec<_>>>()?;
    let lineage = native_columns
        .iter()
        .map(|c| {
            Some(SqliteColumnLineage {
                output_name: bounded_name(&c.name)?,
                ordinal: c.index,
                origin: match &c.origin {
                    Some(o) => Some(SqliteColumnOrigin {
                        relation: bounded_name(&o.table)?,
                        column: bounded_name(&o.column)?,
                    }),
                    None => None,
                },
            })
        })
        .collect::<Option<Vec<_>>>()?;
    Some(Binding::ProposedAnalysis {
        context_sha256: Sha256::digest(payload).into(),
        diagnostics,
        relations,
        lineage,
        native_lineage,
    })
}
/// Parse with syntaqlite's native SQLite grammar; no catalog binding is claimed.
pub fn analyze_native(request: &Request) -> Response {
    let mut result = Response {
        version: PROTOCOL_VERSION,
        basis: Some(observed_basis()),
        dialect: Dialect::Sqlite,
        engine: Engine::Syntaqlite4e4de76,
        content_sha256: Sha256::digest(&request.source).into(),
        syntax: Syntax::Rejected,
        binding: Binding::NotAttempted,
        outline: None,
    };
    if request
        .expected_basis
        .as_ref()
        .is_some_and(|expected| result.basis.as_ref() != Some(expected))
    {
        result.syntax = Syntax::UnsupportedSettings;
        return result;
    }
    if request.version != PROTOCOL_VERSION {
        result.syntax = Syntax::InitializationUnavailable;
        return result;
    }
    if request.source.len() > MAX_SOURCE_BYTES {
        result.syntax = Syntax::ResourceLimit;
        return result;
    }
    let Ok(source) = std::str::from_utf8(&request.source) else {
        return result;
    };
    if request.source.contains(&0) {
        return result;
    }
    let parser = syntaqlite::Parser::with_config(
        &syntaqlite::parse::ParserConfig::default()
            .with_collect_tokens(true)
            .with_collect_node_extents(true),
    );
    let mut session = parser.parse(source);
    let mut count = 0;
    let mut select_only = true;
    let mut builder = outline::Builder::new();
    let mut outline_ok = true;
    loop {
        match session.next() {
            syntaqlite::ParseOutcome::Ok(stmt) => {
                count += 1;
                select_only &= match stmt.root() {
                    Some(syntaqlite::nodes::Stmt::SelectStmt(_)) => true,
                    Some(syntaqlite::nodes::Stmt::WithClause(with)) => matches!(
                        with.select(),
                        Some(syntaqlite::nodes::Select::SelectStmt(_))
                    ),
                    _ => false,
                };
                outline_ok &= builder.add(&stmt).is_ok();
            }
            syntaqlite::ParseOutcome::Err(_) => {
                result.syntax = Syntax::NativeError;
                return result;
            }
            syntaqlite::ParseOutcome::Done => break,
        }
    }
    let outline = builder.finish();
    if !outline_ok || !outline.validate(&request.source) {
        // The parser's coordinates did not hold together; never report a doubtful outline.
        return result;
    }
    result.outline = Some(outline);
    result.syntax = Syntax::Parsed {
        statements: count,
        identifiers: vec![],
        identifier_coverage: IdentifierCoverage::Unavailable,
    };
    result.binding = match &request.context {
        None => Binding::MissingContext,
        Some(SemanticContext::Proposed {
            dialect: Dialect::Sqlite,
            payload,
        }) if count == 1 && select_only => {
            proposed_analysis(source, payload).unwrap_or(Binding::UnsupportedContext)
        }
        Some(_) => Binding::UnsupportedContext,
    };
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    use backend_sql_contract::Settings;
    #[test]
    fn native_reference_observer_preserves_exact_repeated_utf8_ranges_and_limit() {
        use syntaqlite::analysis::{CatalogLayer, NativeReferenceKind, TableDef};
        use syntaqlite::{AnalysisContext, Analyzer, Catalog, sqlite_dialect};
        let source = "SELECT \"π\", \"π\" FROM \"t\"";
        let make_catalog = || {
            let mut catalog = Catalog::new(sqlite_dialect());
            catalog
                .layer_mut(CatalogLayer::Database)
                .insert_table(TableDef::new("t").columns(["π"]));
            catalog
        };
        let mut plain_catalog = make_catalog();
        let plain = Analyzer::new().analyze(source, &mut AnalysisContext::new(&mut plain_catalog));
        let mut catalog = make_catalog();
        let (observed, refs) = Analyzer::new().analyze_with_native_references(
            source,
            &mut AnalysisContext::new(&mut catalog),
            512,
        );
        assert_eq!(plain.diagnostics().count(), observed.diagnostics().count());
        assert_eq!(plain.statements().len(), observed.statements().len());
        let columns = refs
            .references
            .iter()
            .filter(|r| r.kind == NativeReferenceKind::Column)
            .collect::<Vec<_>>();
        assert_eq!(columns.len(), 2);
        assert_ne!(columns[0].start, columns[1].start);
        for reference in columns {
            assert_eq!(
                source.get(reference.start as usize..reference.end as usize),
                Some("\"π\"")
            );
        }
        assert!(
            refs.references
                .iter()
                .any(|r| r.kind == NativeReferenceKind::Relation
                    && source.get(r.start as usize..r.end as usize) == Some("\"t\""))
        );
        let mut bounded_catalog = make_catalog();
        let (_, bounded) = Analyzer::new().analyze_with_native_references(
            source,
            &mut AnalysisContext::new(&mut bounded_catalog),
            1,
        );
        assert_eq!(bounded.references.len(), 1);
        assert!(bounded.limit_reached);
    }
    #[test]
    fn native_observer_does_not_claim_double_quoted_string_as_resolved_column() {
        use syntaqlite::analysis::NativeReferenceKind;
        use syntaqlite::{AnalysisContext, Analyzer, Catalog, sqlite_dialect};
        let source = "SELECT \"not_a_column\"";
        let mut catalog = Catalog::new(sqlite_dialect());
        let (model, observed) = Analyzer::new().analyze_with_native_references(
            source,
            &mut AnalysisContext::new(&mut catalog),
            512,
        );
        assert!(!model.has_diagnostics());
        assert!(
            !observed
                .references
                .iter()
                .any(|event| event.kind == NativeReferenceKind::Column && event.resolved)
        );
    }
    #[test]
    fn native_sqlite_model_projects_relations_and_output_origins() {
        let payload = ProposedCatalog {
            epoch: 42,
            tables: vec![ProposedTable {
                name: "t".into(),
                columns: vec!["id".into(), "name".into()],
            }],
        }
        .encode()
        .unwrap();
        let request = |source: &str| Request {
            version: backend_sql_contract::PROTOCOL_VERSION,
            expected_basis: None,
            source: source.as_bytes().to_vec(),
            settings: Settings::ProcessDefaults,
            context: Some(SemanticContext::Proposed {
                dialect: Dialect::Sqlite,
                payload: payload.clone(),
            }),
        };
        let direct = analyze_native(&request("SELECT id, name FROM t"));
        let Binding::ProposedAnalysis {
            relations,
            lineage,
            native_lineage,
            ..
        } = direct.binding
        else {
            panic!("native contextual analysis unavailable");
        };
        assert_eq!(native_lineage, SqliteLineageCoverage::NativeComplete);
        assert_eq!(
            relations,
            vec![SqliteRelationAccess {
                name: "t".into(),
                kind: SqliteRelationKind::Table
            }]
        );
        assert_eq!(lineage.len(), 2);
        assert_eq!(
            lineage[0].origin,
            Some(SqliteColumnOrigin {
                relation: "t".into(),
                column: "id".into()
            })
        );
        assert_eq!(
            lineage[1].origin,
            Some(SqliteColumnOrigin {
                relation: "t".into(),
                column: "name".into()
            })
        );
        let cte = analyze_native(&request("WITH t AS (SELECT 1 AS id) SELECT id FROM t"));
        let Binding::ProposedAnalysis { relations, .. } = cte.binding else {
            panic!(
                "cte analysis unavailable: {:?} / {:?}",
                cte.syntax, cte.binding
            );
        };
        assert!(
            relations.is_empty(),
            "CTE name must not become a physical table"
        );
    }
    #[test]
    fn native_analyzer_reports_missing_column_against_declared_schema() {
        use syntaqlite::analysis::{CatalogLayer, DiagnosticMessage, TableDef};
        use syntaqlite::{AnalysisContext, Analyzer, Catalog, sqlite_dialect};
        let mut analyzer = Analyzer::new();
        let mut catalog = Catalog::new(sqlite_dialect());
        catalog
            .layer_mut(CatalogLayer::Database)
            .insert_table(TableDef::new("t").columns(["id", "π"]));
        let mut ctx = AnalysisContext::new(&mut catalog);
        assert!(
            !analyzer
                .analyze("SELECT \"π\", id FROM t", &mut ctx)
                .has_diagnostics()
        );
        let missing = analyzer.analyze("SELECT missing FROM t", &mut ctx);
        let diagnostics: Vec<_> = missing.diagnostics().collect();
        assert_eq!(diagnostics.len(), 1);
        assert!(
            matches!(diagnostics[0].message(), DiagnosticMessage::UnknownColumn { column, .. } if column == "missing")
        );
        assert_eq!(
            (
                diagnostics[0].start().as_u32(),
                diagnostics[0].end().as_u32()
            ),
            (7, 14)
        );
        // SQLite's DQS fallback treats an unresolved double-quoted name as a string.
        assert!(
            !analyzer
                .analyze("SELECT \"missing\" FROM t", &mut ctx)
                .has_diagnostics()
        );
        let bracket = analyzer.analyze("SELECT [missing] FROM t", &mut ctx);
        assert!(
            matches!(bracket.diagnostics().next().unwrap().message(), DiagnosticMessage::UnknownColumn { column, .. } if column == "missing")
        );
    }
    #[test]
    fn unknown_columns_are_not_evidence_of_a_resolved_column() {
        use syntaqlite::analysis::{CatalogLayer, TableDef};
        use syntaqlite::{AnalysisContext, Analyzer, Catalog, sqlite_dialect};
        let mut catalog = Catalog::new(sqlite_dialect());
        catalog
            .layer_mut(CatalogLayer::Database)
            .insert_table(TableDef::new("unknown_columns"));
        let mut context = AnalysisContext::new(&mut catalog);
        assert!(
            !Analyzer::new()
                .analyze("SELECT arbitrary FROM unknown_columns", &mut context)
                .has_diagnostics()
        );
        // ProposedCatalog always calls columns(), even for an empty list.
    }
    #[test]
    fn sqlite_grammar_and_missing_context() {
        let req = Request {
            expected_basis: None,
            version: PROTOCOL_VERSION,
            source: "-- π\r\nSELECT \"π\", \"π\" FROM [table];"
                .as_bytes()
                .to_vec(),
            settings: Settings::ProcessDefaults,
            context: None,
        };
        let out = analyze_native(&req);
        assert!(matches!(
            out.syntax,
            Syntax::Parsed {
                statements: 1,
                identifier_coverage: IdentifierCoverage::Unavailable,
                ..
            }
        ));
        assert_eq!(out.binding, Binding::MissingContext);
        assert_eq!(out.dialect, Dialect::Sqlite);
    }
}

/// Run an isolated SQLite worker using the shared framing and response checks.
pub fn analyze(worker: &std::path::Path, request: &Request) -> Response {
    backend_sql_contract::call_worker(worker, request, Dialect::Sqlite, Engine::Syntaqlite4e4de76)
}

/// Verify source domains and run the admitted native worker through the shared supervisor.
pub fn analyze_checked<'a>(
    request: &backend_sql_contract::Request,
    host: &'a [u8],
    expected_input: backend_sql_contract::InputContentVersion,
    expected_source: backend_sql_contract::ContentId<backend_sql_contract::SourceFactDomain>,
    launch: backend_sql_contract::CheckedLaunch<'_>,
) -> Result<backend_sql_contract::CheckedResponse<'a>, backend_sql_contract::CheckedError> {
    backend_sql_contract::analyze_checked(
        request,
        host,
        expected_input,
        expected_source,
        launch,
        backend_sql_contract::Dialect::Sqlite,
        backend_sql_contract::Engine::Syntaqlite4e4de76,
    )
}
