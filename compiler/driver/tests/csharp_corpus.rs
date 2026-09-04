//! The real nuget corpus end to end: twenty pinned `nuget:ID@VERSION`
//! artifacts, each located, fetched under explicit bounds, digest-verified
//! against the registry's declared SHA-512, unpacked through the minimal
//! ZIP reader, run through the vendored Roslyn oracle, lowered through
//! `compile`/`compile_ir` with the produced authority image, published,
//! reopened, indexed, regenerated as a second generation over the same
//! artifact store, and re-validated from the store.
//!
//! Every truth-table constant below was derived from the actual fetched
//! package sources (one live fetch per row while authoring) and cites the
//! pinned primary it was read from.

#![forbid(unsafe_code)]
#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod csharp_support;

use compiler_driver::{
    CompileControl, CompileFailure, CompileOutput, CompileRequest, CompileScratch,
    CompiledFragment, ResolvedToolchain, SemanticAuthorityInput, ToolchainSelection, compile,
    compile_ir,
};
use compiler_ir::{
    DocFragmentInput, EntityKind, FragmentView, ImageProvenance, OccurrenceTarget, ReferenceKind,
};
use compiler_publication::{
    OpenPublicationScratch, PublicationScratch, PublishControl, open_published, publish_compiled,
};
use compiler_publication::{PublishedCompilation, immutable::ImmutableArtifactStore};
use compiler_vocabulary::{CSharpVersion, LanguageProfile, NativeTool, Stage};
use csharp_support::Error as SupportError;
use heart_identity::{ContentId, SourceFactDomain};
use server_index_build::{IndexBuildScratch, build};
use server_index_publish::{
    CompilationIndexScratch, encode_index_pack, plan_index_pack, seal_compilation_index,
};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use sha2::{Digest, Sha256};
use std::{
    fs,
    mem::MaybeUninit,
    path::Path,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};
use thiserror::Error;

const PROFILE: LanguageProfile = LanguageProfile::CSharp(CSharpVersion::CSharp14);
const STAGE: Stage = Stage::LowerIr;

/// Explicit download byte cap and deadline for every pinned package.
const DOWNLOAD_CAP: usize = 16 * 1024 * 1024;
const DOWNLOAD_DEADLINE: Duration = Duration::from_secs(60);
/// The oracle run bound from the locked card.
const ORACLE_DEADLINE: Duration = Duration::from_secs(120);

/// The documented second-generation probe: appended to a copy of the
/// primary source, regenerated through the oracle, recompiled, and required
/// in the second generation's entities. The `const` field lowers as a
/// `Constant` entity, the class as a `Record`.
const GEN_TWO_PROBE: &[u8] =
    b"\ninternal static class NudoxGenTwoProbe { internal const int Marker = 2; }\n";
const GEN_TWO_PROBE_CLASS: &[u8] = b"NudoxGenTwoProbe";
const GEN_TWO_PROBE_MARKER: &[u8] = b"Marker";

const FIDELITY_SOURCE: &[u8] =
    include_bytes!("../../languages/csharp/tests/fixtures/producer/fidelity.cs");
const FIDELITY_IMAGE: &[u8] =
    include_bytes!("../../languages/csharp/tests/fixtures/producer/fidelity.ncaimg");

#[derive(Debug, Error)]
enum TestError {
    #[error(transparent)]
    Support(SupportError),
    #[error("filesystem operation failed: {source}")]
    Io {
        #[source]
        source: std::io::Error,
    },
    #[error("toolchain resolution failed: {source}")]
    Toolchain {
        #[source]
        source: compiler_driver::ToolchainResolutionError,
    },
    #[error("fragment validation failed: {source}")]
    Fragment {
        #[source]
        source: compiler_ir::FragmentError,
    },
    #[error("publication failed: {cause}")]
    Publish { cause: String },
    #[error("publication reopen failed: {cause}")]
    Reopen { cause: String },
    #[error("index build or seal failed: {cause}")]
    Index { cause: String },
    #[error("journey fact falsifier: {0}")]
    Fact(&'static str),
    #[error("journey compile failed: {cause}")]
    JourneyCompile { cause: String },
    #[error("digest lineage mismatch")]
    Digest,
}

impl From<SupportError> for TestError {
    fn from(error: SupportError) -> Self {
        TestError::Support(error)
    }
}

fn io(source: std::io::Error) -> TestError {
    TestError::Io { source }
}

/// What the row's compile terminal is expected to be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Expectation {
    /// The full journey completes: IR, fragment, publication, index, and a
    /// second generation.
    Full,
    /// The reopened fragment carries more entities than the shared exact/
    /// lexical index segment bound admits, so the journey's exact typed
    /// terminal is the index build's EntityLimit admission rejection —
    /// the six 1.17.0 precedent from the python corpus. Compile, publish,
    /// and reopen still complete; `observed` is the reopened entity count.
    IndexEntityLimit { observed: usize },
    /// One declaration of the bound primary carries more interned attribute
    /// spellings than the shared 16-slot attribute list admits, so the
    /// journey's exact typed terminal is the lane's closed capacity
    /// rejection. `declaration` is the declaration coordinate and
    /// `spellings` its attribute-application count, both verified in-test
    /// through the public image reader before the compile runs.
    AttributeCapacity { declaration: u32, spellings: usize },
}

/// One locked corpus row: the pinned PURL, the live-confirmed primary path
/// inside the unpacked archive, the exact declared symbols the bound primary
/// must carry (name bytes + canonical entity kind, read from the fetched
/// package sources), the exact resolved-occurrence count the bound file
/// carries, and whether its XML summaries carry at least one link.
struct CorpusRow {
    label: &'static str,
    purl: &'static str,
    primary: &'static str,
    symbols: &'static [(&'static [u8], EntityKind)],
    occurrences: usize,
    doc_links: bool,
    expectation: Expectation,
}

/// nullable 1.3.1 — primary pinned after live confirmation: the largest
/// `.cs` (4541 bytes) ships in three TFM folders of equal depth; the
/// lexicographic winner is the net40 copy. Declaration 1 carries 23 applied
/// attribute spellings, one above the shared 16-slot attribute list, so the
/// row's terminal is the lane's exact typed capacity rejection.
const NULLABLE_ROW: CorpusRow = CorpusRow {
    label: "nullable 1.3.1",
    purl: "nuget:nullable@1.3.1",
    primary: "contentFiles/cs/net40/Nullable/MemberNotNullWhenAttribute.cs",
    symbols: &[],
    occurrences: 1170,
    doc_links: false,
    expectation: Expectation::AttributeCapacity {
        declaration: 1,
        spellings: 23,
    },
};

