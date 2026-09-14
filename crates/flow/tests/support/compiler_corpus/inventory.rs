//! Offline inventory of the real language/package corpus.
//!
//! The package names and versions are copied from the committed driver-lane
//! tables. Resolution is deliberately local-only: a lane may opt into an
//! explicit root through its documented environment variable, but this
//! module never downloads, searches unrelated directories, or turns a
//! missing source tree into a generated fixture.

use super::*;

#[path = "inventory_resolve.rs"]
mod inventory_resolve;
#[path = "inventory_tables.rs"]
mod inventory_tables;
use inventory_resolve::resolve_package_source;
use inventory_tables::{
    CLANG_PROJECTS, CSHARP_PACKAGES, GO_PACKAGES, JAVA_PACKAGES, PYTHON_PACKAGES, RUST_PACKAGES,
    TYPESCRIPT_PACKAGES,
};

/// The frozen driver tables contain twenty rows for every lane except Go,
/// whose table also carries the `rsc.io/quote` source package.  Keep that
/// asymmetry explicit instead of silently trimming a committed row to fit a
/// convenient rectangular matrix.
pub(super) const REAL_CASES_PER_LANE: usize = 20;
pub(super) const GO_CASES_PER_LANE: usize = 21;
pub(super) const REAL_PACKAGE_COUNT: usize =
    REAL_CASES_PER_LANE * (CorpusLanguage::ALL.len() - 1) + GO_CASES_PER_LANE;
/// The requested package audit is a 200+ row audit.  The committed primary
/// lane tables currently contain only 141 distinct coordinates; this is kept
/// as an explicit capacity terminal rather than padded with generated cases,
/// dependency metadata, or invented package names.
pub(super) const MIN_REAL_PACKAGE_COUNT: usize = 200;
pub(super) const REAL_SOURCE_BYTE_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum PackageCoordinate {
    Purl(&'static str),
    Project(&'static str),
}

impl PackageCoordinate {
    pub(super) const fn raw(self) -> &'static str {
        match self {
            Self::Purl(value) | Self::Project(value) => value,
        }
    }

    pub(super) fn lineage(self) -> (&'static str, &'static str) {
        match self {
            Self::Purl(value) => {
                let Some((ecosystem, package)) = value.split_once(':') else {
                    return ("unknown", value);
                };
                let package = package.rsplit_once('@').map_or(package, |(name, _)| name);
                (ecosystem, package)
            }
            Self::Project(value) => ("clang", value),
        }
    }
}

/// The committed source table which authorizes a coordinate.  Keeping this
/// as a closed enum makes source provenance part of the typed package key;
/// the display path is only derived at the reporting boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum CorpusTable {
    RustDriver,
    TypeScriptDriver,
    PythonDriver,
    GoDriver,
    JavaDriver,
    CSharpDriver,
    ClangEvidence,
}

