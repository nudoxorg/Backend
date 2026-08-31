use std::{
    fs, io,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use nudox_compile_driver::CompiledFragment;
use nudox_compile_publication::binding::{COMPILATION_BINDING_BYTES, CompilationBindingView};
use nudox_compile_publication::{
    OpenPublicationScratch, OpenedFragment, OpenedFragmentCursor, OpenedFragmentError,
    PublicationScratch, PublishCompiledError, PublishControl, UncommittedPublication,
    immutable::ImmutableArtifactStore,
    publication::{open_published, publish_compiled},
};
use nudox_compile_vocab::{CompileRecipeFact, Language, NativeTool, Stage};
use nudox_durable_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use nudox_id::{ContentId, SourceFactDomain, ToolchainDomain};
use nudox_ir_format::{
    AtomInput, EntityKind, EntityRecord, FragmentRangeManifest, FragmentView, PreparedFragment,
    PrimitiveType, SourceIdentity, TypeNode,
};
use nudox_ir_vocab::{AtomId, TypeId};
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
    Limits(#[from] nudox_durable_journal::PublicationLimitError),
    #[error("could not create or reopen the durable publisher")]
    Publisher(#[from] nudox_durable_journal::PublicationOpenError),
    #[error("could not shut down the durable publisher")]
    Shutdown(#[from] nudox_durable_journal::ShutdownError),
    #[error("could not prepare a compact IR fixture")]
    Prepare(#[from] nudox_ir_format::PrepareError),
    #[error("could not write a compact IR fixture")]
    Write(#[from] nudox_ir_format::WriteError),
    #[error("could not validate a compact IR fixture")]
    Fragment(#[from] nudox_ir_format::FragmentError),
    #[error("could not commit complete fixture fragment ranges")]
    Ranges(#[from] nudox_ir_format::FragmentRangeManifestError),
    #[error("could not reuse a stored fixture fragment")]
    Artifact(#[from] nudox_compile_publication::immutable::ImmutableArtifactError),
    #[error("compiler publication failed")]
    Publish(#[from] PublishCompiledError),
    #[error("compiler publication reopen failed")]
    Open(#[from] nudox_compile_publication::publication::OpenPublishedError),
    #[error("opened compiler fragment could not reconstruct its exact immutable view")]
    OpenedFragment(#[from] OpenedFragmentError),
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
            "nudox-compile-publication-{label}-{}-{ordinal}",
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
    let rejected_change = publish(
        &publisher,
        &fixture.artifacts(),
        &[
            compiled(&alpha_bytes[..alpha_length])?,
            compiled(&changed_bytes[..changed_length])?,
        ],
        PublishControl::Continue,
    );
    match rejected_change {
        Err(PublishCompiledError::Uncommitted(UncommittedPublication::Failed {
            attempted,
            source,
        })) => {
            assert_ne!(attempted.generation, second.publication.generation);
            match &*source {
                nudox_durable_journal::PublicationFailure::Conflict { facts } => {
                    assert_eq!(
                        facts.expected_root,
                        *attempted.generation.pinned_root.as_ref()
                    );
                    assert_eq!(
                        facts.expected_dep_set,
                        *attempted.generation.dep_set.as_ref()
                    );
                    assert_eq!(
                        facts.observed_root,
                        *second.publication.generation.pinned_root.as_ref()
                    );
                    assert_eq!(
                        facts.observed_dep_set,
                        *second.publication.generation.dep_set.as_ref()
                    );
                }
                _ => {
                    return Err(TestError::Assertion {
                        expected: "exact durable conflict source",
                        observed: "another durable failure source",
                    });
                }
            }
        }
        _ => {
            return Err(TestError::Assertion {
                expected: "uncommitted durable conflict",
                observed: "another publication terminal",
            });
        }
    }
    assert_eq!(publisher.published()?, Some(second.publication));
    let changed_paths = PublicationPaths::in_directory(&fixture.changed_journal());
    let changed_publisher = DurablePublisher::create(&changed_paths, limits()?)?;
    let third = publish(
        &changed_publisher,
        &fixture.artifacts(),
        &[
            compiled(&alpha_bytes[..alpha_length])?,
            compiled(&changed_bytes[..changed_length])?,
        ],
        PublishControl::Continue,
    )?;
    if third.manifest.identity == first.manifest.identity {
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
    if opened.identity != third.manifest.identity {
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
        Err(nudox_compile_publication::publication::OpenPublishedError::MissingBinding { .. })
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
        Err(nudox_compile_publication::publication::OpenPublishedError::Manifest(_))
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
        Err(nudox_compile_publication::publication::OpenPublishedError::Fragment { .. })
    ));
    fs::remove_dir_all(fragment_fixture.artifacts().join("fragments"))?;
    assert!(matches!(
        open_facts(&fragment_publisher, &fragment_fixture.artifacts()),
        Err(nudox_compile_publication::publication::OpenPublishedError::Fragment { .. })
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
    let identity = nudox_id::ArtifactId::<nudox_id::IrManifestEncoding, nudox_id::IrManifestDomain>::from_encoded_bytes(&manifest);
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
        Err(nudox_compile_publication::publication::OpenPublishedError::FragmentFacts {
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
) -> Result<nudox_compile_publication::publication::PublishedCompilation, PublishCompiledError> {
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
    Option<nudox_compile_publication::manifest::CompilationManifestFacts>,
    nudox_compile_publication::publication::OpenPublishedError,
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
        Language::Rust,
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

fn limits() -> Result<PublicationLimits, nudox_durable_journal::PublicationLimitError> {
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