/// xunit.assert.source 2.9.3 — largest `.cs` by decoded length. Symbols read
/// from `contentFiles/cs/netstandard1.1/Asserts/StringAsserts.cs`. The
/// file's XML docs carry `<see>` links only outside the first `<summary>`
/// element, and the lane emits each doc row's first summary, so the row
/// pins no link expectation.
const XUNIT_ROW: CorpusRow = CorpusRow {
    label: "xunit.assert.source 2.9.3",
    purl: "nuget:xunit.assert.source@2.9.3",
    primary: "contentFiles/cs/netstandard1.1/Asserts/StringAsserts.cs",
    symbols: &[
        (b"Assert", EntityKind::Record),
        (b"Contains", EntityKind::Function),
        (b"Empty", EntityKind::Function),
        (b"charsLineEndings", EntityKind::Field),
    ],
    occurrences: 118,
    doc_links: false,
    expectation: Expectation::Full,
};

/// isexternalinit 1.0.3 — the whole package declares exactly one type in one
/// namespace (ten identical `.cs` copies across `content`/`contentFiles`
/// TFM folders), so the row asserts its complete entity surface; the
/// three-symbol minimum is unachievable in this real package. Symbols read
/// from `content/net40/IsExternalInit/IsExternalInit.cs`.
const ISEXTERNALINIT_ROW: CorpusRow = CorpusRow {
    label: "isexternalinit 1.0.3",
    purl: "nuget:isexternalinit@1.0.3",
    primary: "content/net40/IsExternalInit/IsExternalInit.cs",
    symbols: &[
        (b"System.Runtime.CompilerServices", EntityKind::Module),
        (b"IsExternalInit", EntityKind::Record),
    ],
    occurrences: 2,
    doc_links: false,
    expectation: Expectation::Full,
};

/// polyfill 2.0.0 — the raw-largest `.cs` (DefaultInterpolatedStringHandler.cs,
/// 36722 bytes) is wholly preprocessor-disabled under the locked oracle
/// command's fixed symbol set (`#if HAS_SPAN && !NET6_0_OR_GREATER` never
/// holds), so the authoring-time confirmation pinned the largest file that
/// emits declarations. Symbols read from
/// `contentFiles/cs/netstandard2.0/Polyfill/Polyfill_IEnumerable.cs`; its
/// only distinct declared names are the two asserted here.
const POLYFILL_ROW: CorpusRow = CorpusRow {
    label: "polyfill 2.0.0",
    purl: "nuget:polyfill@2.0.0",
    primary: "contentFiles/cs/netstandard2.0/Polyfill/Polyfill_IEnumerable.cs",
    symbols: &[
        (b"Polyfill", EntityKind::Record),
        (b"Except", EntityKind::Function),
    ],
    occurrences: 10,
    doc_links: false,
    expectation: Expectation::Full,
};

/// devlooped.tablestorage.source 5.5.0 — the largest `.cs` ships in two TFM
/// folders of equal depth; the lexicographic winner is netstandard2.0.
/// Symbols read from `contentFiles/cs/netstandard2.0/TableRepositoryQuery\`1.cs`.
const DEVLOOPED_ROW: CorpusRow = CorpusRow {
    label: "devlooped.tablestorage.source 5.5.0",
    purl: "nuget:devlooped.tablestorage.source@5.5.0",
    primary: "contentFiles/cs/netstandard2.0/TableRepositoryQuery`1.cs",
    symbols: &[
        (b"TableRepositoryQuery", EntityKind::Record),
        (b"ProjectionVisitor", EntityKind::Record),
        (b"JsonElementToDictionary", EntityKind::Function),
        (b"PartitionKey", EntityKind::Field),
    ],
    occurrences: 293,
    doc_links: true,
    expectation: Expectation::Full,
};

/// tinyioc 1.4.0-rc1 — the package's single source file (`content/TinyIoc.cs`,
/// the lexicographic winner over the equal-length `contentFiles/cs/any`
/// copy) resolves 1253 references and now completes under the authorized
/// 8192-row occurrence scratch bound; its 940 reopened entities stop the
/// index build at the shared 256-entity exact/lexical segment bound.
const TINYIOC_RC1_ROW: CorpusRow = CorpusRow {
    label: "tinyioc 1.4.0-rc1",
    purl: "nuget:tinyioc@1.4.0-rc1",
    primary: "content/TinyIoc.cs",
    symbols: &[],
    occurrences: 1253,
    doc_links: false,
    expectation: Expectation::IndexEntityLimit { observed: 940 },
};

/// ramltoopenapiconverter.sourceonly 0.21.0 — largest `.cs`. Symbols read
/// from `contentFiles/cs/any/RamlToOpenApiConverter/RamlConverter.Components.cs`.
const RAML_0210_ROW: CorpusRow = CorpusRow {
    label: "ramltoopenapiconverter.sourceonly 0.21.0",
    purl: "nuget:ramltoopenapiconverter.sourceonly@0.21.0",
    primary: "contentFiles/cs/any/RamlToOpenApiConverter/RamlConverter.Components.cs",
    symbols: &[
        (b"RamlConverter", EntityKind::Record),
        (b"TypeInfo", EntityKind::Record),
        (b"MapComponents", EntityKind::Function),
        (b"ReplaceUses", EntityKind::Function),
    ],
    occurrences: 192,
    doc_links: false,
    expectation: Expectation::Full,
};

/// ramltoopenapiconverter.sourceonly 0.8.0 — largest `.cs`. Symbols read
/// from `contentFiles/cs/any/RamlToOpenApiConverter/RamlConverter.Paths.cs`.
const RAML_080_ROW: CorpusRow = CorpusRow {
    label: "ramltoopenapiconverter.sourceonly 0.8.0",
    purl: "nuget:ramltoopenapiconverter.sourceonly@0.8.0",
    primary: "contentFiles/cs/any/RamlToOpenApiConverter/RamlConverter.Paths.cs",
    symbols: &[
        (b"RamlConverter", EntityKind::Record),
        (b"MapPaths", EntityKind::Function),
        (b"CreateDummyOpenApiReferenceSchema", EntityKind::Function),
    ],
    occurrences: 134,
    doc_links: false,
    expectation: Expectation::Full,
};

