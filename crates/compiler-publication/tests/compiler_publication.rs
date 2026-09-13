//! Exercises the `compiler-publication` tests compiler-publication contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use std::{
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use compiler_driver::{CompiledFragment, CompiledSemantic};
use compiler_ir::{AtomId, TypeId};
use compiler_ir::{
    AtomInput, EntityKind, EntityRecord, FragmentRangeManifest, FragmentView, IrBuilder,
    PackageLineage, PreparedFragment, PrimitiveType, SemanticCoreReader, SourceIdentity, TypeNode,
};
use compiler_publication::binding::{COMPILATION_BINDING_BYTES, CompilationBindingView};
use compiler_publication::{
    OpenPublicationScratch, OpenSemanticPublicationScratch, OpenedFragment, OpenedFragmentCursor,
    OpenedFragmentError, OpenedSemanticArtifactError, PublicationScratch, PublishCompiledError,
    PublishControl, PublishSemanticError, SemanticPublicationScratch, UncommittedPublication,
    immutable::ImmutableArtifactStore,
    manifest::SemanticImageRegion,
    publication::{open_published, open_published_semantic, publish_compiled, publish_semantic},
};
use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, RustEdition, Stage};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use thiserror::Error;

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
#[allow(
    clippy::large_enum_variant,
    reason = "focused fault tests retain exact cold publication facts without allocation or source erasure"
)]
enum TestError {
    #[error("test filesystem operation failed")]
    Io(#[from] io::Error),
    #[error("could not configure the bounded durable publisher")]
    Limits(#[from] server_journal::PublicationLimitError),
    #[error("could not create or reopen the durable publisher")]
    Publisher(#[from] server_journal::PublicationOpenError),
    #[error("could not shut down the durable publisher")]
    Shutdown(#[from] server_journal::ShutdownError),
    #[error("could not prepare a compact IR fixture")]
    Prepare(#[from] compiler_ir::PrepareError),
    #[error("could not write a compact IR fixture")]
    Write(#[from] compiler_ir::WriteError),
    #[error("could not build a complete semantic fixture image")]
    SemanticBuild(#[from] compiler_ir::BuildError),
    #[error("could not enter the semantic fixture package lineage")]
    Lineage(compiler_ir::PackageLineageFault),
    #[error("could not validate a compact IR fixture")]
    Fragment(#[from] compiler_ir::FragmentError),
    #[error("could not validate a stored compiler manifest")]
    Manifest(#[from] compiler_publication::manifest::CompilationManifestError),
    #[error("could not commit complete fixture fragment ranges")]
    Ranges(#[from] compiler_ir::FragmentRangeManifestError),
    #[error("could not reuse a stored fixture fragment")]
    Artifact(#[from] compiler_publication::immutable::ImmutableArtifactError),
    #[error("compiler publication failed")]
    Publish(#[from] PublishCompiledError),
    #[error("semantic compiler publication failed")]
    PublishSemantic(#[from] PublishSemanticError),
    #[error("compiler publication reopen failed")]
    Open(#[from] compiler_publication::publication::OpenPublishedError),
    #[error("opened compiler fragment could not reconstruct its exact immutable view")]
    OpenedFragment(#[from] OpenedFragmentError),
    #[error("opened semantic compiler artifact could not reconstruct its exact immutable views")]
    OpenedSemanticArtifact(#[from] OpenedSemanticArtifactError),
    #[error("fixture source length cannot fit source facts")]
    SourceLength(#[from] core::num::TryFromIntError),
    #[error("expected {expected}, observed {observed}")]
    Assertion {
        expected: &'static str,
        observed: &'static str,
    },
    #[error("opened compiler package did not contain fragment {ordinal}")]
    MissingOpenedFragment { ordinal: usize },
    #[error("opened compiler package contained more fragment views than its manifest")]
    ExtraOpenedFragment,
}

struct Fixture {
    directory: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> Result<Self, io::Error> {
        let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "compiler-publication-{label}-{}-{ordinal}",
            std::process::id()
        ));
        fs::create_dir(&directory)?;
        Ok(Self { directory })
    }

    fn journal(&self) -> PathBuf {
        self.directory.join("journal")
    }

    fn changed_journal(&self) -> PathBuf {
        self.directory.join("changed-journal")
    }

    fn artifacts(&self) -> PathBuf {
        self.directory.join("artifacts")
    }

    fn remove(self) -> Result<(), io::Error> {
        fs::remove_dir_all(self.directory)
    }
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "test retains exact publication terminal"
)]
fn package_manifest_is_order_invariant_and_unchanged_fragment_is_reused() -> Result<(), TestError> {
    let fixture = Fixture::new("order-and-reuse")?;
    let paths = PublicationPaths::in_directory(&fixture.journal());
    let publisher = DurablePublisher::create(&paths, limits()?)?;
    let mut alpha_bytes = [0_u8; 256];
    let mut bravo_bytes = [0_u8; 256];
    let mut changed_bytes = [0_u8; 256];
    let alpha_length = write_fragment(&mut alpha_bytes, b"alpha-source", b"alpha")?;
    let bravo_length = write_fragment(&mut bravo_bytes, b"bravo-source", b"bravo")?;
    let changed_length = write_fragment(&mut changed_bytes, b"bravo-source", b"bravo-changed")?;

    let first = publish(
        &publisher,
        &fixture.artifacts(),
        &[
            compiled(&bravo_bytes[..bravo_length])?,
            compiled(&alpha_bytes[..alpha_length])?,
        ],
        PublishControl::Continue,
    )?;
    let alpha_path = stored_path(
        &fixture.artifacts(),
        compiled(&alpha_bytes[..alpha_length])?,
    )?;
    let alpha_bytes_before = fs::read(&alpha_path)?;
    #[cfg(unix)]
    let alpha_inode = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&alpha_path)?);
    let second = publish(
        &publisher,
        &fixture.artifacts(),
        &[
            compiled(&alpha_bytes[..alpha_length])?,
            compiled(&bravo_bytes[..bravo_length])?,
        ],
        PublishControl::Continue,
    )?;
    if first.manifest != second.manifest {
        return Err(TestError::Assertion {
            expected: "permuted compiler inputs to retain manifest identity",
            observed: "manifest identity changed",
        });
    }
    assert_eq!(publisher.published()?, Some(second.publication));
    let changed_paths = PublicationPaths::in_directory(&fixture.changed_journal());
    let changed_publisher = DurablePublisher::create(&changed_paths, limits()?)?;
    let changed_first = publish(
        &changed_publisher,
        &fixture.artifacts(),
        &[
            compiled(&alpha_bytes[..alpha_length])?,
            compiled(&changed_bytes[..changed_length])?,
        ],
        PublishControl::Continue,
    )?;
    if changed_first.manifest.identity == first.manifest.identity {
        return Err(TestError::Assertion {
            expected: "changed compiler input to select a new manifest",
            observed: "manifest identity was reused",
        });
    }
    if alpha_bytes_before != fs::read(&alpha_path)? {
        return Err(TestError::Assertion {
            expected: "unchanged fragment bytes to remain unrewritten",
            observed: "unchanged fragment bytes changed",
        });
    }
    #[cfg(unix)]
    assert_eq!(
        alpha_inode,
        std::os::unix::fs::MetadataExt::ino(&fs::metadata(&alpha_path)?)
    );
    let opened =
        open_facts(&changed_publisher, &fixture.artifacts())?.ok_or(TestError::Assertion {
            expected: "a selected durable compiler package",
            observed: "no selected compiler package",
        })?;
    if opened.identity != changed_first.manifest.identity {
        return Err(TestError::Assertion {
            expected: "reopen to select the changed durable package",
            observed: "reopen selected another manifest",
        });
    }
    publisher.shutdown()?;
    let reopened = DurablePublisher::reopen(&paths, limits()?)?;
    let original = open_facts(&reopened, &fixture.artifacts())?.ok_or(TestError::Assertion {
        expected: "the original durable compiler package after journal reopen",
        observed: "no selected compiler package after journal reopen",
    })?;
    assert_eq!(original.identity, first.manifest.identity);
    reopened.shutdown()?;
    changed_publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "the integration path retains exact publication and reopened-fragment terminals"
)]
fn reopened_fragment_cursor_borrows_only_manifest_named_immutable_bytes() -> Result<(), TestError> {
    let fixture = Fixture::new("opened-fragment-views")?;
    let paths = PublicationPaths::in_directory(&fixture.journal());
    let publisher = DurablePublisher::create(&paths, limits()?)?;
    let mut alpha_bytes = [0_u8; 256];
    let mut bravo_bytes = [0_u8; 256];
    let alpha_length = write_fragment(&mut alpha_bytes, b"opened-alpha-source", b"opened-alpha")?;
    let bravo_length = write_fragment(&mut bravo_bytes, b"opened-bravo-source", b"opened-bravo")?;
    let alpha = compiled(&alpha_bytes[..alpha_length])?;
    let bravo = compiled(&bravo_bytes[..bravo_length])?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[
            compiled(&bravo_bytes[..bravo_length])?,
            compiled(&alpha_bytes[..alpha_length])?,
        ],
        PublishControl::Continue,
    )?;

    let mut manifest_output = [0_u8; 1024];
    let mut manifest_facts = [None; 2];
    let mut fragment_output = [0_u8; 512];
    let mut locality_output = [0_u8; 1024];
    let opened = open_published(
        &publisher,
        &fixture.artifacts(),
        OpenPublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            fragment_output: &mut fragment_output,
            locality_output: &mut locality_output,
        },
    )?
    .ok_or(TestError::Assertion {
        expected: "a selected durable compiler package",
        observed: "no selected compiler package",
    })?;
    let mut fragments = opened.fragments();
    let first = next_opened_fragment(&mut fragments, 0)?;
    let second = next_opened_fragment(&mut fragments, 1)?;
    if fragments.next().is_some() {
        return Err(TestError::ExtraOpenedFragment);
    }
    if first.facts.fragment >= second.facts.fragment {
        return Err(TestError::Assertion {
            expected: "strict canonical fragment identity order",
            observed: "non-increasing reopened fragment identities",
        });
    }
    assert_opened_fragment(&first, &alpha, &bravo)?;
    assert_opened_fragment(&second, &alpha, &bravo)?;
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "the integration path retains exact chained publication and reopen terminals"
)]
fn chained_publication_reopens_newest_and_preserves_generation_one_artifacts()
-> Result<(), TestError> {
    let fixture = Fixture::new("chained-publication")?;
    let paths = PublicationPaths::in_directory(&fixture.journal());
    let publisher = DurablePublisher::create(&paths, limits()?)?;
    let mut alpha_bytes = [0_u8; 256];
    let mut bravo_bytes = [0_u8; 256];
    let alpha_length = write_fragment(&mut alpha_bytes, b"chain-alpha-source", b"chain-alpha")?;
    let bravo_length = write_fragment(&mut bravo_bytes, b"chain-bravo-source", b"chain-bravo")?;
    let first_fragment = compiled(&alpha_bytes[..alpha_length])?;
    let first = publish(
        &publisher,
        &fixture.artifacts(),
        &[first_fragment],
        PublishControl::Continue,
    )?;
    let second = publish(
        &publisher,
        &fixture.artifacts(),
        &[
            compiled(&alpha_bytes[..alpha_length])?,
            compiled(&bravo_bytes[..bravo_length])?,
        ],
        PublishControl::Continue,
    )?;
    assert_ne!(first.publication.generation, second.publication.generation);
    assert_eq!(publisher.published()?, Some(second.publication));

    let mut manifest_output = [0_u8; 1024];
    let mut manifest_facts = [None; 2];
    let mut fragment_output = [0_u8; 1024];
    let mut locality_output = [0_u8; 1024];
    let opened = open_published(
        &publisher,
        &fixture.artifacts(),
        OpenPublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            fragment_output: &mut fragment_output,
            locality_output: &mut locality_output,
        },
    )?
    .ok_or(TestError::Assertion {
        expected: "the newest chained publication",
        observed: "no selected chained publication",
    })?;
    assert_eq!(opened.publication, second.publication);
    let mut fragments = opened.fragments();
    let opened_alpha = next_opened_fragment(&mut fragments, 0)?;
    let opened_bravo = next_opened_fragment(&mut fragments, 1)?;
    assert!(fragments.next().is_none());
    assert_opened_fragment(
        &opened_alpha,
        &compiled(&alpha_bytes[..alpha_length])?,
        &compiled(&bravo_bytes[..bravo_length])?,
    )?;
    assert_opened_fragment(
        &opened_bravo,
        &compiled(&alpha_bytes[..alpha_length])?,
        &compiled(&bravo_bytes[..bravo_length])?,
    )?;

    let manifest_path = fixture
        .artifacts()
        .join("manifests")
        .join(format!("{}.irmani", hex(first.manifest.identity.as_ref())));
    let manifest_bytes = fs::read(manifest_path)?;
    let mut first_manifest_facts = [None; 2];
    let first_manifest = compiler_publication::manifest::CompilationManifestView::validate(
        &manifest_bytes,
        &mut first_manifest_facts,
    )?;
    assert_eq!(*first_manifest, first.manifest);

    let binding_path = fixture.artifacts().join("bindings").join(format!(
        "{}-{}.binding",
        hex(first.binding.generation.pinned_root.as_ref()),
        hex(first.binding.generation.dep_set.as_ref())
    ));
    let binding_bytes = fs::read(binding_path)?;
    let binding =
        CompilationBindingView::validate(&binding_bytes).map_err(|_| TestError::Assertion {
            expected: "generation-one binding to validate",
            observed: "invalid generation-one binding",
        })?;
    assert_eq!(*binding, first.binding);
    let fragment_path = stored_path(
        &fixture.artifacts(),
        compiled(&alpha_bytes[..alpha_length])?,
    )?;
    let stored_fragment = fs::read(fragment_path)?;
    FragmentView::validate(&stored_fragment)?;
    assert_eq!(stored_fragment, &alpha_bytes[..alpha_length]);

    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "test retains exact publication terminal"
)]
fn pre_storage_and_pre_admission_cancellation_never_selects_a_generation() -> Result<(), TestError>
{
    let fixture = Fixture::new("cancellation")?;
    let paths = PublicationPaths::in_directory(&fixture.journal());
    let publisher = DurablePublisher::create(&paths, limits()?)?;
    let mut bytes = [0_u8; 256];
    let length = write_fragment(&mut bytes, b"cancel-source", b"cancel")?;
    let before_storage = publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(&bytes[..length])?],
        PublishControl::CancelBeforeStorage,
    );
    assert!(matches!(
        before_storage,
        Err(PublishCompiledError::CancelledBeforeStorage)
    ));
    assert!(!fixture.artifacts().exists());
    assert_eq!(publisher.published()?, None);

    let before_admission = publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(&bytes[..length])?],
        PublishControl::CancelBeforeAdmission,
    );
    assert!(matches!(
        before_admission,
        Err(PublishCompiledError::Uncommitted(
            UncommittedPublication::CancelledBeforeAdmission { .. }
        ))
    ));
    assert!(fixture.artifacts().join("fragments").is_dir());
    assert!(fixture.artifacts().join("manifests").is_dir());
    assert!(fixture.artifacts().join("bindings").is_dir());
    assert_eq!(publisher.published()?, None);
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "test retains exact publication terminal"
)]
fn admitted_cancellation_returns_cancelled_only_when_cancel_wins() -> Result<(), TestError> {
    let fixture = Fixture::new("admitted-cancellation")?;
    let paths = PublicationPaths::in_directory(&fixture.journal());
    let publisher = DurablePublisher::create(&paths, limits()?)?;
    let mut bytes = [0_u8; 256];
    let length = write_fragment(&mut bytes, b"cancel-race-source", b"cancel-race")?;
    match publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(&bytes[..length])?],
        PublishControl::CancelAfterAdmission,
    ) {
        Err(PublishCompiledError::Uncommitted(UncommittedPublication::Cancelled { .. })) => {
            assert_eq!(publisher.published()?, None);
        }
        Ok(published) => assert_eq!(publisher.published()?, Some(published.publication)),
        Err(error) => return Err(TestError::Publish(error)),
    }
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "the paired-publication test retains exact semantic corruption and cancellation terminals"
)]
fn semantic_publication_is_order_stable_and_rejects_corrupted_paired_image() -> Result<(), TestError>
{
    let fixture = Fixture::new("semantic-order-and-corruption")?;
    let paths = PublicationPaths::in_directory(&fixture.journal());
    let publisher = DurablePublisher::create(&paths, limits()?)?;
    let mut alpha_bytes = [0_u8; 256];
    let mut bravo_bytes = [0_u8; 256];
    let alpha_length = write_fragment(&mut alpha_bytes, b"semantic-alpha-source", b"alpha")?;
    let bravo_length = write_fragment(&mut bravo_bytes, b"semantic-bravo-source", b"bravo")?;
    let alpha = semantic_compiled(&alpha_bytes[..alpha_length])?;
    let bravo = semantic_compiled(&bravo_bytes[..bravo_length])?;

    let cancelled = publish_semantic_fixture(
        &publisher,
        &fixture.artifacts(),
        &[semantic_compiled(&alpha_bytes[..alpha_length])?],
        PublishControl::CancelBeforeStorage,
    );
    assert!(matches!(
        cancelled,
        Err(PublishSemanticError::CancelledBeforeStorage)
    ));
    assert!(!fixture.artifacts().exists());
    assert_eq!(publisher.published()?, None);

    let first = publish_semantic_fixture(
        &publisher,
        &fixture.artifacts(),
        &[bravo, alpha],
        PublishControl::Continue,
    )?;
    let second = publish_semantic_fixture(
        &publisher,
        &fixture.artifacts(),
        &[
            semantic_compiled(&alpha_bytes[..alpha_length])?,
            semantic_compiled(&bravo_bytes[..bravo_length])?,
        ],
        PublishControl::Continue,
    )?;
    assert_eq!(first.manifest, second.manifest);

    let mut compact_manifest_output = [0_u8; 1024];
    let mut compact_manifest_facts = [None; 2];
    let mut compact_fragment_output = [0_u8; 1024];
    let mut compact_locality_output = [0_u8; 1024];
    assert!(matches!(
        open_published(
            &publisher,
            &fixture.artifacts(),
            OpenPublicationScratch {
                manifest_output: &mut compact_manifest_output,
                manifest_facts: &mut compact_manifest_facts,
                fragment_output: &mut compact_fragment_output,
                locality_output: &mut compact_locality_output,
            },
        ),
        Err(
            compiler_publication::publication::OpenPublishedError::ManifestFormat {
                expected: compiler_publication::manifest::CompilationManifestFormat::CompactV1,
                observed: compiler_publication::manifest::CompilationManifestFormat::SemanticV2,
            }
        )
    ));

    let mut manifest_output = [0_u8; 1024];
    let mut manifest_facts = [None; 2];
    let mut fragment_output = [0_u8; 1024];
    let mut semantic_output = [0_u8; 16_384];
    let mut locality_output = [0_u8; 1024];
    let opened = open_published_semantic(
        &publisher,
        &fixture.artifacts(),
        OpenSemanticPublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            fragment_output: &mut fragment_output,
            semantic_image_output: &mut semantic_output,
            locality_output: &mut locality_output,
        },
    )?
    .ok_or(TestError::Assertion {
        expected: "a selected paired semantic publication",
        observed: "no selected paired semantic publication",
    })?;
    for artifact in opened.artifacts() {
        let artifact = artifact?;
        match artifact.semantic_image.image_facts().provenance {
            compiler_ir::ImageProvenance::Captured { source, recipe, .. } => {
                assert_eq!(source, artifact.fragment.view.source);
                assert_eq!(recipe, artifact.fragment.view.recipe);
            }
            compiler_ir::ImageProvenance::Unavailable => {
                return Err(TestError::Assertion {
                    expected: "captured semantic provenance paired with the compact artifact",
                    observed: "unavailable semantic provenance",
                });
            }
        }
    }

    let semantic_path = fs::read_dir(fixture.artifacts().join("semantic-images"))?
        .next()
        .ok_or(TestError::Assertion {
            expected: "one paired semantic image to corrupt",
            observed: "no paired semantic image",
        })??
        .path();
    fs::write(semantic_path, b"corrupt-semantic-image")?;
    let mut manifest_output = [0_u8; 1024];
    let mut manifest_facts = [None; 2];
    let mut fragment_output = [0_u8; 1024];
    let mut semantic_output = [0_u8; 16_384];
    let mut locality_output = [0_u8; 1024];
    assert!(matches!(
        open_published_semantic(
            &publisher,
            &fixture.artifacts(),
            OpenSemanticPublicationScratch {
                manifest_output: &mut manifest_output,
                manifest_facts: &mut manifest_facts,
                fragment_output: &mut fragment_output,
                semantic_image_output: &mut semantic_output,
                locality_output: &mut locality_output,
            },
        ),
        Err(
            compiler_publication::publication::OpenPublishedError::SemanticImage { ordinal: 0, .. }
        ) | Err(
            compiler_publication::publication::OpenPublishedError::SemanticImage { ordinal: 1, .. }
        )
    ));
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "test retains exact publication terminal"
)]
fn open_published_requires_the_selected_binding() -> Result<(), TestError> {
    let fixture = Fixture::new("open-binding")?;
    let paths = PublicationPaths::in_directory(&fixture.journal());
    let publisher = DurablePublisher::create(&paths, limits()?)?;
    assert!(open_facts(&publisher, &fixture.artifacts())?.is_none());
    let mut bytes = [0_u8; 256];
    let length = write_fragment(&mut bytes, b"open-source", b"open")?;
    publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(&bytes[..length])?],
        PublishControl::Continue,
    )?;
    fs::remove_dir_all(fixture.artifacts().join("bindings"))?;
    assert!(matches!(
        open_facts(&publisher, &fixture.artifacts()),
        Err(compiler_publication::publication::OpenPublishedError::MissingBinding { .. })
    ));
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "test retains exact publication terminal"
)]
fn open_published_rejects_missing_selected_manifest_and_fragment() -> Result<(), TestError> {
    let manifest_fixture = Fixture::new("open-manifest")?;
    let manifest_paths = PublicationPaths::in_directory(&manifest_fixture.journal());
    let manifest_publisher = DurablePublisher::create(&manifest_paths, limits()?)?;
    let mut manifest_bytes = [0_u8; 256];
    let manifest_length = write_fragment(&mut manifest_bytes, b"manifest-source", b"manifest")?;
    publish(
        &manifest_publisher,
        &manifest_fixture.artifacts(),
        &[compiled(&manifest_bytes[..manifest_length])?],
        PublishControl::Continue,
    )?;
    fs::remove_dir_all(manifest_fixture.artifacts().join("manifests"))?;
    assert!(matches!(
        open_facts(&manifest_publisher, &manifest_fixture.artifacts()),
        Err(compiler_publication::publication::OpenPublishedError::Manifest(_))
    ));
    manifest_publisher.shutdown()?;
    manifest_fixture.remove()?;

    let fragment_fixture = Fixture::new("open-fragment")?;
    let fragment_paths = PublicationPaths::in_directory(&fragment_fixture.journal());
    let fragment_publisher = DurablePublisher::create(&fragment_paths, limits()?)?;
    let mut fragment_bytes = [0_u8; 256];
    let fragment_length = write_fragment(&mut fragment_bytes, b"fragment-source", b"fragment")?;
    publish(
        &fragment_publisher,
        &fragment_fixture.artifacts(),
        &[compiled(&fragment_bytes[..fragment_length])?],
        PublishControl::Continue,
    )?;
    let fragment_path = stored_path(
        &fragment_fixture.artifacts(),
        compiled(&fragment_bytes[..fragment_length])?,
    )?;
    fs::write(&fragment_path, b"corrupt")?;
    assert!(matches!(
        open_facts(&fragment_publisher, &fragment_fixture.artifacts()),
        Err(compiler_publication::publication::OpenPublishedError::Fragment { .. })
    ));
    fs::remove_dir_all(fragment_fixture.artifacts().join("fragments"))?;
    assert!(matches!(
        open_facts(&fragment_publisher, &fragment_fixture.artifacts()),
        Err(compiler_publication::publication::OpenPublishedError::Fragment { .. })
    ));
    fragment_publisher.shutdown()?;
    fragment_fixture.remove()?;
    Ok(())
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "test retains exact publication terminal"
)]
fn open_published_rejects_manifest_source_fact_that_disagrees_with_fragment()
-> Result<(), TestError> {
    let fixture = Fixture::new("open-source-fact")?;
    let paths = PublicationPaths::in_directory(&fixture.journal());
    let publisher = DurablePublisher::create(&paths, limits()?)?;
    let mut bytes = [0_u8; 256];
    let length = write_fragment(&mut bytes, b"source-fact-source", b"source-fact")?;
    let published = publish(
        &publisher,
        &fixture.artifacts(),
        &[compiled(&bytes[..length])?],
        PublishControl::Continue,
    )?;
    let manifest_path = only_file(&fixture.artifacts().join("manifests"))?;
    let mut manifest = fs::read(manifest_path)?;
    let source_length = 16 + 32;
    manifest[source_length..source_length + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    let identity = backend_version::ArtifactId::<
        backend_version::IrManifestEncoding,
        backend_version::IrManifestDomain,
    >::from_encoded_bytes(&manifest);
    fs::write(
        fixture
            .artifacts()
            .join("manifests")
            .join(format!("{}.irmani", hex(identity.as_ref()))),
        &manifest,
    )?;
    let binding_path = only_file(&fixture.artifacts().join("bindings"))?;
    let mut binding = [0_u8; COMPILATION_BINDING_BYTES];
    let binding =
        CompilationBindingView::write_into(published.binding.generation, identity, &mut binding)
            .map_err(io::Error::other)?;
    fs::write(binding_path, binding.as_ref())?;
    match open_facts(&publisher, &fixture.artifacts()) {
        Err(compiler_publication::publication::OpenPublishedError::FragmentFacts {
            ordinal,
            expected,
            observed,
        }) => {
            assert_eq!(ordinal, 0);
            assert_eq!(expected.source.byte_len, u32::MAX);
            assert_eq!(observed.source, compiled(&bytes[..length])?.source);
            assert_eq!(expected.recipe, observed.recipe);
            assert_eq!(expected.ranges, observed.ranges);
        }
        _ => {
            return Err(TestError::Assertion {
                expected: "source-fact fragment mismatch",
                observed: "another reopen terminal",
            });
        }
    }
    publisher.shutdown()?;
    fixture.remove()?;
    Ok(())
}

#[allow(
    clippy::result_large_err,
    reason = "fixture retains exact publication terminal"
)]
fn publish<'fragment>(
    publisher: &DurablePublisher,
    artifacts: &Path,
    fragments: &[CompiledFragment<'fragment>],
    control: PublishControl<'_>,
) -> Result<compiler_publication::publication::PublishedCompilation, PublishCompiledError> {
    let mut manifest = [0_u8; 1024];
    let mut facts = [None; 2];
    let mut ordinals = [0_usize; 2];
    let mut locality = [0_u8; 1024];
    let mut binding = [0_u8; COMPILATION_BINDING_BYTES];
    publish_compiled(
        publisher,
        artifacts,
        fragments,
        control,
        PublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )
}

