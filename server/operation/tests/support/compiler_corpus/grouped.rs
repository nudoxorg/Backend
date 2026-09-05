//! One bounded source/authority/publication transaction per language.
//!
//! The legacy matrix compiled every generated declaration independently and
//! repeated that work for each permutation.  This module keeps the same
//! source facts, but composes the already typed `CorpusPackage::render`
//! outputs into one source unit per language.  Each unit is compiled, stored,
//! shut down, reopened, and observed once.  Individual declarations are then
//! joined by their source-name span and `CaseId`; no canonical iterator
//! position is used as identity.

use super::*;

use super::authority::{go_fixture_for_source, rust_fixture_for_source};
use super::comparison::GroupedField;
use super::execution::{compile_with_authority_until, terminal_kind};
use super::observation::{
    observe_entity_at_source, observe_owned_semantic, observe_reopened_semantic,
};

const CASES_PER_BATCH: usize = multilingual_corpus::CASES_PER_LANGUAGE;
const BATCH_SOURCE_LIMIT: usize = 64 * 1024;
const BATCH_FRAGMENT_LIMIT: usize = 16 * 1024 * 1024;
const BATCH_DEADLINE: Duration = Duration::from_secs(20);
const BATCH_AUTHORITY_DEADLINE: Duration = Duration::from_secs(12);

/// The order gives cheap, locally unavailable lanes a chance to report before
/// a potentially slow Rust-analyzer transaction.  It is fixed and independent
/// of source declaration or entity order.
const LANGUAGE_ORDER: [CorpusLanguage; CorpusLanguage::ALL.len()] = [
    CorpusLanguage::TypeScript,
    CorpusLanguage::Python,
    CorpusLanguage::Go,
    CorpusLanguage::CSharp,
    CorpusLanguage::Java,
    CorpusLanguage::Clang,
    CorpusLanguage::Rust,
];

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct BatchRange {
    start: u32,
    end: u32,
}

impl BatchRange {
    fn from_bounds(start: usize, end: usize) -> Result<Self, GroupedSourceError> {
        let start = u32::try_from(start).map_err(|_| GroupedSourceError::RangeOverflow)?;
        let end = u32::try_from(end).map_err(|_| GroupedSourceError::RangeOverflow)?;
        (start <= end)
            .then_some(Self { start, end })
            .ok_or(GroupedSourceError::InvalidRange)
    }