/// esp-net-source 0.6.4 — largest `.cs`. Symbols read from
/// `content/App_Packages/Esp.Net.0.6.4/Router.cs`. The bound file's
/// parameterless `void` executables (`Router` ctor, `PurgeEventQueues`)
/// were the zero-children function-pointer panic at `lower.rs:1595`; the
/// typed `Dangling` rejection and legal zero-arity path landed, and the
/// row now completes the full journey.
const ESP_NET_064_ROW: CorpusRow = CorpusRow {
    label: "esp-net-source 0.6.4",
    purl: "nuget:esp-net-source@0.6.4",
    primary: "content/App_Packages/Esp.Net.0.6.4/Router.cs",
    symbols: &[
        (b"Router", EntityKind::Record),
        (b"ModelChangedEventPublisher", EntityKind::Record),
        (b"CreateModelRouter", EntityKind::Function),
    ],
    occurrences: 218,
    doc_links: false,
    expectation: Expectation::Full,
};

/// esp-net-source 0.2.3 — largest `.cs`. Symbols read from
/// `content/App_Packages/Esp.Net.0.2.3/Router.cs` (`PurgeEventQueue`,
/// `ThrowIfHalted`, `ThrowIfInvalidThread`, …). Was red on the same
/// parameterless-`void` lane panic as `ESP_NET_064_ROW`; fixed with it.
const ESP_NET_023_ROW: CorpusRow = CorpusRow {
    label: "esp-net-source 0.2.3",
    purl: "nuget:esp-net-source@0.2.3",
    primary: "content/App_Packages/Esp.Net.0.2.3/Router.cs",
    symbols: &[
        (b"IRouter", EntityKind::Trait),
        (b"IEventPublisher", EntityKind::Trait),
        (b"Router", EntityKind::Record),
        (b"Status", EntityKind::Enum),
        (b"Idle", EntityKind::Variant),
    ],
    occurrences: 116,
    doc_links: false,
    expectation: Expectation::Full,
};

/// nullability.source 2.3.0 — largest `.cs`. Symbols read from
/// `contentFiles/cs/netstandard2.0/Nullability.Source/NullabilityInfoContext.cs`.
/// Was red on the same parameterless-`void` lane panic as `ESP_NET_064_ROW`
/// (`EnsureIsSupported`); fixed with it.
const NULLABILITY_230_ROW: CorpusRow = CorpusRow {
    label: "nullability.source 2.3.0",
    purl: "nuget:nullability.source@2.3.0",
    primary: "contentFiles/cs/netstandard2.0/Nullability.Source/NullabilityInfoContext.cs",
    symbols: &[
        (b"NullabilityInfoContext", EntityKind::Record),
        (b"NotAnnotatedStatus", EntityKind::Enum),
        (b"None", EntityKind::Variant),
        (b"NullableAttributeStateParser", EntityKind::Record),
    ],
    occurrences: 346,
    doc_links: true,
    expectation: Expectation::Full,
};

/// nullability.source 2.1.0 — largest `.cs`. Symbols read from
/// `contentFiles/cs/netstandard2.0/Nullability.Source/NullabilityInfoContext.cs`.
/// Was red on the same parameterless-`void` lane panic as `ESP_NET_064_ROW`
/// (`EnsureIsSupported`); fixed with it.
const NULLABILITY_210_ROW: CorpusRow = CorpusRow {
    label: "nullability.source 2.1.0",
    purl: "nuget:nullability.source@2.1.0",
    primary: "contentFiles/cs/netstandard2.0/Nullability.Source/NullabilityInfoContext.cs",
    symbols: &[
        (b"NullabilityInfoContext", EntityKind::Record),
        (b"NotAnnotatedStatus", EntityKind::Enum),
        (b"None", EntityKind::Variant),
        (b"NullableAttributeStateParser", EntityKind::Record),
    ],
    occurrences: 343,
    doc_links: true,
    expectation: Expectation::Full,
};

/// morelinq.source.moreenumerable.distinctby 1.0.2 — the package's single
/// `.cs`. Symbols read from `content/net35/MoreLinq/MoreEnumerable.DistinctBy.cs`.
const MORELINQ_DISTINCTBY_ROW: CorpusRow = CorpusRow {
    label: "morelinq.source.moreenumerable.distinctby 1.0.2",
    purl: "nuget:morelinq.source.moreenumerable.distinctby@1.0.2",
    primary: "content/net35/MoreLinq/MoreEnumerable.DistinctBy.cs",
    symbols: &[
        (b"MoreLinq", EntityKind::Module),
        (b"MoreEnumerable", EntityKind::Record),
        (b"DistinctBy", EntityKind::Function),
        (b"DistinctByImpl", EntityKind::Function),
    ],
    occurrences: 11,
    doc_links: false,
    expectation: Expectation::Full,
};

/// morelinq.source.moreenumerable.pairwise 1.0.2 — the package's single
/// `.cs`. Symbols read from `content/net20/MoreLinq/MoreEnumerable.Pairwise.cs`.
const MORELINQ_PAIRWISE_ROW: CorpusRow = CorpusRow {
    label: "morelinq.source.moreenumerable.pairwise 1.0.2",
    purl: "nuget:morelinq.source.moreenumerable.pairwise@1.0.2",
    primary: "content/net20/MoreLinq/MoreEnumerable.Pairwise.cs",
    symbols: &[
        (b"MoreLinq", EntityKind::Module),
        (b"MoreEnumerable", EntityKind::Record),
        (b"Pairwise", EntityKind::Function),
        (b"PairwiseImpl", EntityKind::Function),
    ],
    occurrences: 20,
    doc_links: false,
    expectation: Expectation::Full,
};

/// morelinq.source.moreenumerable.acquire 1.0.2 — the package's single
/// `.cs`. Symbols read from `content/net20/MoreLinq/MoreEnumerable.Acquire.cs`.
const MORELINQ_ACQUIRE_ROW: CorpusRow = CorpusRow {
    label: "morelinq.source.moreenumerable.acquire 1.0.2",
    purl: "nuget:morelinq.source.moreenumerable.acquire@1.0.2",
    primary: "content/net20/MoreLinq/MoreEnumerable.Acquire.cs",
    symbols: &[
        (b"MoreLinq", EntityKind::Module),
        (b"MoreEnumerable", EntityKind::Record),
        (b"Acquire", EntityKind::Function),
    ],
    occurrences: 10,
    doc_links: true,
    expectation: Expectation::Full,
};