#[allow(
    clippy::result_large_err,
    reason = "fixture retains exact publication terminal"
)]
fn open_facts(
    publisher: &DurablePublisher,
    artifacts: &Path,
) -> Result<
    Option<compiler_publication::manifest::CompilationManifestFacts>,
    compiler_publication::publication::OpenPublishedError,
> {
    let mut manifest = [0_u8; 1024];
    let mut facts = [None; 2];
    let mut fragments = [0_u8; 512];
    let mut locality = [0_u8; 1024];
    let opened = open_published(
        publisher,
        artifacts,
        OpenPublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            fragment_output: &mut fragments,
            locality_output: &mut locality,
        },
    )?;
    Ok(opened.map(|opened| *opened.manifest))
}

#[allow(
    clippy::result_large_err,
    reason = "fixture retains exact publication terminal"
)]
fn write_fragment(
    output: &mut [u8; 256],
    source_bytes: &[u8],
    name: &[u8],
) -> Result<usize, TestError> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
        byte_len: u32::try_from(source_bytes.len())?,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"publication-test-toolchain"),
    );
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Constant,
    }];
    let nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
    let atoms = [AtomInput { bytes: name }];
    let prepared = PreparedFragment::prepare(source, recipe, &entities, &nodes, &atoms)?;
    let bytes = prepared.write_into(output)?;
    Ok(bytes.len())
}