impl CorpusTable {
    pub(super) const fn path(self) -> &'static str {
        match self {
            Self::RustDriver => "compiler/driver/tests/rust_corpus.rs",
            Self::TypeScriptDriver => "compiler/driver/tests/typescript_corpus.rs",
            Self::PythonDriver => "compiler/driver/tests/python_packages.rs",
            Self::GoDriver => "compiler/driver/tests/go_corpus.rs",
            Self::JavaDriver => "compiler/driver/tests/java_corpus/mod.rs",
            Self::CSharpDriver => "compiler/driver/tests/csharp_corpus.rs",
            Self::ClangEvidence => ".codex/evidence/capabilities/clang-c-lifecycle/corpus.md",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceExtension {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    CSharp,
    CFamily,
}

impl SourceExtension {
    fn accepts(self, path: &Path) -> bool {
        let Some(extension) = path.extension().and_then(|extension| extension.to_str()) else {
            return false;
        };
        match self {
            Self::Rust => extension == "rs",
            Self::TypeScript => {
                matches!(extension, "ts" | "tsx")
                    || path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.ends_with(".d.ts"))
            }
            Self::Python => extension == "py",
            Self::Go => extension == "go",
            Self::Java => extension == "java",
            Self::CSharp => extension == "cs",
            Self::CFamily => matches!(extension, "c" | "h" | "cc" | "cpp" | "cxx" | "hpp"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PackageLayout {
    CargoRegistry,
    NodeModules,
    PythonDistribution,
    GoModule,
    MavenSource,
    NugetSource,
    ClangProject,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceRoot {
    Environment {
        variable: &'static str,
        relative: &'static str,
        layout: PackageLayout,
        extension: SourceExtension,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct CorpusLane {
    pub(super) language: CorpusLanguage,
    pub(super) profile: LanguageProfile,
    pub(super) table: CorpusTable,
    coordinates: &'static [PackageCoordinate],
    root: SourceRoot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RealPackageCase {
    pub(super) ordinal: usize,
    pub(super) case_id: CaseId,
    pub(super) language: CorpusLanguage,
    pub(super) profile: LanguageProfile,
    pub(super) table: CorpusTable,
    pub(super) coordinate: PackageCoordinate,
    root: SourceRoot,
}

impl RealPackageCase {
    pub(super) const fn source_slot(self) -> CorpusLanguage {
        self.language
    }
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum InventoryInvariant {
    LaneCount {
        language: CorpusLanguage,
        observed: usize,
        expected: usize,
    },
    TotalCount {
        observed: usize,
        expected: usize,
    },
    CorpusCapacity {
        observed: usize,
        required: usize,
    },
    CaseIdentity {
        ordinal: usize,
        observed: CaseId,
    },
    DuplicateCaseId {
        observed: CaseId,
    },
    MissingCaseId,
    DuplicateCoordinate {
        language: CorpusLanguage,
        coordinate: PackageCoordinate,
    },
    DuplicateCoordinateGlobal {
        coordinate: PackageCoordinate,
    },
    ProfileMismatch {
        language: CorpusLanguage,
        profile: LanguageProfile,
    },
    TableMismatch {
        language: CorpusLanguage,
        table: CorpusTable,
    },
    FixtureCount {
        observed: usize,
        expected: usize,
    },
    FixtureProfileMismatch {
        language: CorpusLanguage,
        profile: LanguageProfile,
    },
    EmptyFixture {
        language: CorpusLanguage,
    },
    FixtureAvailabilityMismatch {
        language: CorpusLanguage,
    },
    SourceBindingMismatch {
        language: CorpusLanguage,
    },
    SourceCauseMismatch {
        language: CorpusLanguage,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LocalFixture {
    pub(super) language: CorpusLanguage,
    pub(super) profile: LanguageProfile,
    pub(super) path: &'static str,
    pub(super) bytes: Option<&'static [u8]>,
    pub(super) absence: Option<SourceUnavailableKind>,
}

pub(super) enum LocalFixtureInput {
    Source {
        fixture: LocalFixture,
        identity: SourceIdentity,
    },
    Unavailable {
        fixture: LocalFixture,
        cause: SourceUnavailableCause,
    },
}

/// A source selected beneath one explicit package root. The digest is always
/// computed from the bytes actually read, so later authority/output joins can
/// retain `{coordinate, source_root, path, digest, profile}` without trusting
/// a label. The authority tool and version identity are added by the compile
/// adapter after this source binding is admitted; they are never inferred from
/// the package coordinate.
#[derive(Debug)]
pub(super) struct ResolvedPackageSource {
    pub(super) coordinate: PackageCoordinate,
    pub(super) language: CorpusLanguage,
    pub(super) profile: LanguageProfile,
    pub(super) table: CorpusTable,
    pub(super) source_root: PathBuf,
    pub(super) path: PathBuf,
    pub(super) bytes: Vec<u8>,
    pub(super) identity: SourceIdentity,
    /// Source resolution cannot prove a compiler version. This remains an
    /// explicit pending authority binding until the compile adapter supplies
    /// the caller-resolved toolchain identity.
    pub(super) authority: AuthorityProvenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AuthorityProvenance {
    Unresolved {
        tool: NativeTool,
    },
    Bound {
        tool: NativeTool,
        toolchain: ContentId<ToolchainDomain>,
    },
    Unavailable(AuthorityUnavailableCause),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AuthorityBindingFault {
    ToolMismatch {
        expected: NativeTool,
        observed: NativeTool,
    },
}

impl ResolvedPackageSource {
    pub(super) fn bind_toolchain(
        &mut self,
        expected_tool: NativeTool,
        toolchain: ResolvedToolchain<'_>,
    ) -> Result<(), AuthorityBindingFault> {
        if toolchain.tool != expected_tool {
            return Err(AuthorityBindingFault::ToolMismatch {
                expected: expected_tool,
                observed: toolchain.tool,
            });
        }
        self.authority = AuthorityProvenance::Bound {
            tool: toolchain.tool,
            toolchain: toolchain.identity,
        };
        Ok(())
    }
}

/// Source-first input for the future real-package semantic pass. A missing
/// source is retained as a closed terminal, never converted into a generated
/// `CorpusPackage` or an output-parity success.
pub(super) enum RealPackageInput {
    Source {
        case: RealPackageCase,
        source: ResolvedPackageSource,
    },
    Unavailable {
        case: RealPackageCase,
        cause: SourceUnavailableCause,
    },
}

const LANES: [CorpusLane; 7] = [
    CorpusLane {
        language: CorpusLanguage::Rust,
        profile: LanguageProfile::Rust(RustEdition::Rust2024),
        table: CorpusTable::RustDriver,
        coordinates: &RUST_PACKAGES,
        root: SourceRoot::Environment {
            variable: "NUDOX_RUST_CORPUS_DIR",
            relative: "",
            layout: PackageLayout::CargoRegistry,
            extension: SourceExtension::Rust,
        },
    },
    CorpusLane {
        language: CorpusLanguage::TypeScript,
        profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        table: CorpusTable::TypeScriptDriver,
        coordinates: &TYPESCRIPT_PACKAGES,
        root: SourceRoot::Environment {
            variable: "NUDOX_TYPESCRIPT_CORPUS_DIR",
            relative: "",
            layout: PackageLayout::NodeModules,
            extension: SourceExtension::TypeScript,
        },
    },
    CorpusLane {
        language: CorpusLanguage::Python,
        profile: LanguageProfile::Python(PythonVersion::Python314),
        table: CorpusTable::PythonDriver,
        coordinates: &PYTHON_PACKAGES,
        root: SourceRoot::Environment {
            variable: "NUDOX_PYTHON_CORPUS_DIR",
            relative: "",
            layout: PackageLayout::PythonDistribution,
            extension: SourceExtension::Python,
        },
    },
    CorpusLane {
        language: CorpusLanguage::Go,
        profile: LanguageProfile::Go(GoVersion::Go125),
        table: CorpusTable::GoDriver,
        coordinates: &GO_PACKAGES,
        root: SourceRoot::Environment {
            variable: "NUDOX_GO_CORPUS_DIR",
            relative: "",
            layout: PackageLayout::GoModule,
            extension: SourceExtension::Go,
        },
    },
    CorpusLane {
        language: CorpusLanguage::Java,
        profile: LanguageProfile::Java(JavaRelease::Java21),
        table: CorpusTable::JavaDriver,
        coordinates: &JAVA_PACKAGES,
        root: SourceRoot::Environment {
            variable: "NUDOX_JAVA_CORPUS_DIR",
            relative: "",
            layout: PackageLayout::MavenSource,
            extension: SourceExtension::Java,
        },
    },
    CorpusLane {
        language: CorpusLanguage::CSharp,
        profile: LanguageProfile::CSharp(CSharpVersion::CSharp14),
        table: CorpusTable::CSharpDriver,
        coordinates: &CSHARP_PACKAGES,
        root: SourceRoot::Environment {
            variable: "NUDOX_CSHARP_CORPUS_DIR",
            relative: "",
            layout: PackageLayout::NugetSource,
            extension: SourceExtension::CSharp,
        },
    },
    CorpusLane {
        language: CorpusLanguage::Clang,
        profile: LanguageProfile::Cxx(CxxStandard::Cxx23),
        table: CorpusTable::ClangEvidence,
        coordinates: &CLANG_PROJECTS,
        root: SourceRoot::Environment {
            variable: "NUDOX_CLANG_CORPUS_DIR",
            relative: "",
            layout: PackageLayout::ClangProject,
            extension: SourceExtension::CFamily,
        },
    },
];

/// Existing repository fixtures are source-bearing evidence, not substitutes
/// for a missing real package. Python and Clang intentionally have no tracked
/// source fixture in this worktree and therefore carry `None`.
pub(super) const LOCAL_FIXTURES: [LocalFixture; 7] = [
    LocalFixture {
        language: CorpusLanguage::Rust,
        profile: LanguageProfile::Rust(RustEdition::Rust2024),
        path: "crates/compiler-driver/tests/native_compile/matrix.rs",
        bytes: Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../compiler-driver/tests/native_compile/matrix.rs"
        ))),
        absence: None,
    },
    LocalFixture {
        language: CorpusLanguage::TypeScript,
        profile: LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
        path: "frontends/typescript/tests/fixtures/source.ts",
        bytes: Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../frontends/typescript/tests/fixtures/source.ts"
        ))),
        absence: None,
    },
    LocalFixture {
        language: CorpusLanguage::Python,
        profile: LanguageProfile::Python(PythonVersion::Python314),
        path: "frontends/python/tests",
        bytes: None,
        absence: Some(SourceUnavailableKind::RepositoryFixtureMissing),
    },
    LocalFixture {
        language: CorpusLanguage::Go,
        profile: LanguageProfile::Go(GoVersion::Go125),
        path: "frontends/go/tests/fixtures/module/demo.go",
        bytes: Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../frontends/go/tests/fixtures/module/demo.go"
        ))),
        absence: None,
    },
    LocalFixture {
        language: CorpusLanguage::Java,
        profile: LanguageProfile::Java(JavaRelease::Java21),
        path: "frontends/java/tests/fixtures/authority/src/demo/Cafe.java",
        bytes: Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../frontends/java/tests/fixtures/authority/src/demo/Cafe.java"
        ))),
        absence: None,
    },
    LocalFixture {
        language: CorpusLanguage::CSharp,
        profile: LanguageProfile::CSharp(CSharpVersion::CSharp14),
        path: "frontends/csharp/tests/fixtures/producer/fidelity.cs",
        bytes: Some(include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../frontends/csharp/tests/fixtures/producer/fidelity.cs"
        ))),
        absence: None,
    },
    LocalFixture {
        language: CorpusLanguage::Clang,
        profile: LanguageProfile::Cxx(CxxStandard::Cxx23),
        path: "frontends/clang/tests",
        bytes: None,
        absence: Some(SourceUnavailableKind::RepositoryFixtureMissing),
    },
];