/// morelinq.source.moreenumerable.assertcount 1.0.2 — the package's single
/// `.cs`. Symbols read from `content/net20/MoreLinq/MoreEnumerable.AssertCount.cs`.
const MORELINQ_ASSERTCOUNT_ROW: CorpusRow = CorpusRow {
    label: "morelinq.source.moreenumerable.assertcount 1.0.2",
    purl: "nuget:morelinq.source.moreenumerable.assertcount@1.0.2",
    primary: "content/net20/MoreLinq/MoreEnumerable.AssertCount.cs",
    symbols: &[
        (b"MoreLinq", EntityKind::Module),
        (b"MoreEnumerable", EntityKind::Record),
        (b"AssertCountImpl", EntityKind::Function),
        (b"ExpectingCountYieldingImpl", EntityKind::Function),
    ],
    occurrences: 10,
    doc_links: false,
    expectation: Expectation::Full,
};

/// morelinq.source.moreenumerable.batch 1.0.2 — the package's single `.cs`.
/// Symbols read from `content/net20/MoreLinq/MoreEnumerable.Batch.cs`.
const MORELINQ_BATCH_ROW: CorpusRow = CorpusRow {
    label: "morelinq.source.moreenumerable.batch 1.0.2",
    purl: "nuget:morelinq.source.moreenumerable.batch@1.0.2",
    primary: "content/net20/MoreLinq/MoreEnumerable.Batch.cs",
    symbols: &[
        (b"MoreLinq", EntityKind::Module),
        (b"MoreEnumerable", EntityKind::Record),
        (b"Batch", EntityKind::Function),
        (b"BatchImpl", EntityKind::Function),
    ],
    occurrences: 21,
    doc_links: false,
    expectation: Expectation::Full,
};

/// morelinq.source.moreenumerable.generate 1.0.2 — the package's single
/// `.cs`. Symbols read from `content/net20/MoreLinq/MoreEnumerable.Generate.cs`.
const MORELINQ_GENERATE_ROW: CorpusRow = CorpusRow {
    label: "morelinq.source.moreenumerable.generate 1.0.2",
    purl: "nuget:morelinq.source.moreenumerable.generate@1.0.2",
    primary: "content/net20/MoreLinq/MoreEnumerable.Generate.cs",
    symbols: &[
        (b"MoreLinq", EntityKind::Module),
        (b"MoreEnumerable", EntityKind::Record),
        (b"Generate", EntityKind::Function),
        (b"GenerateImpl", EntityKind::Function),
    ],
    occurrences: 5,
    doc_links: false,
    expectation: Expectation::Full,
};

/// morelinq.source.moreenumerable.generatebyindex 1.0.2 — the package's
/// single `.cs`. Symbols read from
/// `content/net20/MoreLinq/MoreEnumerable.GenerateByIndex.cs`.
const MORELINQ_GENERATEBYINDEX_ROW: CorpusRow = CorpusRow {
    label: "morelinq.source.moreenumerable.generatebyindex 1.0.2",
    purl: "nuget:morelinq.source.moreenumerable.generatebyindex@1.0.2",
    primary: "content/net20/MoreLinq/MoreEnumerable.GenerateByIndex.cs",
    symbols: &[
        (b"MoreLinq", EntityKind::Module),
        (b"MoreEnumerable", EntityKind::Record),
        (b"GenerateByIndex", EntityKind::Function),
        (b"GenerateByIndexImpl", EntityKind::Function),
    ],
    occurrences: 8,
    doc_links: false,
    expectation: Expectation::Full,
};

/// tinyioc 1.3.0 — the package's single source file (`Content/TinyIoC.cs`)
/// resolves 1170 references and now completes under the authorized 8192-row
/// occurrence scratch bound; its 876 reopened entities stop the index build
/// at the shared 256-entity exact/lexical segment bound.
const TINYIOC_130_ROW: CorpusRow = CorpusRow {
    label: "tinyioc 1.3.0",
    purl: "nuget:tinyioc@1.3.0",
    primary: "Content/TinyIoC.cs",
    symbols: &[],
    occurrences: 1170,
    doc_links: false,
    expectation: Expectation::IndexEntityLimit { observed: 876 },
};

#[test]
fn malformed_purl_is_typed_rejection() -> Result<(), TestError> {
    for malformed in [
        "nuget:tinyioc",
        "npm:react@18.2.0",
        "nuget:tinyioc@1.3.0@rc",
        "nuget:@1.3.0",
        "nuget:tinyioc@",
    ] {
        match csharp_support::Purl::parse(malformed) {
            Err(csharp_support::Error::Purl { input }) if input == malformed => {}
            _ => {
                return Err(TestError::Fact(
                    "malformed PURL was accepted or lost its input",
                ));
            }
        }
    }
    Ok(())
}

/// The live transport falsifiers on one pinned package: the 1 KiB cap
/// breach, the 1 ns deadline, the tampered-digest comparison, and the
/// corrupted-archive unpack.
#[test]
fn real_downloader_enforces_cap_timeout_digest_and_archive_corruption() -> Result<(), TestError> {
    let purl = csharp_support::Purl::parse("nuget:tinyioc@1.3.0")?;
    let (url, declared) = csharp_support::locate(&purl)?;
    if !url.ends_with("tinyioc.1.3.0.nupkg") {
        return Err(TestError::Fact("archive URL was not located"));
    }
    match csharp_support::download(&url, 1024, Instant::now() + DOWNLOAD_DEADLINE) {
        Err(csharp_support::Error::Cap {
            cap: 1024,
            observed,
        }) if observed > 1024 => {}
        _ => {
            return Err(TestError::Fact(
                "real downloader admitted the 1 KiB cap breach",
            ));
        }
    }
    match csharp_support::download(&url, DOWNLOAD_CAP, Instant::now() + Duration::from_nanos(1)) {
        Err(csharp_support::Error::Deadline { observed }) => {
            let _ = observed;
        }
        _ => {
            return Err(TestError::Fact(
                "1ns deadline did not produce typed timeout",
            ));
        }
    }
    let archive = csharp_support::download(&url, DOWNLOAD_CAP, Instant::now() + DOWNLOAD_DEADLINE)?;
    // The digest the registry declares must match the fetched bytes.
    verify_digest(&archive, &declared)?;
    // A wrong constant must be caught by the same typed comparison.
    let mut tampered = declared;
    tampered[0] ^= 1;
    match verify_digest(&archive, &tampered) {
        Err(TestError::Digest) => {}
        _ => return Err(TestError::Fact("tampered digest was not rejected")),
    }
    // A single flipped mid-byte must reject the archive at unpack.
    let mut corrupt = archive.clone();
    let at = corrupt.len() / 2;
    let byte = corrupt
        .get_mut(at)
        .ok_or(TestError::Fact("empty archive"))?;
    *byte ^= 1;
    let root = csharp_support::fresh_dir("corrupt")?;
    let result = csharp_support::unpack(&corrupt, &root);
    fs::remove_dir_all(root).map_err(io)?;
    match result {
        Err(csharp_support::Error::Archive { .. }) => Ok(()),
        _ => Err(TestError::Fact("mutated archive was accepted")),
    }
}