#[allow(
    clippy::result_large_err,
    reason = "fixture retains exact publication terminal"
)]
fn compiled(bytes: &[u8]) -> Result<CompiledFragment<'_>, TestError> {
    let fragment = FragmentView::validate(bytes)?;
    Ok(CompiledFragment {
        source: fragment.source,
        recipe: fragment.recipe,
        fragment,
    })
}

#[allow(
    clippy::result_large_err,
    reason = "fixture retains one compact artifact and one independently owned semantic image"
)]
fn semantic_compiled(bytes: &[u8]) -> Result<CompiledSemantic<'_>, TestError> {
    let artifact = compiled(bytes)?;
    let lineage =
        PackageLineage::new("cargo", "publication-semantic-fixture").map_err(TestError::Lineage)?;
    let mut builder = IrBuilder::new();
    builder.set_image_provenance(artifact.source, artifact.recipe, lineage, "src/lib.rs")?;
    Ok(CompiledSemantic {
        artifact,
        ir: builder.finish()?,
    })
}

#[allow(
    clippy::result_large_err,
    reason = "fixture retains caller-owned semantic publication capacity and exact terminals"
)]
fn publish_semantic_fixture<'fragment>(
    publisher: &DurablePublisher,
    artifacts: &Path,
    compiled: &[CompiledSemantic<'fragment>],
    control: PublishControl<'_>,
) -> Result<compiler_publication::publication::PublishedCompilation, PublishSemanticError> {
    let mut manifest = [0_u8; 1024];
    let mut facts = [None; 2];
    let mut ordinals = [0_usize; 2];
    let mut image_plan = [SemanticImageRegion::EMPTY; 2];
    let mut semantic_output = [0_u8; 16_384];
    let mut locality = [0_u8; 1024];
    let mut binding = [0_u8; COMPILATION_BINDING_BYTES];
    publish_semantic(
        publisher,
        artifacts,
        compiled,
        control,
        SemanticPublicationScratch {
            manifest_output: &mut manifest,
            manifest_facts: &mut facts,
            ordinals: &mut ordinals,
            semantic_image_plan: &mut image_plan,
            semantic_image_output: &mut semantic_output,
            locality_output: &mut locality,
            binding_output: &mut binding,
        },
    )
}

