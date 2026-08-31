#![cfg(feature = "mmap")]

use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
};

use nudox_compile_vocab::{CompileRecipeFact, Language, NativeTool, Stage};
use nudox_id::{ContentId, SourceFactDomain, ToolchainDomain};
use nudox_ir_format::{
    AtomInput, EntityKind, EntityRecord, FragmentRangeManifest, FragmentRangeVerifyError,
    FragmentView, MappedFragment, MappedFragmentError, MappedFragmentIoPhase, PreparedFragment,
    PrimitiveType, SourceIdentity, TypeNode, open_fragment_mmap,
};
use nudox_ir_vocab::{AtomId, TypeId};
use thiserror::Error;

static FIXTURE_SEQUENCE: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
enum TestFailure {
    #[error("could not create mapped-fragment fixture")]
    Create(#[source] std::io::Error),
    #[error("could not write mapped-fragment fixture")]
    Write(#[source] std::io::Error),
    #[error("could not sync mapped-fragment fixture")]
    Sync(#[source] std::io::Error),
    #[error("could not unlink mapped-fragment fixture")]
    Unlink(#[source] std::io::Error),
    #[error(transparent)]
    Prepare(#[from] nudox_ir_format::PrepareError),
    #[error(transparent)]
    Fragment(#[from] nudox_ir_format::FragmentError),
    #[error(transparent)]
    Manifest(#[from] nudox_ir_format::FragmentRangeManifestError),
    #[error(transparent)]
    WriteFragment(#[from] nudox_ir_format::WriteError),
    #[error(transparent)]
    Map(#[from] MappedFragmentError),
    #[error("fixture source length {actual} does not fit the compact source fact")]
    SourceLength {
        actual: usize,
        #[source]
        source: core::num::TryFromIntError,
    },
}

struct TemporaryFixture {
    path: PathBuf,
}

impl TemporaryFixture {
    fn create(law: &str, bytes: &[u8]) -> Result<Self, TestFailure> {
        let sequence = FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nudox-mapped-fragment-{law}-{}-{sequence}",
            std::process::id()
        ));
        let mut file = File::create(&path).map_err(TestFailure::Create)?;
        file.write_all(bytes).map_err(TestFailure::Write)?;
        file.sync_all().map_err(TestFailure::Sync)?;
        Ok(Self { path })
    }
}

impl Drop for TemporaryFixture {
    fn drop(&mut self) {
        let _removed = fs::remove_file(&self.path);
    }
}

fn fragment() -> Result<([u8; 256], usize, FragmentRangeManifest), TestFailure> {
    let source_bytes = b"mapped-fragment-source";
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source_bytes),
        byte_len: u32::try_from(source_bytes.len()).map_err(|source| {
            TestFailure::SourceLength {
                actual: source_bytes.len(),
                source,
            }
        })?,
    };
    let recipe = CompileRecipeFact::derive(
        Language::Rust,
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"mapped-fragment-toolchain"),
    );
    let entities = [EntityRecord {
        semantic_type: TypeId::new(0),
        name: AtomId::new(0),
        kind: EntityKind::Constant,
    }];
    let nodes = [TypeNode::Primitive(PrimitiveType::Bool)];
    let atoms = [AtomInput { bytes: b"alpha" }];
    let prepared = PreparedFragment::prepare(source, recipe, &entities, &nodes, &atoms)?;
    let mut bytes = [0; 256];
    let length = prepared.write_into(&mut bytes)?.len();
    let view = FragmentView::validate(&bytes[..length])?;
    let manifest = FragmentRangeManifest::from_view(&view)?;
    Ok((bytes, length, manifest))
}

fn open_owned(
    path: &Path,
    manifest: FragmentRangeManifest,
) -> Result<MappedFragment, MappedFragmentError> {
    // SAFETY: each fixture has a unique pathname and this test owns every descriptor that can
    // mutate it. The test only unlinks after mapping, never changes the inode while mapped, and
    // drops the mapping before the fixture owner is dropped on platforms that disallow unlinking.
    unsafe { open_fragment_mmap(path, manifest) }
}

#[test]
fn mapped_owner_validates_once_then_reuses_the_same_borrowed_bytes()
-> Result<(), TestFailure> {
    let (bytes, length, manifest) = fragment()?;
    let fixture = TemporaryFixture::create("same-pointer", &bytes[..length])?;
    let mapped = open_owned(&fixture.path, manifest)?;
    let first = mapped.view();
    let second = mapped.view();
    assert_eq!(mapped.manifest, manifest);
    assert_eq!(mapped.mapped_bytes, length);
    assert_eq!(first.as_ref().as_ptr(), mapped.as_ref().as_ptr());
    assert_eq!(first.as_ref().as_ptr(), second.as_ref().as_ptr());
    assert_eq!(first.atoms().next().map(|atom| atom.bytes), Some(&b"alpha"[..]));

    #[cfg(unix)]
    {
        fs::remove_file(&fixture.path).map_err(TestFailure::Unlink)?;
        assert_eq!(mapped.view().entities().count(), 1);
    }
    #[cfg(not(unix))]
    {
        drop(mapped);
    }
    Ok(())
}

#[test]
fn corrupt_truncated_empty_and_missing_files_fail_before_owner_construction()
-> Result<(), TestFailure> {
    let (bytes, length, manifest) = fragment()?;
    let mut corrupt = bytes;
    corrupt[length - 1] ^= 1;
    let corrupt_fixture = TemporaryFixture::create("corrupt", &corrupt[..length])?;
    assert!(matches!(
        open_owned(&corrupt_fixture.path, manifest),
        Err(MappedFragmentError::Validation(
            FragmentRangeVerifyError::FragmentIdentity { expected, observed }
        )) if expected == manifest.fragment && observed != expected
    ));

    let truncated_fixture = TemporaryFixture::create("truncated", &bytes[..length - 1])?;
    assert!(matches!(
        open_owned(&truncated_fixture.path, manifest),
        Err(MappedFragmentError::Validation(
            FragmentRangeVerifyError::FragmentLength { expected, observed }
        )) if expected == manifest.fragment_length && observed + 1 == expected
    ));

    let empty_fixture = TemporaryFixture::create("empty", &[])?;
    assert!(matches!(
        open_owned(&empty_fixture.path, manifest),
        Err(MappedFragmentError::EmptyFile)
    ));

    let missing = std::env::temp_dir().join(format!(
        "nudox-mapped-fragment-missing-{}-{}",
        std::process::id(),
        FIXTURE_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    assert!(matches!(
        open_owned(&missing, manifest),
        Err(MappedFragmentError::Io {
            phase: MappedFragmentIoPhase::Open,
            source,
        }) if source.kind() == std::io::ErrorKind::NotFound
    ));
    Ok(())
}