fn verify_digest(archive: &[u8], declared: &[u8; 64]) -> Result<(), TestError> {
    if csharp_support::sha512(archive) != *declared {
        return Err(TestError::Digest);
    }
    Ok(())
}

/// The committed producer fixture must survive image admission, C# collect,
/// canonical admission, and fragment validation without dropping its deep
/// Roslyn facts.  The operator and constructor names are source spellings;
/// the delegate's three carriers and the interface binding are structural
/// truths from `languages/csharp/tests/fixtures/producer/fidelity.cs`.
#[test]
fn committed_fidelity_fixture_collects_without_faults() -> Result<(), TestError> {
    let image = compiler_languages_csharp::CSharpImage::open(FIDELITY_IMAGE).map_err(|cause| {
        TestError::JourneyCompile {
            cause: format!("authority image admission failed: {cause:?}"),
        }
    })?;
    let declarations = image
        .declarations()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|cause| TestError::JourneyCompile {
            cause: format!("declaration collect failed: {cause:?}"),
        })?;
    for declaration in &declarations {
        for _parameter in declaration.parameters.iter() {}
        for _parameter in declaration.type_parameters.iter() {}
    }
    image
        .attributes()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|cause| TestError::JourneyCompile {
            cause: format!("attribute collect failed: {cause:?}"),
        })?;
    image
        .docs()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|cause| TestError::JourneyCompile {
            cause: format!("documentation collect failed: {cause:?}"),
        })?;
    let interface_bindings = image
        .references()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|cause| TestError::JourneyCompile {
            cause: format!("reference collect failed: {cause:?}"),
        })?
        .into_iter()
        .filter(|reference| {
            reference.kind == compiler_languages_csharp::ReferenceTag::InterfaceImplementation
        })
        .count();
    if interface_bindings == 0 {
        return Err(TestError::Fact(
            "fixture lost interface implementation binding",
        ));
    }
    let operator = declarations.iter().any(|declaration| {
        declaration.kind == compiler_languages_csharp::DeclarationKind::Operator
            && declaration.name.bytes == b"+"
    });
    let constructor = declarations.iter().any(|declaration| {
        declaration.kind == compiler_languages_csharp::DeclarationKind::Constructor
            && declaration.name.bytes == b"Widget"
    });
    let delegate = declarations.iter().any(|declaration| {
        declaration.kind == compiler_languages_csharp::DeclarationKind::Delegate
            && declaration.parameters.len() == 3
    });
    if !(operator && constructor && delegate) {
        return Err(TestError::Fact("fixture declaration truth changed"));
    }

    let tool = csharp_toolchain()?;
    let work = csharp_support::fresh_dir("fidelity-fixture-work")?;
    let mut output = vec![0_u8; 8 * 1024 * 1024];
    let fragment = compile_fragment(FIDELITY_SOURCE, tool, FIDELITY_IMAGE, &work, &mut output)?;
    let view = FragmentView::validate(fragment.fragment.as_ref())
        .map_err(|source| TestError::Fragment { source })?;
    let atoms: Vec<&[u8]> = view.atoms().map(|atom| atom.bytes).collect();
    let has_operator = view
        .entities()
        .any(|entity| atoms.get(entity.name.raw as usize).copied() == Some(b"+".as_slice()));
    let has_constructor = view
        .entities()
        .any(|entity| atoms.get(entity.name.raw as usize).copied() == Some(b"Widget".as_slice()));
    if !(has_operator && has_constructor) {
        return Err(TestError::Fact(
            "fixture facts absent from canonical fragment",
        ));
    }
    let mut occurrences = view
        .occurrences()
        .ok_or(TestError::Fact("fixture occurrence lane absent"))?;
    let mut has_interface_call = false;
    while let Some(row) = occurrences.next() {
        let row = row.map_err(|cause| TestError::Index {
            cause: cause.to_string(),
        })?;
        if row.occurrence.kind == ReferenceKind::MethodCall
            && matches!(row.occurrence.target, OccurrenceTarget::Local(_))
        {
            has_interface_call = true;
            break;
        }
    }
    if !has_interface_call {
        return Err(TestError::Fact(
            "interface implementation occurrence did not survive as MethodCall",
        ));
    }
    drop(fragment);
    fs::remove_dir_all(work).map_err(io)?;
    Ok(())
}

/// Resolves the configured dotnet executable into a validated Roslyn
/// toolchain identity; a missing tool is a typed failure, never a skip.
fn csharp_toolchain() -> Result<ResolvedToolchain<'static>, TestError> {
    let path = csharp_support::dotnet_executable()?;
    let path = Box::leak(path.canonicalize().map_err(io)?.into_boxed_path());
    let output = std::process::Command::new(&*path)
        .arg("--version")
        .output()
        .map_err(io)?;
    let version = if output.stdout.is_empty() {
        output.stderr.as_slice()
    } else {
        output.stdout.as_slice()
    };
    ResolvedToolchain::from_version(NativeTool::CSharpCompiler, path, version)
        .map_err(|source| TestError::Toolchain { source })
}

/// Runs the vendored Roslyn oracle for one bound source file.
fn oracle_image(roots: &[&Path], assembly: &str, binding: &Path) -> Result<Vec<u8>, TestError> {
    csharp_support::authority_image(roots, assembly, binding, Instant::now() + ORACLE_DEADLINE)
        .map_err(TestError::from)
}

fn compile_request<'source, 'toolchain, 'cancel>(
    source: &'source [u8],
    tool: ResolvedToolchain<'toolchain>,
    image: &'source [u8],
    cancelled: &'cancel AtomicBool,
) -> CompileRequest<'source, 'toolchain, 'cancel> {
    CompileRequest {
        profile: PROFILE,
        stage: STAGE,
        source,
        declaration_scope: compiler_driver::DeclarationScope::fixture(),
        toolchain: ToolchainSelection::ResolvedNative(tool),
        authority: SemanticAuthorityInput::CSharp { image },
        control: CompileControl {
            deadline: Instant::now() + Duration::from_secs(60),
            cancelled,
        },
    }
}