#[allow(
    clippy::result_large_err,
    reason = "the integration assertion retains exact cold reopen corruption evidence without boxing"
)]
fn next_opened_fragment<'opened, 'fragment>(
    fragments: &mut OpenedFragmentCursor<'opened, 'fragment>,
    ordinal: usize,
) -> Result<OpenedFragment<'fragment>, TestError> {
    match fragments.next() {
        Some(fragment) => fragment.map_err(TestError::OpenedFragment),
        None => Err(TestError::MissingOpenedFragment { ordinal }),
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the integration assertion preserves exact durable and reopened-fragment failures"
)]
fn assert_opened_fragment(
    opened: &OpenedFragment<'_>,
    alpha: &CompiledFragment<'_>,
    bravo: &CompiledFragment<'_>,
) -> Result<(), TestError> {
    let expected = if opened.facts.fragment
        == FragmentRangeManifest::from_view(&alpha.fragment)?.fragment
    {
        alpha
    } else if opened.facts.fragment == FragmentRangeManifest::from_view(&bravo.fragment)?.fragment {
        bravo
    } else {
        return Err(TestError::Assertion {
            expected: "one of the two manifest-selected fragment identities",
            observed: "an identity outside the published compact fragments",
        });
    };
    if opened.view.as_ref() != expected.fragment.as_ref() {
        return Err(TestError::Assertion {
            expected: "reopened view bytes to equal the immutable manifest-named fragment",
            observed: "different reopened fragment bytes",
        });
    }
    if opened.view.source != expected.source || opened.view.recipe != expected.recipe {
        return Err(TestError::Assertion {
            expected: "reopened view facts to equal its exact compiler fragment",
            observed: "reopened source or recipe facts differed",
        });
    }
    Ok(())
}