pub(super) const fn real_package_inventory() -> &'static [CorpusLane; 7] {
    &LANES
}

pub(super) fn local_fixture_inputs() -> impl Iterator<Item = LocalFixtureInput> {
    LOCAL_FIXTURES
        .into_iter()
        .map(|fixture| match fixture.bytes {
            Some(bytes) => LocalFixtureInput::Source {
                fixture,
                identity: source_identity(bytes),
            },
            None => LocalFixtureInput::Unavailable {
                fixture,
                cause: SourceUnavailableCause {
                    language: fixture.language,
                    kind: fixture
                        .absence
                        .unwrap_or(SourceUnavailableKind::RepositoryFixtureMissing),
                },
            },
        })
}

pub(super) fn real_package_cases() -> impl Iterator<Item = RealPackageCase> {
    LANES.iter().enumerate().flat_map(|(lane_index, lane)| {
        let offset = match lane_index {
            0 => 0,
            1 => REAL_CASES_PER_LANE,
            2 => REAL_CASES_PER_LANE * 2,
            3 => REAL_CASES_PER_LANE * 3,
            4 => REAL_CASES_PER_LANE * 3 + GO_CASES_PER_LANE,
            5 => REAL_CASES_PER_LANE * 4 + GO_CASES_PER_LANE,
            6 => REAL_CASES_PER_LANE * 5 + GO_CASES_PER_LANE,
            _ => REAL_PACKAGE_COUNT,
        };
        lane.coordinates
            .iter()
            .copied()
            .enumerate()
            .map(move |(index, coordinate)| RealPackageCase {
                ordinal: offset + index,
                case_id: CaseId((offset + index) as u16),
                language: lane.language,
                profile: lane.profile,
                table: lane.table,
                coordinate,
                root: lane.root,
            })
    })
}