/// Compiles one bound primary into its durable canonical fragment, lending
/// the caller-owned output buffer the fragment borrows.
fn compile_fragment<'source, 'output>(
    source: &'source [u8],
    tool: ResolvedToolchain<'_>,
    image: &'source [u8],
    work: &Path,
    output: &'output mut [u8],
) -> Result<CompiledFragment<'output>, TestError> {
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [0_u8; 4096];
    compile(
        compile_request(source, tool, image, &cancelled),
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: work,
        },
        CompileOutput {
            fragment_output: output,
        },
    )
    .map_err(|failure| TestError::JourneyCompile {
        cause: format!("{failure:?}"),
    })
}

/// Asserts the row's committed truth table against the decoded entity lane.
fn assert_symbols(
    decoded: &FragmentView<'_>,
    symbols: &[(&[u8], EntityKind)],
) -> Result<(), TestError> {
    let atoms: Vec<&[u8]> = decoded.atoms().map(|atom| atom.bytes).collect();
    let facts: Vec<(&[u8], EntityKind)> = decoded
        .entities()
        .map(|entity| {
            (
                atoms.get(entity.name.raw as usize).copied().unwrap_or(&[]),
                entity.kind,
            )
        })
        .collect();
    for (wanted, kind) in symbols {
        if !facts
            .iter()
            .any(|(name, found)| *name == *wanted && found == kind)
        {
            return Err(TestError::Fact("required package declaration absent"));
        }
    }
    Ok(())
}

/// Counts the decoded fragment's resolved occurrences.
fn count_occurrences(decoded: &FragmentView<'_>) -> Result<usize, TestError> {
    let mut occurrences = decoded
        .occurrences()
        .ok_or(TestError::Fact("occurrence lane absent"))?;
    let mut observed = 0_usize;
    while let Some(occurrence) = occurrences.next() {
        occurrence.map_err(|cause| TestError::Index {
            cause: cause.to_string(),
        })?;
        observed += 1;
    }
    Ok(observed)
}

/// Asserts at least one documentation link when the row's summaries carry
/// XML docs.
fn assert_doc_links(decoded: &FragmentView<'_>) -> Result<(), TestError> {
    let mut docs = decoded
        .docs()
        .ok_or(TestError::Fact("documentation lane absent"))?;
    while let Some(doc) = docs.next() {
        let doc = doc.map_err(|cause| TestError::Index {
            cause: cause.to_string(),
        })?;
        if matches!(doc.fragment, DocFragmentInput::Link { .. }) {
            return Ok(());
        }
    }
    Err(TestError::Fact("no documentation link in XML summaries"))
}

/// Opens one durable journal over `directory`.
fn create_journal(directory: &Path) -> Result<DurablePublisher, TestError> {
    let limits = PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN)
        .map_err(|cause| TestError::Index {
        cause: cause.to_string(),
    })?;
    DurablePublisher::create(&PublicationPaths::in_directory(directory), limits).map_err(|cause| {
        TestError::Publish {
            cause: cause.to_string(),
        }
    })
}

/// Reopens one durable journal over `directory`.
fn reopen_journal(directory: &Path) -> Result<DurablePublisher, TestError> {
    let limits = PublicationLimits::new(std::num::NonZeroUsize::MIN, std::num::NonZeroUsize::MIN)
        .map_err(|cause| TestError::Index {
        cause: cause.to_string(),
    })?;
    DurablePublisher::reopen(&PublicationPaths::in_directory(directory), limits).map_err(|cause| {
        TestError::Reopen {
            cause: cause.to_string(),
        }
    })
}

fn publish_fragment(
    journal: &DurablePublisher,
    artifacts: &Path,
    fragment: &CompiledFragment<'_>,
) -> Result<PublishedCompilation, TestError> {
    let mut manifest = vec![0_u8; 1 << 20];
    let mut manifest_facts = [None; 1];
    let mut ordinals = [0_usize; 1];
    let mut locality = vec![0_u8; 1 << 16];
    let mut binding = vec![0_u8; 128];
    publish_compiled(
        journal,
        artifacts,
        std::slice::from_ref(fragment),
        PublishControl::Continue,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut manifest_facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )
    .map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })
}

