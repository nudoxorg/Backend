//! Bounded projection of producer-owned Stylo facts into revision-local rows.
//!
//! This is a pure shape and ownership check. It does not authenticate the host
//! bytes or establish that the producer executed; live capture must provide that
//! witness, and workspace admission must authenticate the selected relation.
use super::{MAX_STYLO_RULE_DEPTH, StyloRecordKind, stylo_record_key, stylo_source_occurrence_key};
use backend_semantic::stylo_analysis::{
    StyloAnalysis, StyloCoverage, StyloDeclaration, StyloDiagnostic, StyloPreferenceStability,
    StyloRule, StyloRuleKind, StyloSelector, StyloSelectorComponentKind, StyloSourceDisposition,
    StyloSourceGap, StyloSourceOccurrence, StyloSourceRole, StyloVariableDependency,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// Maximum size of one encoded projected fact.
pub const MAX_STYLO_PROJECTED_ROW_BYTES: usize = 30 * 1024;
/// Maximum encoded fact work in one host projection.
pub const MAX_STYLO_PROJECTED_BYTES: usize = 64 * 1024 * 1024;

/// A native stylesheet fact. None of these claims is a verified host byte span.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub enum StyloProjectedRow {
    /// A native rule in its stylesheet hierarchy.
    Rule(StyloRule),
    /// A native parsed selector.
    Selector(StyloSelector),
    /// A declaration retained by Stylo after normalization and deduplication.
    RetainedDeclaration(StyloDeclaration),
    /// A native exposed variable reference, without a resolved provider.
    Dependency(StyloVariableDependency),
    /// A source site claimed by the bounded native observer; host bytes must be verified separately.
    SourceOccurrence(StyloSourceOccurrence),
    /// A native diagnostic, with approximate location only.
    Diagnostic(StyloDiagnostic),
}

/// One projected native claim and the local coordinates used to derive its key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StyloProjectedFact {
    /// Revision-scoped fact key.
    pub key: [u8; 32],
    /// Within-family native ordinal.
    pub ordinal: u32,
    /// Within-parent child ordinal.
    pub subordinal: u32,
    /// The producer-owned native semantic claim.
    pub fact: StyloProjectedRow,
}