    fn contains(self, other: Self) -> bool {
        self.start <= other.start && self.end >= other.end
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct BatchCase {
    package: CorpusPackage,
    source: BatchRange,
    name: BatchRange,
    member: Option<BatchRange>,
}

struct BatchSource {
    language: CorpusLanguage,
    bytes: Vec<u8>,
    cases: Box<[BatchCase; CASES_PER_BATCH]>,
}

impl BatchSource {
    fn build(language: CorpusLanguage) -> Result<Self, GroupedSourceError> {
        let mut bytes = Vec::with_capacity(16 * 1024);
        let mut cases = Vec::with_capacity(CASES_PER_BATCH);
        let packages: Vec<_> = corpus_packages()
            .filter(|package| package.language == language)
            .collect();
        if packages.len() != CASES_PER_BATCH {
            return Err(GroupedSourceError::CaseCount {
                language,
                observed: packages.len(),
                expected: CASES_PER_BATCH,
            });
        }
        for (index, package) in packages.into_iter().enumerate() {
            let mut rendered_bytes = [0_u8; SOURCE_BYTE_LIMIT];
            let rendered = package
                .render(&mut rendered_bytes)
                .map_err(GroupedSourceError::Render)?;
            let transformed = transform_source(
                language,
                package,
                index == 0,
                rendered.source.as_bytes(),
            )?;
            let source_start = bytes.len();
            bytes.extend_from_slice(&transformed);
            let source_end = bytes.len();
            let expected_name = transformed_name(language, package, rendered.expected_symbol);
            let name_offset = find_subslice(&transformed, &expected_name)
                .ok_or(GroupedSourceError::MissingName { package })?;
            let name = BatchRange::from_bounds(
                source_start
                    .checked_add(name_offset)
                    .ok_or(GroupedSourceError::RangeOverflow)?,
                source_start
                    .checked_add(
                        name_offset
                            .checked_add(expected_name.len())
                            .ok_or(GroupedSourceError::RangeOverflow)?,
                    )
                    .ok_or(GroupedSourceError::RangeOverflow)?,
            )?;
            let member = rendered
                .expected_member
                .map(|member| {
                    let offset = find_subslice(&transformed, member.as_bytes())
                        .ok_or(GroupedSourceError::MissingMember { package })?;
                    BatchRange::from_bounds(
                        source_start
                            .checked_add(offset)
                            .ok_or(GroupedSourceError::RangeOverflow)?,
                        source_start
                            .checked_add(
                                offset
                                    .checked_add(member.len())
                                    .ok_or(GroupedSourceError::RangeOverflow)?,
                            )
                            .ok_or(GroupedSourceError::RangeOverflow)?,
                    )
                })
                .transpose()?;
            cases.push(BatchCase {
                package,
                source: BatchRange::from_bounds(source_start, source_end)?,
                name,
                member,
            });
        }
        if bytes.len() > BATCH_SOURCE_LIMIT {
            return Err(GroupedSourceError::SourceLimit {
                limit: BATCH_SOURCE_LIMIT,
                observed: bytes.len(),
            });
        }
        let cases = cases
            .into_boxed_slice()
            .try_into()
            .map_err(|_| GroupedSourceError::CaseCount {
                language,
                observed: CASES_PER_BATCH,
                expected: CASES_PER_BATCH,
            })?;
        Ok(Self {
            language,
            bytes,
            cases,
        })
    }

    fn case(&self, index: usize) -> BatchCase {
        self.cases[index]
    }

    fn source(&self) -> &[u8] {
        &self.bytes
    }

    fn file(&self) -> &'static [u8] {
        self.case(0).package.scope_spec().path.as_bytes()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Error)]
pub(super) enum GroupedSourceError {
    #[error("grouped {language:?} source rendered {observed} cases, expected {expected}")]
    CaseCount {
        language: CorpusLanguage,
        observed: usize,
        expected: usize,
    },
    #[error("grouped source renderer failed: {0}")]
    Render(#[from] multilingual_corpus::CorpusRenderError),
    #[error("grouped source for {package:?} did not contain its typed primary name")]
    MissingName { package: CorpusPackage },
    #[error("grouped source for {package:?} did not contain its typed member name")]
    MissingMember { package: CorpusPackage },
    #[error("grouped source range exceeded u32 address space")]
    RangeOverflow,
    #[error("grouped source produced an inverted range")]
    InvalidRange,
    #[error("grouped source exceeded its {limit}-byte lease with {observed} bytes")]
    SourceLimit { limit: usize, observed: usize },
    #[error("grouped source could not normalize its generated language envelope")]
    InvalidEnvelope,
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn transformed_name(
    language: CorpusLanguage,
    package: CorpusPackage,
    rendered_name: &str,
) -> Vec<u8> {
    if language == CorpusLanguage::Java
        && matches!(package.shape, PackageShape::Aggregate | PackageShape::Generic)
    {
        format!("Package{:03}", package.ordinal).into_bytes()
    } else {
        rendered_name.as_bytes().to_vec()
    }
}

fn transform_source(
    language: CorpusLanguage,
    package: CorpusPackage,
    first: bool,
    source: &[u8],
) -> Result<Vec<u8>, GroupedSourceError> {
    match language {
        CorpusLanguage::Go => {
            const PREFIX: &[u8] = b"package fixture\n\n";
            if first {
                Ok(source.to_vec())
            } else {
                source
                    .strip_prefix(PREFIX)
                    .map(ToOwned::to_owned)
                    .ok_or(GroupedSourceError::InvalidEnvelope)
            }
        }
        CorpusLanguage::Java => normalize_java_source(package, source),
        _ => Ok(source.to_vec()),
    }
}

fn normalize_java_source(
    package: CorpusPackage,
    source: &[u8],
) -> Result<Vec<u8>, GroupedSourceError> {
    let replacement = format!("Package{:03}", package.ordinal);
    let (marker, replacement_prefix) = if source
        .windows(b"public final class Package".len())
        .any(|window| window == b"public final class Package")
    {
        (&b"public final class Package"[..], &b"final class "[..])
    } else if source
        .windows(b"public class Package".len())
        .any(|window| window == b"public class Package")
    {
        (&b"public class Package"[..], &b"class "[..])
    } else {
        return Err(GroupedSourceError::InvalidEnvelope);
    };
    let start = find_subslice(source, marker).ok_or(GroupedSourceError::InvalidEnvelope)?;
    let class_name_end = start
        .checked_add(marker.len())
        .ok_or(GroupedSourceError::RangeOverflow)?;
    let mut output = Vec::with_capacity(
        source
            .len()
            .checked_add(replacement.len())
            .ok_or(GroupedSourceError::RangeOverflow)?,
    );
    output.extend_from_slice(&source[..start]);
    output.extend_from_slice(replacement_prefix);
    output.extend_from_slice(replacement.as_bytes());
    output.extend_from_slice(&source[class_name_end..]);
    Ok(output)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct GroupedLanguageSummary {
    language: CorpusLanguage,
    declarations: usize,
    output: usize,
    unavailable: usize,
    terminals: usize,
    mismatches: usize,
    elapsed: Duration,
}

#[derive(Default)]
struct GroupedAuditSummary {
    languages: Vec<GroupedLanguageSummary>,
    mismatches: Vec<CorpusMismatch>,
}

/// Runs the one-batch-per-language audit.  The returned mismatch list is
/// intentionally red for every unavailable/unsupported/observer-deferred
/// row; callers must not interpret a missing producer as parity.
pub(super) fn run_grouped_audit(
    hosts: &HostTools,
    resolved: &ResolvedTools<'_>,
    authorities: &AuthorityFactory,
) -> Result<(), CorpusAuditError> {
    let started = Instant::now();
    let mut summary = GroupedAuditSummary::default();
    for language in LANGUAGE_ORDER {
        let language_started = Instant::now();
        let result = run_language(language, hosts, resolved, authorities)?;
        let language_summary = GroupedLanguageSummary {
            language,
            declarations: CASES_PER_BATCH,
            output: result.output,
            unavailable: result.unavailable,
            terminals: result.terminals,
            mismatches: result.mismatches.len(),
            elapsed: language_started.elapsed(),
        };
        let first_mismatch = result.mismatches.first().copied();
        eprintln!(
            "grouped-corpus language={:?} declarations={} output={} unavailable={} terminals={} mismatches={} elapsed_ms={} first={:?}",
            language_summary.language,
            language_summary.declarations,
            language_summary.output,
            language_summary.unavailable,
            language_summary.terminals,
            language_summary.mismatches,
            language_summary.elapsed.as_millis(),
            first_mismatch,
        );
        summary.languages.push(language_summary);
        summary.mismatches.extend(result.mismatches);
    }
    eprintln!(
        "grouped-corpus total_declarations={} output={} unavailable={} terminals={} mismatches={} elapsed_ms={}",
        summary
            .languages
            .iter()
            .map(|language| language.declarations)
            .sum::<usize>(),
        summary
            .languages
            .iter()
            .map(|language| language.output)
            .sum::<usize>(),
        summary
            .languages
            .iter()
            .map(|language| language.unavailable)
            .sum::<usize>(),
        summary
            .languages
            .iter()
            .map(|language| language.terminals)
            .sum::<usize>(),
        summary.mismatches.len(),
        started.elapsed().as_millis(),
    );
    if let Some(first) = summary.mismatches.first().copied() {
        return Err(CorpusAuditError::Mismatches {
            count: summary.mismatches.len(),
            first: Some(first),
        });
    }
    Ok(())
}

struct LanguageResult {
    output: usize,
    unavailable: usize,
    terminals: usize,
    mismatches: Vec<CorpusMismatch>,
}

fn unavailable_result(
    batch: &BatchSource,
    cause: AuthorityUnavailableCause,
) -> LanguageResult {
    let mismatches = batch
        .cases
        .iter()
        .map(|case| CorpusMismatch::Availability {
            key: case_key(case.package),
            expected: CaseAvailability::Output,
            observed: cause,
        })
        .collect();
    LanguageResult {
        output: 0,
        unavailable: CASES_PER_BATCH,
        terminals: 0,
        mismatches,
    }
}

fn terminal_result(
    batch: &BatchSource,
    source: SourceIdentity,
    terminal: CompileTerminalKind,
) -> LanguageResult {
    let mismatches = batch
        .cases
        .iter()
        .map(|case| CorpusMismatch::CompileTerminal {
            key: case_key(case.package),
            source,
            terminal,
        })
        .collect();
    LanguageResult {
        output: 0,
        unavailable: 0,
        terminals: CASES_PER_BATCH,
        mismatches,
    }
}

fn run_language(
    language: CorpusLanguage,
    hosts: &HostTools,
    resolved: &ResolvedTools<'_>,
    authorities: &AuthorityFactory,
) -> Result<LanguageResult, CorpusAuditError> {
    let batch = BatchSource::build(language).map_err(CorpusAuditError::GroupedSource)?;
    let first_package = batch.case(0).package;
    let key = case_key(first_package);
    let expected_source = source_identity(batch.source());
    let (profile, toolchain) = match native_slot(language, resolved) {
        Ok(value) => value,
        Err(cause) => return Ok(unavailable_result(&batch, AuthorityUnavailableCause::Native(cause))),
    };
    let scope = declaration_scope(first_package)?;
    let mut diagnostic = [0_u8; DIAGNOSTIC_BYTES];
    let work = NativeWork::create().map_err(|cause| CorpusAuditError::NativeWork { key, cause })?;
    let cancelled = AtomicBool::new(false);
    let mut fragment_output = vec![0xa5_u8; BATCH_FRAGMENT_LIMIT];
    let deadline = Instant::now() + BATCH_DEADLINE;
    let authority_deadline = Instant::now() + BATCH_AUTHORITY_DEADLINE;

    match language {
        CorpusLanguage::Rust => {
            let Some(host) = hosts.rust.host() else {
                return Ok(unavailable_result(
                    &batch,
                    AuthorityUnavailableCause::Native(hosts.rust.cause),
                ));
            };
            let fixture = match rust_fixture_for_source(0, batch.source(), host) {
                Ok(fixture) => fixture,
                Err(error) if rust_error_is_unavailable(&error) => {
                    return Ok(unavailable_result(
                        &batch,
                        AuthorityUnavailableCause::RustAuthority,
                    ));
                }
                Err(error) => return Err(CorpusAuditError::AuthoritySetup { key, cause: error }),
            };
            let compiled = compile_with_authority_until(
                profile,
                batch.source(),
                scope,
                toolchain,
                SemanticAuthorityInput::Rust {
                    project: &fixture.project,
                    maximum_source_bytes: compiler_languages_rust::SourceByteLimit(
                        u32::try_from(batch.source().len()).unwrap_or(u32::MAX),
                    ),
                    features: fixture.features,
                },
                &cancelled,
                deadline.min(authority_deadline),
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_language(
                batch,
                expected_source,
                profile,
                toolchain,
                compiled,
                work,
            )
        }
        CorpusLanguage::Go => {
            let fixture = match go_fixture_for_source(0, batch.source()) {
                Ok(fixture) => fixture,
                Err(AuthorityBuildError::Go(error)) if go_error_is_unavailable(&error) => {
                    return Ok(unavailable_result(&batch, AuthorityUnavailableCause::GoOracle));
                }
                Err(error) => return Err(CorpusAuditError::AuthoritySetup { key, cause: error }),
            };
            let compiled = compile_with_authority_until(
                profile,
                batch.source(),
                scope,
                toolchain,
                SemanticAuthorityInput::Go { image: &fixture.image },
                &cancelled,
                deadline.min(authority_deadline),
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_language(
                batch,
                expected_source,
                profile,
                toolchain,
                compiled,
                work,
            )
        }
        CorpusLanguage::Java => {
            let ProviderSlot::Ready(provider) = &authorities.java else {
                let ProviderSlot::Unavailable(cause) = &authorities.java else { unreachable!() };
                return Ok(unavailable_result(&batch, *cause));
            };
            let image = provider
                .image(batch.source())
                .map_err(|cause| CorpusAuditError::AuthoritySetup { key, cause })?;
            let compiled = compile_with_authority_until(
                profile,
                batch.source(),
                scope,
                toolchain,
                SemanticAuthorityInput::Java { image: &image },
                &cancelled,
                deadline.min(authority_deadline),
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_language(
                batch,
                expected_source,
                profile,
                toolchain,
                compiled,
                work,
            )
        }
        CorpusLanguage::CSharp => {
            let ProviderSlot::Ready(provider) = &authorities.csharp else {
                let ProviderSlot::Unavailable(cause) = &authorities.csharp else { unreachable!() };
                return Ok(unavailable_result(&batch, *cause));
            };
            let image = provider
                .image_for_source(0, batch.source())
                .map_err(|cause| CorpusAuditError::AuthoritySetup { key, cause })?;
            let compiled = compile_with_authority_until(
                profile,
                batch.source(),
                scope,
                toolchain,
                SemanticAuthorityInput::CSharp { image: &image },
                &cancelled,
                deadline.min(authority_deadline),
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_language(
                batch,
                expected_source,
                profile,
                toolchain,
                compiled,
                work,
            )
        }
        CorpusLanguage::TypeScript => {
            let LanguageProfile::TypeScript(ts_profile) = profile else {
                return Ok(unavailable_result(
                    &batch,
                    AuthorityUnavailableCause::TypeScriptChecker,
                ));
            };
            let checker = compiler_languages_typescript::Checker {
                timeout: BATCH_AUTHORITY_DEADLINE,
                ..compiler_languages_typescript::Checker::default()
            };
            let report = match checker.run(ts_profile, batch.source()) {
                Ok(report) => report,
                Err(_) => {
                    return Ok(unavailable_result(
                        &batch,
                        AuthorityUnavailableCause::TypeScriptChecker,
                    ));
                }
            };
            let compiled = compile_with_authority_until(
                LanguageProfile::TypeScript(ts_profile),
                batch.source(),
                scope,
                toolchain,
                SemanticAuthorityInput::TypeScript { report: &report },
                &cancelled,
                deadline.min(authority_deadline),
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_language(
                batch,
                expected_source,
                LanguageProfile::TypeScript(ts_profile),
                toolchain,
                compiled,
                work,
            )
        }
        CorpusLanguage::Python => {
            let LanguageProfile::Python(py_profile) = profile else {
                return Ok(unavailable_result(
                    &batch,
                    AuthorityUnavailableCause::PythonChecker,
                ));
            };
            let facts = match compiler_languages_python::extract(batch.source(), py_profile) {
                Ok(facts) => facts,
                Err(_) => {
                    return Ok(unavailable_result(
                        &batch,
                        AuthorityUnavailableCause::PythonChecker,
                    ));
                }
            };
            let checker = compiler_languages_python::Pyrefly::from_env()
                .with_timeout(BATCH_AUTHORITY_DEADLINE);
            if !checker.is_available() {
                return Ok(unavailable_result(
                    &batch,
                    AuthorityUnavailableCause::PythonChecker,
                ));
            }
            let report = match checker.analyze(batch.source(), py_profile, &facts) {
                Ok(report) => report,
                Err(_) => {
                    return Ok(unavailable_result(
                        &batch,
                        AuthorityUnavailableCause::PythonChecker,
                    ));
                }
            };
            let compiled = compile_with_authority_until(
                LanguageProfile::Python(py_profile),
                batch.source(),
                scope,
                toolchain,
                SemanticAuthorityInput::Python { report: &report },
                &cancelled,
                deadline.min(authority_deadline),
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_language(
                batch,
                expected_source,
                LanguageProfile::Python(py_profile),
                toolchain,
                compiled,
                work,
            )
        }
        CorpusLanguage::Clang => {
            let compiled = compile_with_authority_until(
                profile,
                batch.source(),
                scope,
                toolchain,
                SemanticAuthorityInput::None,
                &cancelled,
                deadline.min(authority_deadline),
                &mut diagnostic,
                work.path(),
                &mut fragment_output,
            );
            finish_language(
                batch,
                expected_source,
                profile,
                toolchain,
                compiled,
                work,
            )
        }
    }
}

fn finish_language<'diagnostic, 'output>(
    batch: BatchSource,
    expected_source: SourceIdentity,
    profile: LanguageProfile,
    toolchain: ResolvedToolchain<'_>,
    compiled: Result<CompiledSemantic<'output>, CompileFailure<'diagnostic>>,
    work: NativeWork,
) -> Result<LanguageResult, CorpusAuditError> {
    let key = case_key(batch.case(0).package);
    let compiled = match compiled {
        Ok(compiled) => compiled,
        Err(failure) => {
            let terminal = terminal_kind(&failure);
            work.assert_empty().map_err(|cause| CorpusAuditError::NativeWork { key, cause })?;
            return Ok(terminal_result(&batch, expected_source, terminal));
        }
    };
    work.assert_empty().map_err(|cause| CorpusAuditError::NativeWork { key, cause })?;
    let mut publisher = PassPublisher::new(Pass::Original)?;
    let result = inspect_batch(
        &batch,
        expected_source,
        profile,
        toolchain,
        &compiled,
        &mut publisher,
    )?;
    publisher.finish()?;
    Ok(result)
}

fn grouped_digest_range(range: BatchRange) -> Digest {
    observation::digest_typed(&(range.start, range.end))
}

fn grouped_digest_entity(entity: Option<EntityObservation>) -> Digest {
    observation::digest_entity_observation(entity)
}

fn grouped_digest_count(value: u32) -> Digest {
    observation::digest_u64(u64::from(value))
}

fn grouped_digest_fact(value: FactAvailability) -> Digest {
    observation::digest_fact_availability(value)
}

fn grouped_digest_render(value: RenderVerdict) -> Digest {
    observation::digest_render(value)
}

fn grouped_digest_expected_type(value: ExpectedType) -> Digest {
    match value {
        ExpectedType::Primitive(primitive) => {
            observation::digest_u64(u64::from(u32::from(primitive)))
        }
        ExpectedType::Builtin(builtin) => observation::digest_typed(&builtin),
        ExpectedType::Callable => observation::digest_u64(10),
        ExpectedType::Nominal => observation::digest_u64(11),
        ExpectedType::Structural => observation::digest_u64(12),
        ExpectedType::Generic => observation::digest_u64(13),
        ExpectedType::Reference => observation::digest_u64(14),
    }
}

fn grouped_digest_parent(value: multilingual_corpus::ParentExpectation) -> Digest {
    let code = match value {
        multilingual_corpus::ParentExpectation::Root => 0,
        multilingual_corpus::ParentExpectation::Nested => 1,
        multilingual_corpus::ParentExpectation::Unavailable => 2,
    };
    observation::digest_u64(code)
}

fn grouped_digest_render_availability(value: RenderAvailability) -> Digest {
    let code = match value {
        RenderAvailability::NeutralRequired => 0,
        RenderAvailability::DialectUnsupported => 1,
        RenderAvailability::Unavailable => 2,
    };
    observation::digest_u64(code)
}

fn grouped_digest_member(value: Option<BatchRange>) -> Digest {
    match value {
        Some(value) => grouped_digest_range(value),
        None => observation::digest_u64(u64::MAX),
    }
}

fn inspect_batch(
    batch: &BatchSource,
    expected_source: SourceIdentity,
    profile: LanguageProfile,
    toolchain: ResolvedToolchain<'_>,
    compiled: &CompiledSemantic<'_>,
    publisher: &mut PassPublisher,
) -> Result<LanguageResult, CorpusAuditError> {
    let mut mismatches = Vec::new();
    let expected_recipe = expected_recipe(
        profile,
        native_tool(batch.language),
        expected_source,
        toolchain,
    );
    if compiled.artifact.source != expected_source {
        push_batch_mismatch(
            batch,
            GroupedField::Reopened,
            observation::digest_source(expected_source),
            observation::digest_source(compiled.artifact.source),
            &mut mismatches,
        );
    }
    if compiled.artifact.recipe != expected_recipe {
        push_batch_mismatch(
            batch,
            GroupedField::Reopened,
            observation::digest_recipe(expected_recipe),
            observation::digest_recipe(compiled.artifact.recipe),
            &mut mismatches,
        );
    }
    let owned_semantic = observe_owned_semantic(&compiled.ir, None);
    let owned_provenance = owned_semantic.image.map(|image| image.provenance);
    if !matches!(
        owned_provenance,
        Some(ImageProvenance::Captured { source, recipe, .. })
            if source == expected_source && recipe == expected_recipe
    ) {
        push_batch_mismatch(
            batch,
            GroupedField::Reopened,
            observation::digest_source(expected_source),
            observation::digest_image_provenance(owned_provenance.unwrap_or(ImageProvenance::Unavailable)),
            &mut mismatches,
        );
    }
    let owned_identity = owned_semantic.identity;
    let owned_census = owned_semantic.census;
    let owned_spans = owned_semantic.source_spans;

    publisher.publish_with_reader(compiled, |fragment, image| {
        let reopened_semantic = observe_reopened_semantic(image, None);
        if owned_semantic != reopened_semantic {
            push_batch_mismatch(
                batch,
                GroupedField::Reopened,
                observation::digest_semantic(owned_semantic),
                observation::digest_semantic(reopened_semantic),
                &mut mismatches,
            );
        }
        if owned_identity != reopened_semantic.identity {
            push_batch_mismatch(
                batch,
                GroupedField::Reopened,
                observation::digest_typed(&owned_identity),
                observation::digest_typed(&reopened_semantic.identity),
                &mut mismatches,
            );
        }
        if owned_census.is_none() || reopened_semantic.census.is_none() {
            push_batch_mismatch(
                batch,
                GroupedField::Reopened,
                observation::digest_semantic_census(owned_census),
                observation::digest_semantic_census(reopened_semantic.census),
                &mut mismatches,
            );
        } else if owned_census != reopened_semantic.census {
            push_batch_mismatch(
                batch,
                GroupedField::Reopened,
                observation::digest_semantic_census(owned_census),
                observation::digest_semantic_census(reopened_semantic.census),
                &mut mismatches,
            );
        }
        if owned_spans != reopened_semantic.source_spans {
            push_batch_mismatch(
                batch,
                GroupedField::SourceSpan,
                owned_spans,
                reopened_semantic.source_spans,
                &mut mismatches,
            );
        }
        let compact_census = fragment.view.discover().census();
        if compact_census.entities < CASES_PER_BATCH as u32 {
            push_batch_mismatch(
                batch,
                GroupedField::Declaration,
                grouped_digest_count(CASES_PER_BATCH as u32),
                grouped_digest_count(compact_census.entities),
                &mut mismatches,
            );
        }
        let compact_fragment = observation::digest_bytes(fragment.view.as_ref());
        if compact_fragment == [0_u8; 32] {
            push_batch_mismatch(
                batch,
                GroupedField::Reopened,
                observation::digest_bytes(compiled.artifact.fragment.as_ref()),
                compact_fragment,
                &mut mismatches,
            );
        }

        for case in batch.cases.iter().copied() {
            let expected = case.package.expected_facts();
            let expected_name = batch
                .source()
                .get(case.name.start as usize..case.name.end as usize)
                .unwrap_or_default();
            let expected_kind = observation::item_kind(expected.primary_kind);
            let (owned_matches, owned, owned_links, owned_occurrences) = observe_entity_at_source(
                &compiled.ir,
                expected_kind,
                expected_name,
                case.name.start,
                case.name.end,
                batch.file(),
            );
            let (reopened_matches, reopened, reopened_links, reopened_occurrences) =
                observe_entity_at_source(
                    image,
                    expected_kind,
                    expected_name,
                    case.name.start,
                    case.name.end,
                    batch.file(),
                );
            let key = case_key(case.package);
            if owned_matches != 1 {
                mismatches.push(CorpusMismatch::Grouped {
                    key,
                    field: GroupedField::Declaration,
                    expected: observation::digest_u64(1),
                    observed: observation::digest_u64(u64::from(owned_matches)),
                });
            }
            if reopened_matches != 1 {
                mismatches.push(CorpusMismatch::Grouped {
                    key,
                    field: GroupedField::Reopened,
                    expected: observation::digest_u64(1),
                    observed: observation::digest_u64(u64::from(reopened_matches)),
                });
            }
            check_entity(
                batch,
                case,
                expected,
                expected_name,
                owned,
                owned_links,
                owned_occurrences,
                false,
                &mut mismatches,
            );
            check_entity(
                batch,
                case,
                expected,
                expected_name,
                reopened,
                reopened_links,
                reopened_occurrences,
                true,
                &mut mismatches,
            );
            if let (Some(owned), Some(reopened)) = (owned, reopened) {
                if owned != reopened {
                    mismatches.push(CorpusMismatch::Grouped {
                        key,
                        field: GroupedField::Reopened,
                        expected: grouped_digest_entity(Some(owned)),
                        observed: grouped_digest_entity(Some(reopened)),
                    });
                }
                let owned_render = render_neutral(&compiled.ir, Some(owned));
                let reopened_render = observation::render_neutral_reader(image, Some(reopened.id));
                if !matches!(expected.neutral_render, RenderAvailability::NeutralRequired)
                    || !matches!(owned_render, RenderVerdict::Rendered(_))
                    || !matches!(reopened_render, RenderVerdict::Rendered(_))
                {
                    mismatches.push(CorpusMismatch::Grouped {
                        key,
                        field: GroupedField::Render,
                        expected: grouped_digest_render(owned_render),
                        observed: grouped_digest_render(reopened_render),
                    });
                }
                // Dialect renderers are intentionally not admitted by this
                // seam.  Keep the exact unsupported observation red rather
                // than treating the absence as a successful dialect check.
                mismatches.push(CorpusMismatch::Grouped {
                    key,
                    field: GroupedField::Render,
                    expected: grouped_digest_render_availability(expected.dialect_render),
                    observed: grouped_digest_render(RenderVerdict::Unsupported),
                });
            }
        }
        Ok(())
    })?;
    Ok(LanguageResult {
        output: CASES_PER_BATCH,
        unavailable: 0,
        terminals: 0,
        mismatches,
    })
}

fn push_batch_mismatch(
    batch: &BatchSource,
    field: GroupedField,
    expected: Digest,
    observed: Digest,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    for case in batch.cases.iter().copied() {
        mismatches.push(CorpusMismatch::Grouped {
            key: case_key(case.package),
            field,
            expected,
            observed,
        });
    }
}

fn check_entity(
    _batch: &BatchSource,
    case: BatchCase,
    expected: ExpectedFacts,
    expected_name: &[u8],
    observed: Option<EntityObservation>,
    links: u32,
    occurrences: u32,
    reopened: bool,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    let key = case_key(case.package);
    let Some(entity) = observed else {
        return;
    };
    if entity.kind != observation::item_kind(expected.primary_kind)
        || observation::digest_bytes(expected_name) != entity.name
    {
        mismatches.push(CorpusMismatch::Grouped {
            key,
            field: GroupedField::Declaration,
            expected: observation::digest_typed(&(
                observation::item_kind(expected.primary_kind),
                expected_name,
            )),
            observed: grouped_digest_entity(Some(entity)),
        });
    }
    if !comparison::type_matches(expected.primary_type, entity.type_shape) {
        mismatches.push(CorpusMismatch::Grouped {
            key,
            field: GroupedField::Type,
            expected: grouped_digest_expected_type(expected.primary_type),
            observed: observation::digest_type_shape(entity.type_shape),
        });
    }
    let span_ok = entity.source.is_some_and(|span| {
        let observed_range = BatchRange {
            start: span.start(),
            end: span.end(),
        };
        observed_range.contains(case.name) && case.source.contains(observed_range)
    });
    if !span_ok {
        mismatches.push(CorpusMismatch::Grouped {
            key,
            field: if reopened {
                GroupedField::Reopened
            } else {
                GroupedField::SourceSpan
            },
            expected: grouped_digest_range(case.source),
            observed: observation::digest_typed(&entity.source),
        });
    }
    let parent_ok = match expected.parent {
        multilingual_corpus::ParentExpectation::Root => entity.parent.is_none(),
        multilingual_corpus::ParentExpectation::Nested => entity.parent.is_some(),
        multilingual_corpus::ParentExpectation::Unavailable => false,
    };
    if !parent_ok {
        mismatches.push(CorpusMismatch::Grouped {
            key,
            field: GroupedField::Parent,
            expected: grouped_digest_parent(expected.parent),
            observed: observation::digest_typed(&entity.parent),
        });
    }
    if expected.nested_member != (entity.members > 0) {
        mismatches.push(CorpusMismatch::Grouped {
            key,
            field: GroupedField::Declaration,
            expected: observation::digest_typed(&expected.nested_member),
            observed: grouped_digest_count(entity.members),
        });
    }
    let Some(authority) = entity.authority else {
        mismatches.push(CorpusMismatch::Grouped {
            key,
            field: GroupedField::Reopened,
            expected: grouped_digest_fact(FactAvailability::Captured),
            observed: observation::digest_u64(0),
        });
        return;
    };
    check_fact_plane(
        key,
        GroupedField::SourceSpan,
        expected.provenance,
        authority.source,
        mismatches,
    );
    check_fact_plane(
        key,
        GroupedField::Documentation,
        expected.documentation,
        authority.documentation,
        mismatches,
    );
    check_fact_plane(
        key,
        GroupedField::Attributes,
        expected.attributes,
        authority.attributes,
        mismatches,
    );
    check_fact_plane(
        key,
        GroupedField::Extension,
        expected.extension,
        authority.language_extension,
        mismatches,
    );
    if expected.relation == RelationExpectation::OccurrenceRequired
        && occurrences == 0
    {
        mismatches.push(CorpusMismatch::Grouped {
            key,
            field: GroupedField::Relations,
            expected: observation::digest_u64(1),
            observed: grouped_digest_count(occurrences),
        });
    }
    if expected.relation == RelationExpectation::OverloadRequired
        && links.saturating_add(occurrences) == 0
    {
        mismatches.push(CorpusMismatch::Grouped {
            key,
            field: GroupedField::Relations,
            expected: observation::digest_u64(1),
            observed: grouped_digest_count(links.saturating_add(occurrences)),
        });
    }
    if case.member.is_some() && entity.first_member.is_none() {
        mismatches.push(CorpusMismatch::Grouped {
            key,
            field: GroupedField::Declaration,
            expected: grouped_digest_member(case.member),
            observed: observation::digest_u64(0),
        });
    }
}

fn check_fact_plane(
    key: CaseKey,
    field: GroupedField,
    expected: PlaneAvailability,
    observed: FactAvailability,
    mismatches: &mut Vec<CorpusMismatch>,
) {
    let expected = match expected {
        PlaneAvailability::Required => FactAvailability::Captured,
        PlaneAvailability::Unavailable => FactAvailability::Unavailable,
        PlaneAvailability::ObserverUnavailable | PlaneAvailability::Unsupported => return,
    };
    if expected != observed {
        mismatches.push(CorpusMismatch::Grouped {
            key,
            field,
            expected: grouped_digest_fact(expected),
            observed: grouped_digest_fact(observed),
        });
    }
}