/// The shared fetch→compile→publish→reopen→index→second-generation→
/// old-fragment-revalidate skeleton behind the twenty locked corpus rows.
fn corpus_row_lifecycle(row: &CorpusRow) -> Result<(), TestError> {
    let purl = csharp_support::Purl::parse(row.purl)?;
    let (url, declared) = csharp_support::locate(&purl)?;
    let archive = csharp_support::download(&url, DOWNLOAD_CAP, Instant::now() + DOWNLOAD_DEADLINE)?;
    verify_digest(&archive, &declared)?;
    let root = csharp_support::fresh_dir("journey")?;
    csharp_support::unpack(&archive, &root)?;
    let primary_path = csharp_support::primary(&root, row.primary)?;
    let source = fs::read(&primary_path).map_err(io)?;
    let tool = csharp_toolchain()?;
    let image = oracle_image(&[&root], &purl.name, &primary_path)?;
    let work = csharp_support::fresh_dir("work")?;

    // The produced image must bind exactly the fetched primary bytes and
    // resolve exactly the pinned reference count.
    let opened_image = compiler_languages_csharp::CSharpImage::open(&image).map_err(|cause| {
        TestError::JourneyCompile {
            cause: format!("{cause:?}"),
        }
    })?;
    if opened_image.source_digest() != <[u8; 32]>::from(Sha256::digest(&source)) {
        return Err(TestError::Digest);
    }
    let mut resolved = 0_usize;
    for reference in opened_image.references() {
        reference.map_err(|cause| TestError::JourneyCompile {
            cause: format!("{cause:?}"),
        })?;
        resolved += 1;
    }
    let attributes_of = |declaration: u32| -> Result<usize, TestError> {
        let mut count = 0_usize;
        for attribute in opened_image.attributes() {
            let attribute = attribute.map_err(|cause| TestError::JourneyCompile {
                cause: format!("{cause:?}"),
            })?;
            if attribute.declaration == declaration {
                count += 1;
            }
        }
        Ok(count)
    };
    match &row.expectation {
        Expectation::Full | Expectation::IndexEntityLimit { .. } => {}
        Expectation::AttributeCapacity {
            declaration,
            spellings: expected,
        } => {
            let observed = attributes_of(*declaration)?;
            if observed != *expected {
                return Err(TestError::Fact("attribute capacity operand drifted"));
            }
        }
    }

    // The lane's typed capacity terminal: interned attribute spellings beyond
    // the shared 16-slot list stop before a fragment is emitted.
    let capacity_operand = match &row.expectation {
        Expectation::Full | Expectation::IndexEntityLimit { .. } => None,
        Expectation::AttributeCapacity {
            declaration,
            spellings,
        } => Some(format!(
            "declaration {declaration} carries {spellings} attribute spellings against the 16-slot attribute list"
        )),
    };
    if let Some(operand) = capacity_operand {
        let cancelled = AtomicBool::new(false);
        let mut diagnostic = [0_u8; 4096];
        let failed = compile_ir(
            compile_request(&source, tool, &image, &cancelled),
            CompileScratch {
                diagnostic_output: &mut diagnostic,
                native_work: &work,
            },
        );
        match failed {
            Err(CompileFailure::CSharpProjection { .. }) => {
                eprintln!(
                    "csharp corpus typed terminal: {} stopped at the lane's closed capacity bound ({operand})",
                    row.label
                );
                fs::remove_dir_all(&root).map_err(io)?;
                fs::remove_dir_all(&work).map_err(io)?;
                return Ok(());
            }
            _ => {
                return Err(TestError::Fact(
                    "capacity row did not produce its exact typed terminal",
                ));
            }
        }
    }

    let ir = compile_ir(
        compile_request(&source, tool, &image, &AtomicBool::new(false)),
        CompileScratch {
            diagnostic_output: &mut [0_u8; 4096],
            native_work: &work,
        },
    )
    .map_err(|failure| TestError::JourneyCompile {
        cause: format!("{failure:?}"),
    })?;
    // Identity law: the persisted source ContentId is the primary bytes' digest.
    let ImageProvenance::Captured {
        source: image_source,
        ..
    } = ir.ir.image_provenance()
    else {
        return Err(TestError::Digest);
    };
    if image_source.identity != ContentId::<SourceFactDomain>::from_canonical_bytes(&source) {
        return Err(TestError::Digest);
    }

    let mut fragment_output = vec![0_u8; 8 * 1024 * 1024];
    let fragment = compile_fragment(&source, tool, &image, &work, &mut fragment_output)?;
    let fragment_bytes = fragment.fragment.as_ref().to_vec();
    let entity_count;
    {
        let decoded = FragmentView::validate(fragment.fragment.as_ref())
            .map_err(|source| TestError::Fragment { source })?;
        assert_symbols(&decoded, row.symbols)?;
        let observed = count_occurrences(&decoded)?;
        if observed != row.occurrences {
            return Err(TestError::Fact("resolved occurrence count drifted"));
        }
        if row.doc_links {
            assert_doc_links(&decoded)?;
        }
        entity_count = decoded.entities().len();
    }
    let root_store = csharp_support::fresh_dir("published")?;
    let journal_dir = root_store.join("journal");
    let artifacts = root_store.join("artifacts");
    let journal = create_journal(&journal_dir)?;
    let first_published = publish_fragment(&journal, &artifacts, &fragment)?;
    let first_generation = first_published.publication.generation;
    journal.shutdown().map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    let first_fragment_facts = {
        let reopened = reopen_journal(&journal_dir)?;
        let mut manifest = vec![0_u8; 1 << 20];
        let mut manifest_facts = [None; 1];
        let mut fragments = vec![0_u8; 8 * 1024 * 1024];
        let mut locality = vec![0_u8; 1 << 16];
        let opened = open_published(
            &reopened,
            &artifacts,
            OpenPublicationScratch {
                manifest_output: &mut manifest,
                manifest_facts: &mut manifest_facts,
                fragment_output: &mut fragments,
                locality_output: &mut locality,
            },
        )
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?
        .ok_or_else(|| TestError::Reopen {
            cause: "no published compilation".to_owned(),
        })?;
        let fragment_ref = opened
            .fragments()
            .next()
            .ok_or_else(|| TestError::Reopen {
                cause: "published compilation has no fragment".to_owned(),
            })?
            .map_err(|cause| TestError::Reopen {
                cause: cause.to_string(),
            })?;
        let facts = fragment_ref.facts;
        let entity_count = fragment_ref.view.entities().len();
        let atom_count = fragment_ref.view.atoms().len();
        let type_node_count = fragment_ref.view.type_nodes().len();
        let mut projections = (0..entity_count)
            .map(|_| MaybeUninit::uninit())
            .collect::<Vec<_>>();
        let mut entities = (0..entity_count)
            .map(|_| MaybeUninit::uninit())
            .collect::<Vec<_>>();
        let mut exact = (0..entity_count)
            .map(|_| MaybeUninit::uninit())
            .collect::<Vec<_>>();
        let mut lexical = (0..entity_count)
            .map(|_| MaybeUninit::uninit())
            .collect::<Vec<_>>();
        let mut atoms = (0..atom_count)
            .map(|_| MaybeUninit::uninit())
            .collect::<Vec<_>>();
        let mut types = (0..type_node_count)
            .map(|_| MaybeUninit::uninit())
            .collect::<Vec<_>>();
        let prepared = match build(
            &fragment_ref,
            IndexBuildScratch {
                projections: &mut projections,
                entities: &mut entities,
                exact_rows: &mut exact,
                lexical_rows: &mut lexical,
                atoms: &mut atoms,
                type_nodes: &mut types,
            },
        ) {
            Ok(prepared) => Some(prepared),
            Err(server_index_build::BuildError::Admission(
                server_index_build::BuildAdmissionError::EntityLimit { maximum, observed },
            )) if row.expectation == Expectation::IndexEntityLimit { observed }
                && maximum == 256 =>
            {
                eprintln!(
                    "{} typed index terminal: shared segment bound {maximum}, \
                     reopened entities {observed}",
                    row.label
                );
                None
            }
            Err(cause) => {
                return Err(TestError::Index {
                    cause: cause.to_string(),
                });
            }
        };
        if let Some(prepared) = prepared {
            let mut exact_ids = [prepared.exact.id];
            let mut lexical_ids = [prepared.lexical.id];
            let sealed = seal_compilation_index(
                opened,
                std::slice::from_ref(&prepared),
                CompilationIndexScratch {
                    exact: &mut exact_ids,
                    lexical: &mut lexical_ids,
                },
            )
            .map_err(|cause| TestError::Index {
                cause: cause.error.to_string(),
            })?;
            let plan = plan_index_pack(&sealed).map_err(|cause| TestError::Index {
                cause: cause.to_string(),
            })?;
            let mut encoded = vec![0_u8; plan.encoded_bytes];
            encode_index_pack(&plan, &mut encoded).map_err(|cause| TestError::Index {
                cause: cause.to_string(),
            })?;
        }
        reopened.shutdown().map_err(|cause| TestError::Publish {
            cause: cause.to_string(),
        })?;
        facts
    };

    // Second generation: the documented probe appended to a copy of the
    // primary source, a regenerated authority image, and a second journal
    // over the SAME immutable artifact store.
    let probe_root = csharp_support::fresh_dir("gen-two")?;
    csharp_support::copy_dir(&root, &probe_root)?;
    let probe_primary = probe_root.join(row.primary);
    let mut modified = source.clone();
    modified.extend_from_slice(GEN_TWO_PROBE);
    fs::write(&probe_primary, &modified).map_err(io)?;
    let second_image = oracle_image(&[&probe_root], &purl.name, &probe_primary)?;
    let mut second_output = vec![0_u8; 8 * 1024 * 1024];
    let second = compile_fragment(&modified, tool, &second_image, &work, &mut second_output)?;
    let second_journal_dir = root_store.join("journal-second");
    let second_journal = create_journal(&second_journal_dir)?;
    let second_published = publish_fragment(&second_journal, &artifacts, &second)?;
    let second_generation = second_published.publication.generation;
    second_journal
        .shutdown()
        .map_err(|cause| TestError::Publish {
            cause: cause.to_string(),
        })?;
    let mut newest_manifest = vec![0_u8; 1 << 20];
    let mut newest_facts = [None; 1];
    let mut newest_fragments = vec![0_u8; 8 * 1024 * 1024];
    let mut newest_locality = vec![0_u8; 1 << 16];
    let newest = reopen_journal(&second_journal_dir)?;
    let newest_opened = open_published(
        &newest,
        &artifacts,
        OpenPublicationScratch {
            manifest_output: &mut newest_manifest,
            manifest_facts: &mut newest_facts,
            fragment_output: &mut newest_fragments,
            locality_output: &mut newest_locality,
        },
    )
    .map_err(|cause| TestError::Reopen {
        cause: cause.to_string(),
    })?
    .ok_or_else(|| TestError::Reopen {
        cause: "no newest published compilation".to_owned(),
    })?;
    {
        let newest_fragment = newest_opened
            .fragments()
            .next()
            .ok_or_else(|| TestError::Reopen {
                cause: "newest compilation has no fragment".to_owned(),
            })?
            .map_err(|cause| TestError::Reopen {
                cause: cause.to_string(),
            })?;
        let second_view = FragmentView::validate(newest_fragment.view.as_ref())
            .map_err(|source| TestError::Fragment { source })?;
        let atoms: Vec<&[u8]> = second_view.atoms().map(|atom| atom.bytes).collect();
        for (wanted, kind) in [
            (GEN_TWO_PROBE_CLASS, EntityKind::Record),
            (GEN_TWO_PROBE_MARKER, EntityKind::Constant),
        ] {
            if !second_view.entities().any(|entity| {
                entity.kind == kind
                    && atoms.get(entity.name.raw as usize).copied().unwrap_or(&[]) == wanted
            }) {
                return Err(TestError::Fact("new generation lost the probe declaration"));
            }
        }
    }
    drop(newest_opened);
    newest.shutdown().map_err(|cause| TestError::Publish {
        cause: cause.to_string(),
    })?;
    if first_generation == second_generation {
        return Err(TestError::Fact(
            "second publication reused first generation",
        ));
    }

    // The first generation's fragment keeps validating from the artifact
    // store after the second generation landed over the same store.
    let mut old_fragment_output = vec![0_u8; 8 * 1024 * 1024];
    let old_fragment = ImmutableArtifactStore::new(&artifacts)
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?
        .open(first_fragment_facts, &mut old_fragment_output)
        .map_err(|cause| TestError::Reopen {
            cause: cause.to_string(),
        })?;
    if csharp_support::sha512(old_fragment.as_ref()) != csharp_support::sha512(&fragment_bytes) {
        return Err(TestError::Digest);
    }
    FragmentView::validate(old_fragment.as_ref())
        .map_err(|source| TestError::Fragment { source })?;
    drop(old_fragment);
    fs::remove_dir_all(&root).map_err(io)?;
    fs::remove_dir_all(&root_store).map_err(io)?;
    fs::remove_dir_all(&probe_root).map_err(io)?;
    fs::remove_dir_all(&work).map_err(io)?;
    eprintln!(
        "csharp corpus row ok: {} entities={entity_count} occurrences={} doc_links={} index=sealed gen2=sealed",
        row.label, row.occurrences, row.doc_links
    );
    Ok(())
}