// Borrow during bounded encoding; clone only after the row fits its budget.
#[derive(Serialize)]
enum BorrowedRow<'a> {
    Rule(&'a StyloRule),
    Selector(&'a StyloSelector),
    RetainedDeclaration(&'a StyloDeclaration),
    Dependency(&'a StyloVariableDependency),
    SourceOccurrence(&'a StyloSourceOccurrence),
    Diagnostic(&'a StyloDiagnostic),
}
impl BorrowedRow<'_> {
    fn owned(&self) -> StyloProjectedRow {
        match self {
            Self::Rule(value) => StyloProjectedRow::Rule((*value).clone()),
            Self::Selector(value) => StyloProjectedRow::Selector((*value).clone()),
            Self::RetainedDeclaration(value) => {
                StyloProjectedRow::RetainedDeclaration((*value).clone())
            }
            Self::Dependency(value) => StyloProjectedRow::Dependency((*value).clone()),
            Self::SourceOccurrence(value) => StyloProjectedRow::SourceOccurrence((*value).clone()),
            Self::Diagnostic(value) => StyloProjectedRow::Diagnostic((*value).clone()),
        }
    }
}
struct BoundedEncoding {
    bytes: Vec<u8>,
    limit: usize,
}
impl std::io::Write for BoundedEncoding {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
            return Err(std::io::Error::other("encoded Stylo value exceeds budget"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(crate) fn bounded_encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, String> {
    let mut output = BoundedEncoding {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value).map_err(|error| error.to_string())?;
    Ok(output.bytes)
}

/// Effective supervisor policy for one native worker attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StyloWorkerPolicy {
    /// Supervisory wall-time limit in nanoseconds.
    pub wall_time_nanos: u64,
    /// Maximum framed request bytes.
    pub input_bytes: u64,
    /// Maximum standard-output bytes.
    pub stdout_bytes: u64,
    /// Maximum standard-error bytes.
    pub stderr_bytes: u64,
    /// Maximum aggregate captured output bytes.
    pub output_bytes: u64,
    /// Requested hard memory cap. None means no hard cap is claimed.
    pub memory_bytes: Option<u64>,
}
impl StyloWorkerPolicy {
    /// Rejects invalid zero or inconsistent process budgets.
    pub fn validate(&self) -> Result<(), String> {
        if self.wall_time_nanos == 0
            || self.input_bytes == 0
            || self.stdout_bytes == 0
            || self.stderr_bytes == 0
            || self.output_bytes == 0
            || self.memory_bytes == Some(0)
            || self.stdout_bytes > self.output_bytes
            || self.stderr_bytes > self.output_bytes
        {
            return Err("invalid Stylo worker policy".into());
        }
        Ok(())
    }
}

fn source_gap_order(gap: StyloSourceGap) -> u8 {
    match gap {
        StyloSourceGap::NestedOrComplexUncovered => 0,
        StyloSourceGap::CustomPropertyReferencesUncovered => 1,
        StyloSourceGap::ObserverSkipped => 2,
        StyloSourceGap::ParserDiagnostic => 3,
    }
}

/// Exact host and producer identities supplied by the capture authority.
pub struct StyloProjectionBasis<'a> {
    /// Owning project identity.
    pub project: [u8; 32],
    /// Canonical relative source path.
    pub path: &'a str,
    /// Identity of actual captured host bytes.
    pub content: [u8; 32],
    /// Typed source-fact identity of the same captured bytes.
    pub source: [u8; 32],
    /// Actual captured source length.
    pub byte_length: u64,
    /// Fingerprint of the native producer and its support dependencies.
    pub producer: [u8; 32],
    /// Exact admitted supervised worker artifact identity.
    pub worker_artifact: [u8; 32],
    /// Fingerprint of the admission adapter and its source-walk support.
    pub adapter: [u8; 32],
    /// Exact policy enforced or requested for the supervised attempt.
    pub worker_policy: &'a StyloWorkerPolicy,
}

/// Commits the declared native recipe and parser configuration.
///
/// # Errors
/// Rejects empty or oversized recipe and configuration values.
pub fn stylo_parser_configuration_id(
    recipe: &str,
    config: &backend_semantic::stylo_analysis::StyloParseConfig,
) -> Result<[u8; 32], String> {
    if recipe.is_empty() || recipe.len() > 4096 {
        return Err("invalid Stylo recipe".into());
    }
    let encoded = bounded_encode(config, MAX_STYLO_PROJECTED_ROW_BYTES)?;
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-standalone-v1.config\0");
    hash.update(&(recipe.len() as u64).to_be_bytes());
    hash.update(recipe.as_bytes());
    hash.update(&(encoded.len() as u64).to_be_bytes());
    hash.update(&encoded);
    Ok(*hash.finalize().as_bytes())
}

/// Validates native rule ownership and emits individually bounded keyed facts.
/// The configuration digest binds actual parsing options and resource limits.
/// It does not assert that source positions correspond to exact byte offsets.
///
/// # Errors
/// Returns an error for invalid facts, duplicate identities, or work limits.
pub fn project_stylo_facts(
    basis: &StyloProjectionBasis<'_>,
    facts: &StyloAnalysis,
) -> Result<([u8; 32], Vec<StyloProjectedFact>), String> {
    if facts.selectors.len() > 1_000_000
        || facts.declarations.len() > 1_000_000
        || facts.dependencies.len() > 1_000_000
        || facts.rules.len() > 1_000_000
        || facts.diagnostics.len() > 1_000_000
        || facts.occurrences.len() > 1_000_000
    {
        return Err("Stylo facts exceed hard count limit".into());
    }
    let records = facts
        .selectors
        .len()
        .checked_add(facts.declarations.len())
        .and_then(|count| count.checked_add(facts.dependencies.len()))
        .and_then(|count| count.checked_add(facts.occurrences.len()))
        .and_then(|count| {
            facts.selectors.iter().try_fold(count, |total, selector| {
                total.checked_add(selector.components.len())
            })
        })
        .ok_or("Stylo record count overflow")?;
    if facts.support_fingerprint != basis.producer || basis.producer == [0; 32] {
        return Err("Stylo producer fingerprint mismatch".into());
    }
    if records > 1_000_000
        || facts.rules.len() > 1_000_000
        || facts.diagnostics.len() > 1_000_000
        || basis.byte_length > facts.config.limits.input_bytes
        || facts.rules.len() > facts.config.limits.rules as usize
        || records > facts.config.limits.records as usize
        || facts.diagnostics.len() > facts.config.limits.diagnostics as usize
    {
        return Err("Stylo facts exceed attempt limits".into());
    }
    let mut text_bytes = 0u64;
    let mut charge = |text: &str| -> Result<(), String> {
        text_bytes = text_bytes
            .checked_add(text.len() as u64)
            .ok_or("Stylo text work overflow")?;
        if text_bytes > facts.config.limits.text_bytes {
            return Err("Stylo facts exceed text budget".into());
        }
        Ok(())
    };
    for rule in &facts.rules {
        charge(&rule.native_kind)?;
        if let Some(condition) = &rule.condition {
            charge(condition)?;
        }
    }
    for selector in &facts.selectors {
        charge(&selector.canonical)?;
        for component in &selector.components {
            charge(&component.name)?;
        }
    }
    for declaration in &facts.declarations {
        charge(&declaration.property)?;
        charge(&declaration.value)?;
    }
    for dependency in &facts.dependencies {
        charge(&dependency.name)?;
        charge(&dependency.kind)?;
    }
    for occurrence in &facts.occurrences {
        charge(&occurrence.raw)?;
        charge(&occurrence.decoded)?;
    }
    for diagnostic in &facts.diagnostics {
        charge(&diagnostic.message)?;
    }
    if facts.source_coverage != StyloCoverage::Partial
        || facts.source_gaps.len() > 4
        || !facts
            .source_gaps
            .contains(&StyloSourceGap::NestedOrComplexUncovered)
        || !facts
            .source_gaps
            .contains(&StyloSourceGap::CustomPropertyReferencesUncovered)
        || (!facts.diagnostics.is_empty()
            && !facts
                .source_gaps
                .contains(&StyloSourceGap::ParserDiagnostic))
        || facts
            .source_gaps
            .windows(2)
            .any(|pair| source_gap_order(pair[0]) >= source_gap_order(pair[1]))
        || facts.source_observer_recipe.is_empty()
        || facts.source_observer_recipe.len() > 4096
    {
        return Err("invalid Stylo source observer coverage".into());
    }
    basis.worker_policy.validate()?;
    let parser_configuration = stylo_parser_configuration_id(&facts.recipe, &facts.config)?;
    let mut config_hash = blake3::Hasher::new();
    config_hash.update(b"nudox.stylo-standalone-v1.host-basis\0");
    config_hash.update(&parser_configuration);
    config_hash.update(&basis.producer);
    config_hash.update(&basis.worker_artifact);
    config_hash.update(&basis.adapter);
    let worker_policy = bounded_encode(basis.worker_policy, MAX_STYLO_PROJECTED_ROW_BYTES)?;
    config_hash.update(&(worker_policy.len() as u64).to_be_bytes());
    config_hash.update(&worker_policy);
    config_hash.update(&[u8::from(facts.utf8_bom_removed)]);
    let source_observer = bounded_encode(
        &(
            &facts.source_observer_recipe,
            &facts.source_coverage,
            &facts.source_gaps,
        ),
        MAX_STYLO_PROJECTED_ROW_BYTES,
    )?;
    config_hash.update(&(source_observer.len() as u64).to_be_bytes());
    config_hash.update(&source_observer);
    let preferences = bounded_encode(&facts.preferences, MAX_STYLO_PROJECTED_ROW_BYTES)?;
    let stability = bounded_encode(&facts.preference_stability, MAX_STYLO_PROJECTED_ROW_BYTES)?;
    config_hash.update(&(preferences.len() as u64).to_be_bytes());
    config_hash.update(&preferences);
    config_hash.update(&(stability.len() as u64).to_be_bytes());
    config_hash.update(&stability);
    let config_id = *config_hash.finalize().as_bytes();
    if facts.coverage == StyloCoverage::Complete
        && (facts.preference_stability == StyloPreferenceStability::Unknown
            || !facts.diagnostics.is_empty()
            || facts.rules.iter().any(|rule| {
                rule.kind == StyloRuleKind::Other
                    || (rule.kind == StyloRuleKind::Media && rule.condition.is_none())
            }))
    {
        return Err("complete Stylo projection has known gaps".into());
    }
    let mut paths = BTreeMap::<&[u32], StyloRuleKind>::new();
    let mut prior = None::<&[u32]>;
    for rule in &facts.rules {
        if rule.native_kind.is_empty()
            || rule.path.is_empty()
            || rule.path.len() > MAX_STYLO_RULE_DEPTH
            || rule.path.len() > facts.config.limits.depth as usize
            || prior.is_some_and(|previous| previous >= rule.path.as_slice())
            || rule.location.is_some_and(|location| location.column == 0)
            || (rule.path.len() > 1 && !paths.contains_key(&rule.path[..rule.path.len() - 1]))
        {
            return Err("invalid Stylo rule hierarchy".into());
        }
        paths.insert(rule.path.as_slice(), rule.kind);
        prior = Some(&rule.path);
    }
    let mut rows = Vec::new();
    let mut identities = BTreeSet::new();
    let mut encoded_bytes = 0usize;
    let mut add =
        |path: &[u32], kind, ordinal, subordinal, row: BorrowedRow<'_>| -> Result<(), String> {
            let key = match &row {
                BorrowedRow::SourceOccurrence(value) => stylo_source_occurrence_key(
                    basis.project,
                    basis.path,
                    basis.content,
                    basis.source,
                    basis.producer,
                    basis.worker_artifact,
                    basis.adapter,
                    config_id,
                    &facts.source_observer_recipe,
                    facts.utf8_bom_removed,
                    value.role,
                    value.start,
                    value.end,
                ),
                _ => stylo_record_key(
                    basis.project,
                    basis.path,
                    basis.content,
                    basis.source,
                    basis.producer,
                    basis.worker_artifact,
                    basis.adapter,
                    config_id,
                    path,
                    kind,
                    ordinal,
                    subordinal,
                ),
            }
            .map_err(str::to_owned)?;
            if !identities.insert(key) {
                return Err("duplicate Stylo fact identity".into());
            }
            let encoded = bounded_encode(&row, MAX_STYLO_PROJECTED_ROW_BYTES)?;
            if encoded.len() > MAX_STYLO_PROJECTED_ROW_BYTES {
                return Err("Stylo fact exceeds row budget".into());
            }
            encoded_bytes = encoded_bytes
                .checked_add(encoded.len())
                .ok_or("Stylo projection work overflow")?;
            if encoded_bytes > MAX_STYLO_PROJECTED_BYTES {
                return Err("Stylo projection exceeds work budget".into());
            }
            rows.push(StyloProjectedFact {
                key,
                ordinal,
                subordinal,
                fact: row.owned(),
            });
            Ok(())
        };
    for rule in &facts.rules {
        add(
            &rule.path,
            StyloRecordKind::Rule,
            0,
            0,
            BorrowedRow::Rule(rule),
        )?;
    }
    let mut selectors_by_key = BTreeMap::new();
    for selector in &facts.selectors {
        if selectors_by_key
            .insert((selector.rule_path.as_slice(), selector.ordinal), selector)
            .is_some()
        {
            return Err("duplicate Stylo selector ordinal".into());
        }
        if paths.get(selector.rule_path.as_slice()) != Some(&StyloRuleKind::Style)
            || selector.canonical.is_empty()
        {
            return Err("invalid Stylo selector owner".into());
        }
        add(
            &selector.rule_path,
            StyloRecordKind::Selector,
            selector.ordinal,
            0,
            BorrowedRow::Selector(selector),
        )?;
    }
    let mut declarations = BTreeSet::new();
    for declaration in &facts.declarations {
        if paths.get(declaration.rule_path.as_slice()) != Some(&StyloRuleKind::Style)
            || declaration.property.is_empty()
            || !declarations.insert((declaration.rule_path.as_slice(), declaration.ordinal))
        {
            return Err("invalid Stylo declaration owner".into());
        }
        add(
            &declaration.rule_path,
            StyloRecordKind::RetainedDeclaration,
            declaration.ordinal,
            0,
            BorrowedRow::RetainedDeclaration(declaration),
        )?;
    }
    let mut dependency_ordinals = BTreeMap::<(&[u32], u32), u32>::new();
    let mut reference_paths = BTreeSet::<(&[u32], u32, &[u32])>::new();
    for dependency in &facts.dependencies {
        let owner = (
            dependency.rule_path.as_slice(),
            dependency.declaration_ordinal,
        );
        if !declarations.contains(&owner)
            || dependency.name.is_empty()
            || !matches!(dependency.kind.as_str(), "var" | "env" | "attr")
            || dependency.reference_path.is_empty()
            || dependency.reference_path.len() > MAX_STYLO_RULE_DEPTH
            || dependency.reference_path.len() > facts.config.limits.depth as usize
            || (dependency.reference_path.len() > 1
                && !reference_paths.contains(&(
                    owner.0,
                    owner.1,
                    &dependency.reference_path[..dependency.reference_path.len() - 1],
                )))
            || !reference_paths.insert((owner.0, owner.1, dependency.reference_path.as_slice()))
        {
            return Err("invalid Stylo dependency owner".into());
        }
        let ordinal = dependency_ordinals.entry(owner).or_default();
        add(
            &dependency.rule_path,
            StyloRecordKind::Dependency,
            dependency.declaration_ordinal,
            *ordinal,
            BorrowedRow::Dependency(dependency),
        )?;
        *ordinal = ordinal
            .checked_add(1)
            .ok_or("Stylo dependency ordinal overflow")?;
    }
    let mut previous_end = None;
    for (ordinal, occurrence) in facts.occurrences.iter().enumerate() {
        if occurrence.rule_path.len() != 1
            || paths.get(occurrence.rule_path.as_slice()) != Some(&StyloRuleKind::Style)
            || occurrence.start >= occurrence.end
            || occurrence.end > basis.byte_length
            || occurrence.end - occurrence.start != occurrence.raw.len() as u64
            || occurrence.decoded.is_empty()
            || occurrence.disposition != StyloSourceDisposition::Unknown
            || (facts.utf8_bom_removed && occurrence.start < 3)
            || previous_end.is_some_and(|end| end > occurrence.start)
        {
            return Err("invalid Stylo source site".into());
        }
        match occurrence.role {
            StyloSourceRole::ClassName => {
                let selector = occurrence
                    .selector_ordinal
                    .and_then(|ordinal| {
                        selectors_by_key.get(&(occurrence.rule_path.as_slice(), ordinal))
                    })
                    .ok_or("Stylo class source has no native selector")?;
                if !selector.components.iter().any(|component| {
                    component.kind == StyloSelectorComponentKind::Class
                        && component.name == occurrence.decoded
                }) {
                    return Err("Stylo class source/name mismatch".into());
                }
            }
            StyloSourceRole::CustomPropertyDeclarationName => {
                if occurrence.selector_ordinal.is_some()
                    || !occurrence.decoded.starts_with("--")
                    || occurrence.decoded.len() <= 2
                {
                    return Err("invalid Stylo custom-property source site".into());
                }
            }
        }
        previous_end = Some(occurrence.end);
        let ordinal = u32::try_from(ordinal).map_err(|_| "too many Stylo source sites")?;
        add(
            &occurrence.rule_path,
            StyloRecordKind::SourceOccurrence,
            ordinal,
            0,
            BorrowedRow::SourceOccurrence(occurrence),
        )?;
    }
    for (ordinal, diagnostic) in facts.diagnostics.iter().enumerate() {
        if diagnostic.location.column == 0 || !diagnostic.location.approximate {
            return Err("invalid Stylo diagnostic location".into());
        }
        let ordinal = u32::try_from(ordinal).map_err(|_| "too many Stylo diagnostics")?;
        add(
            &[],
            StyloRecordKind::Diagnostic,
            ordinal,
            0,
            BorrowedRow::Diagnostic(diagnostic),
        )?;
    }
    rows.sort_by_key(|row| row.key);
    Ok((config_id, rows))
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::stylo_analysis::{
        StyloDependencyCoverage, StyloLimits, StyloParseConfig,
    };

    fn fixture() -> StyloAnalysis {
        StyloAnalysis {
            config: StyloParseConfig {
                source_kind: backend_semantic::stylo_analysis::StyloSourceKind::StandaloneUtf8,
                base_url: "https://example.invalid/a.css".into(),
                author_origin: true,
                no_quirks: true,
                allow_imports: false,
                limits: StyloLimits {
                    input_bytes: 1000,
                    rules: 10,
                    depth: 10,
                    records: 10,
                    diagnostics: 10,
                    text_bytes: 1000,
                },
            },
            recipe: "stylo-test".into(),
            support_fingerprint: [3; 32],
            utf8_bom_removed: false,
            preferences: backend_semantic::stylo_analysis::StyloPreferenceSnapshot {
                booleans: vec![true],
                integers: vec![1],
            },
            preference_stability: StyloPreferenceStability::Unknown,
            rules: vec![StyloRule {
                path: vec![0],
                kind: StyloRuleKind::Style,
                native_kind: "Style".into(),
                condition: None,
                location: None,
            }],
            selectors: vec![StyloSelector {
                rule_path: vec![0],
                ordinal: 0,
                canonical: ".button".into(),
                specificity: 0x400,
                components: vec![],
                component_coverage: StyloDependencyCoverage::Unknown,
            }],
            declarations: vec![],
            dependencies: vec![],
            occurrences: vec![],
            source_coverage: StyloCoverage::Partial,
            source_gaps: vec![
                StyloSourceGap::NestedOrComplexUncovered,
                StyloSourceGap::CustomPropertyReferencesUncovered,
            ],
            source_observer_recipe: "test-observer-v1".into(),
            diagnostics: vec![],
            coverage: StyloCoverage::Partial,
        }
    }
    fn basis() -> StyloProjectionBasis<'static> {
        StyloProjectionBasis {
            project: [1; 32],
            path: "src/a.css",
            content: [2; 32],
            source: [4; 32],
            byte_length: 20,
            producer: [3; 32],
            worker_artifact: [7; 32],
            adapter: [8; 32],
            worker_policy: &StyloWorkerPolicy {
                wall_time_nanos: 1,
                input_bytes: 1024,
                stdout_bytes: 1024,
                stderr_bytes: 64,
                output_bytes: 2048,
                memory_bytes: None,
            },
        }
    }
    #[test]
    fn equal_content_at_different_stylesheet_urls_has_distinct_native_basis() {
        let first = fixture();
        let mut second = first.clone();
        second.config.base_url = "https://example.invalid/other/a.css".into();
        let first_basis = basis();
        let second_basis = StyloProjectionBasis {
            path: "other/a.css",
            ..basis()
        };
        let (first_context, first_rows) = project_stylo_facts(&first_basis, &first).unwrap();
        let (second_context, second_rows) = project_stylo_facts(&second_basis, &second).unwrap();
        let changed_adapter = StyloProjectionBasis {
            adapter: [9; 32],
            ..basis()
        };
        let (adapter_context, adapter_rows) =
            project_stylo_facts(&changed_adapter, &first).unwrap();
        assert_ne!(first_context, adapter_context);
        assert_ne!(first_rows[0].key, adapter_rows[0].key);
        assert_ne!(first_context, second_context);
        assert_ne!(first_rows[0].key, second_rows[0].key);
        let mut changed_bom = first.clone();
        changed_bom.utf8_bom_removed = true;
        let (bom_context, _) = project_stylo_facts(&first_basis, &changed_bom).unwrap();
        assert_ne!(first_context, bom_context);
        let coverage = super::super::complete_coverage(
            backend_execution::AuthorityVersion::from_value(b"Stylo test authority"),
        )
        .unwrap();
        let as_entry = |row: &StyloProjectedFact| {
            (
                row.key,
                super::super::StyloAnalysisRecord::Fact {
                    host: [9; 32],
                    ordinal: row.ordinal,
                    subordinal: row.subordinal,
                    fact: row.fact.clone(),
                },
            )
        };
        let first_root =
            backend_version::RelationState::<super::super::StyloAnalysisRelation>::from_entries(
                [as_entry(&first_rows[0])],
                coverage.clone(),
            )
            .unwrap();
        let second_root =
            backend_version::RelationState::<super::super::StyloAnalysisRelation>::from_entries(
                [as_entry(&second_rows[0])],
                coverage,
            )
            .unwrap();
        assert_ne!(first_root.root(), second_root.root());
    }
    #[test]
    fn observed_sites_are_distinct_and_duplicate_physical_claims_refused() {
        use backend_semantic::stylo_analysis::StyloSelectorComponent;
        let mut facts = fixture();
        facts.selectors[0].components.push(StyloSelectorComponent {
            kind: StyloSelectorComponentKind::Class,
            name: "button".into(),
        });
        let site = |start| StyloSourceOccurrence {
            rule_path: vec![0],
            selector_ordinal: Some(0),
            role: StyloSourceRole::ClassName,
            start,
            end: start + 6,
            raw: "button".into(),
            decoded: "button".into(),
            disposition: StyloSourceDisposition::Unknown,
        };
        facts.occurrences = vec![site(1), site(10)];
        let (_, rows) = project_stylo_facts(&basis(), &facts).unwrap();
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(&row.fact, StyloProjectedRow::SourceOccurrence(_)))
                .count(),
            2
        );
        facts.occurrences.push(site(10));
        assert!(project_stylo_facts(&basis(), &facts).is_err());
    }
    #[test]
    fn oversized_native_row_is_refused_without_truncation() {
        let mut facts = fixture();
        facts.config.limits.text_bytes = 100_000;
        facts.selectors[0].canonical = "x".repeat(MAX_STYLO_PROJECTED_ROW_BYTES + 1);
        assert!(project_stylo_facts(&basis(), &facts).is_err());
    }
    #[test]
    fn projection_binds_native_basis_and_rejects_forged_ownership() {
        let mut facts = fixture();
        let (config, rows) = project_stylo_facts(&basis(), &facts).unwrap();
        assert_eq!(rows.len(), 2);
        facts.config.base_url = "https://example.invalid/b.css".into();
        let (different_config, changed) = project_stylo_facts(&basis(), &facts).unwrap();
        assert_ne!(config, different_config);
        assert_ne!(rows[0].key, changed[0].key);
        facts.selectors[0].rule_path = vec![1];
        assert!(project_stylo_facts(&basis(), &facts).is_err());
        let mut facts = fixture();
        facts.support_fingerprint = [9; 32];
        assert!(project_stylo_facts(&basis(), &facts).is_err());
        let mut facts = fixture();
        facts.rules[0].kind = StyloRuleKind::Other;
        facts.coverage = StyloCoverage::Complete;
        assert!(project_stylo_facts(&basis(), &facts).is_err());
    }
}