pub(super) fn validate_real_inventory() -> Result<(), InventoryInvariant> {
    let mut total = 0_usize;
    let mut all_coordinates = Vec::new();
    for lane in real_package_inventory().iter().copied() {
        let observed = lane.coordinates.len();
        let expected = match lane.language {
            CorpusLanguage::Go => GO_CASES_PER_LANE,
            _ => REAL_CASES_PER_LANE,
        };
        if observed != expected {
            return Err(InventoryInvariant::LaneCount {
                language: lane.language,
                observed,
                expected,
            });
        }
        let expected_profile = match lane.language {
            CorpusLanguage::Rust => LanguageProfile::Rust(RustEdition::Rust2024),
            CorpusLanguage::TypeScript => LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            CorpusLanguage::Python => LanguageProfile::Python(PythonVersion::Python314),
            CorpusLanguage::Go => LanguageProfile::Go(GoVersion::Go125),
            CorpusLanguage::Java => LanguageProfile::Java(JavaRelease::Java21),
            CorpusLanguage::CSharp => LanguageProfile::CSharp(CSharpVersion::CSharp14),
            CorpusLanguage::Clang => LanguageProfile::Cxx(CxxStandard::Cxx23),
        };
        if lane.profile != expected_profile {
            return Err(InventoryInvariant::ProfileMismatch {
                language: lane.language,
                profile: lane.profile,
            });
        }
        if lane.table != expected_table(lane.language) {
            return Err(InventoryInvariant::TableMismatch {
                language: lane.language,
                table: lane.table,
            });
        }
        for (index, left) in lane.coordinates.iter().copied().enumerate() {
            if lane.coordinates[index + 1..]
                .iter()
                .copied()
                .any(|right| right == left)
            {
                return Err(InventoryInvariant::DuplicateCoordinate {
                    language: lane.language,
                    coordinate: left,
                });
            }
        }
        all_coordinates.extend(lane.coordinates.iter().copied());
        total += observed;
    }
    if total != REAL_PACKAGE_COUNT {
        return Err(InventoryInvariant::TotalCount {
            observed: total,
            expected: REAL_PACKAGE_COUNT,
        });
    }
    all_coordinates.sort_unstable();
    for pair in all_coordinates.windows(2) {
        if pair[0] == pair[1] {
            return Err(InventoryInvariant::DuplicateCoordinateGlobal {
                coordinate: pair[0],
            });
        }
    }
    let mut seen_case_ids = [false; REAL_PACKAGE_COUNT];
    for case in real_package_cases() {
        let raw = usize::from(case.case_id.raw());
        if raw >= REAL_PACKAGE_COUNT || case.ordinal != raw {
            return Err(InventoryInvariant::CaseIdentity {
                ordinal: case.ordinal,
                observed: case.case_id,
            });
        }
        if seen_case_ids[raw] {
            return Err(InventoryInvariant::DuplicateCaseId {
                observed: case.case_id,
            });
        }
        seen_case_ids[raw] = true;
        if case.table != expected_table(case.language) {
            return Err(InventoryInvariant::TableMismatch {
                language: case.language,
                table: case.table,
            });
        }
    }
    if seen_case_ids.iter().any(|seen| !seen) {
        return Err(InventoryInvariant::MissingCaseId);
    }
    if LOCAL_FIXTURES.len() != CorpusLanguage::ALL.len() {
        return Err(InventoryInvariant::FixtureCount {
            observed: LOCAL_FIXTURES.len(),
            expected: CorpusLanguage::ALL.len(),
        });
    }
    for fixture in LOCAL_FIXTURES {
        let lane = real_package_inventory()
            .iter()
            .find(|lane| lane.language == fixture.language)
            .ok_or(InventoryInvariant::FixtureProfileMismatch {
                language: fixture.language,
                profile: fixture.profile,
            })?;
        if lane.profile != fixture.profile {
            return Err(InventoryInvariant::FixtureProfileMismatch {
                language: fixture.language,
                profile: fixture.profile,
            });
        }
        if fixture.bytes.is_some_and(<[u8]>::is_empty) {
            return Err(InventoryInvariant::EmptyFixture {
                language: fixture.language,
            });
        }
        if fixture.bytes.is_some() == fixture.absence.is_some() {
            return Err(InventoryInvariant::FixtureAvailabilityMismatch {
                language: fixture.language,
            });
        }
    }
    Ok(())
}