#[test]
fn row_nullable_1_3_1_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&NULLABLE_ROW)
}

#[test]
fn row_xunit_assert_source_2_9_3_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&XUNIT_ROW)
}

#[test]
fn row_isexternalinit_1_0_3_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&ISEXTERNALINIT_ROW)
}

#[test]
fn row_polyfill_2_0_0_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&POLYFILL_ROW)
}

#[test]
fn row_devlooped_tablestorage_source_5_5_0_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&DEVLOOPED_ROW)
}

#[test]
fn row_tinyioc_1_4_0_rc1_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&TINYIOC_RC1_ROW)
}

#[test]
fn row_raml_to_open_api_source_only_0_21_0_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&RAML_0210_ROW)
}

#[test]
fn row_raml_to_open_api_source_only_0_8_0_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&RAML_080_ROW)
}

#[test]
fn row_esp_net_source_0_6_4_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&ESP_NET_064_ROW)
}

#[test]
fn row_esp_net_source_0_2_3_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&ESP_NET_023_ROW)
}

#[test]
fn row_nullability_source_2_3_0_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&NULLABILITY_230_ROW)
}

#[test]
fn row_nullability_source_2_1_0_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&NULLABILITY_210_ROW)
}

#[test]
fn row_morelinq_distinctby_1_0_2_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&MORELINQ_DISTINCTBY_ROW)
}

#[test]
fn row_morelinq_pairwise_1_0_2_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&MORELINQ_PAIRWISE_ROW)
}

#[test]
fn row_morelinq_acquire_1_0_2_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&MORELINQ_ACQUIRE_ROW)
}

#[test]
fn row_morelinq_assertcount_1_0_2_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&MORELINQ_ASSERTCOUNT_ROW)
}

#[test]
fn row_morelinq_batch_1_0_2_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&MORELINQ_BATCH_ROW)
}

#[test]
fn row_morelinq_generate_1_0_2_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&MORELINQ_GENERATE_ROW)
}

#[test]
fn row_morelinq_generatebyindex_1_0_2_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&MORELINQ_GENERATEBYINDEX_ROW)
}

#[test]
fn row_tinyioc_1_3_0_full_lifecycle() -> Result<(), TestError> {
    corpus_row_lifecycle(&TINYIOC_130_ROW)
}