fn limits() -> Result<PublicationLimits, server_journal::PublicationLimitError> {
    PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)
}

#[allow(
    clippy::result_large_err,
    reason = "fixture retains exact publication terminal"
)]
fn stored_path(directory: &Path, fragment: CompiledFragment<'_>) -> Result<PathBuf, TestError> {
    let range = FragmentRangeManifest::from_view(&fragment.fragment)?;
    let mut store = ImmutableArtifactStore::new(directory)?;
    Ok(store.ensure(&range, &fragment.fragment)?.path)
}

#[allow(
    clippy::result_large_err,
    reason = "fixture retains exact publication terminal"
)]
fn only_file(directory: &Path) -> Result<PathBuf, TestError> {
    let mut entries = fs::read_dir(directory)?;
    let Some(entry) = entries.next() else {
        return Err(TestError::Assertion {
            expected: "one immutable artifact",
            observed: "empty immutable artifact directory",
        });
    };
    let path = entry?.path();
    if entries.next().is_some() {
        return Err(TestError::Assertion {
            expected: "one immutable artifact",
            observed: "multiple immutable artifacts",
        });
    }
    Ok(path)
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(HEX[usize::from(*byte >> 4)] as char);
        text.push(HEX[usize::from(*byte & 0x0f)] as char);
    }
    text
}