/// Resolve every frozen row in deterministic table order. The iterator owns
/// one source buffer at a time and carries an unavailable package together
/// with its exact source-root cause, allowing the semantic pass to count
/// attempted/verified/unavailable rows without treating absence as parity.
pub(super) fn real_package_inputs() -> impl Iterator<Item = RealPackageInput> {
    real_package_cases().map(|case| match resolve_package_source(case) {
        Ok(source) => RealPackageInput::Source { case, source },
        Err(cause) => RealPackageInput::Unavailable { case, cause },
    })
}

/// Partition the source inventory without assigning any row to output
/// verification. `source_bound` means only that bytes were read and checked;
/// the semantic authority/image/IR/reopen/render pass must perform its own
/// exact comparison before a row can become verified.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RealInventorySummary {
    pub(super) attempted: [usize; CorpusLanguage::ALL.len()],
    pub(super) source_bound: [usize; CorpusLanguage::ALL.len()],
    pub(super) unavailable: [usize; CorpusLanguage::ALL.len()],
    pub(super) capacity: CorpusCapacityVerdict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CorpusCapacityVerdict {
    MeetsMinimum,
    Insufficient { observed: usize, required: usize },
}

pub(super) fn validate_source_inventory() -> Result<RealInventorySummary, InventoryInvariant> {
    validate_real_inventory()?;
    for input in local_fixture_inputs() {
        match input {
            LocalFixtureInput::Source { fixture, identity } => {
                let Some(bytes) = fixture.bytes else {
                    return Err(InventoryInvariant::FixtureAvailabilityMismatch {
                        language: fixture.language,
                    });
                };
                if fixture.absence.is_some() || identity != source_identity(bytes) {
                    return Err(InventoryInvariant::SourceBindingMismatch {
                        language: fixture.language,
                    });
                }
            }
            LocalFixtureInput::Unavailable { fixture, cause } => {
                if fixture.bytes.is_some()
                    || fixture.absence != Some(cause.kind)
                    || fixture.language != cause.language
                {
                    return Err(InventoryInvariant::SourceCauseMismatch {
                        language: fixture.language,
                    });
                }
            }
        }
    }
    let mut summary = RealInventorySummary {
        attempted: [0; CorpusLanguage::ALL.len()],
        source_bound: [0; CorpusLanguage::ALL.len()],
        unavailable: [0; CorpusLanguage::ALL.len()],
        capacity: CorpusCapacityVerdict::Insufficient {
            observed: REAL_PACKAGE_COUNT,
            required: MIN_REAL_PACKAGE_COUNT,
        },
    };
    for input in real_package_inputs() {
        match input {
            RealPackageInput::Source { case, source } => {
                let index = inventory_language_index(case.language);
                summary.attempted[index] += 1;
                summary.source_bound[index] += 1;
                let authority_tool = match source.authority {
                    AuthorityProvenance::Unresolved { tool }
                    | AuthorityProvenance::Bound { tool, .. } => tool,
                    AuthorityProvenance::Unavailable(_) => {
                        return Err(InventoryInvariant::SourceBindingMismatch {
                            language: case.language,
                        });
                    }
                };
                if source.coordinate != case.coordinate
                    || source.language != case.language
                    || source.profile != case.profile
                    || source.table != case.table
                    || authority_tool != native_tool(case.language)
                    || !source.path.starts_with(&source.source_root)
                    || source.identity != source_identity(&source.bytes)
                {
                    return Err(InventoryInvariant::SourceBindingMismatch {
                        language: case.language,
                    });
                }
            }
            RealPackageInput::Unavailable { case, cause } => {
                let index = inventory_language_index(case.language);
                summary.attempted[index] += 1;
                summary.unavailable[index] += 1;
                if cause.language != case.language {
                    return Err(InventoryInvariant::SourceCauseMismatch {
                        language: case.language,
                    });
                }
            }
        }
    }
    if summary.attempted.iter().sum::<usize>() != REAL_PACKAGE_COUNT
        || summary
            .source_bound
            .iter()
            .zip(summary.unavailable.iter())
            .any(|(bound, unavailable)| bound + unavailable == 0)
    {
        return Err(InventoryInvariant::TotalCount {
            observed: summary.attempted.iter().sum(),
            expected: REAL_PACKAGE_COUNT,
        });
    }
    summary.capacity = if summary.attempted.iter().sum::<usize>() >= MIN_REAL_PACKAGE_COUNT {
        CorpusCapacityVerdict::MeetsMinimum
    } else {
        CorpusCapacityVerdict::Insufficient {
            observed: summary.attempted.iter().sum(),
            required: MIN_REAL_PACKAGE_COUNT,
        }
    };
    Ok(summary)
}

const fn expected_table(language: CorpusLanguage) -> CorpusTable {
    match language {
        CorpusLanguage::Rust => CorpusTable::RustDriver,
        CorpusLanguage::TypeScript => CorpusTable::TypeScriptDriver,
        CorpusLanguage::Python => CorpusTable::PythonDriver,
        CorpusLanguage::Go => CorpusTable::GoDriver,
        CorpusLanguage::Java => CorpusTable::JavaDriver,
        CorpusLanguage::CSharp => CorpusTable::CSharpDriver,
        CorpusLanguage::Clang => CorpusTable::ClangEvidence,
    }
}

const fn inventory_language_index(language: CorpusLanguage) -> usize {
    match language {
        CorpusLanguage::Rust => 0,
        CorpusLanguage::TypeScript => 1,
        CorpusLanguage::Python => 2,
        CorpusLanguage::Go => 3,
        CorpusLanguage::Java => 4,
        CorpusLanguage::CSharp => 5,
        CorpusLanguage::Clang => 6,
    }
}
