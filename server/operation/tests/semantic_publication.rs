//! Ordinary public journey from one authority transaction through durable semantic reopen,
//! discovery, canonical rendering, and exact/lexical index admission.

use core::{mem::MaybeUninit, num::NonZeroUsize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

use compiler_driver::{
    CompileControl, CompileOutput, CompileRequest, CompileScratch, NativeTool, ResolvedToolchain,
    SemanticAuthorityInput, ToolchainSelection, compile_semantic,
};
use compiler_ir::{
    AtomId, CanonicalTypeRenderLimits, ImageProvenance, SemanticCoreReader, SemanticEntity,
    SemanticImageCensus, SemanticImageDiscovery, SemanticImageFacts, SemanticReader,
    full_semantic_image_len, prepare_canonical_type,
};
use compiler_publication::{
    OpenSemanticPublicationScratch, PublishControl, SemanticPublicationScratch,
    binding::COMPILATION_BINDING_BYTES,
    manifest::{COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES, SemanticImageRegion},
    open_published_semantic, publish_semantic,
};
use compiler_vocabulary::{GoVersion, LanguageProfile, Stage};
use server_index_build::{
    IndexedType, SemanticIndexBuildScratch, SemanticTypeFact, build_semantic,
};
use server_index_core::{EntityArtifactIdentity, EntityDocumentId};
use server_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use sha2::{Digest, Sha256};
use thiserror::Error;

const SOURCE: &[u8] = b"package demo\nfunc Brew() {}\n";
const MANIFEST_BYTES: usize = 16 + COMPILATION_SEMANTIC_MANIFEST_ENTRY_BYTES;
const ROOT_WORK_BYTES: usize = 4_096;
const FRAGMENT_BYTES: usize = 65_536;
const TYPE_RENDER_BYTES: usize = 512;
static NEXT_JOURNEY: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Error)]
enum SemanticJourneyError {
    #[error("could not construct the typed Go toolchain fact")]
    Toolchain(#[from] compiler_driver::ToolchainResolutionError),
    #[error("could not create the semantic publication directory")]
    Create(#[source] std::io::Error),
    #[error(transparent)]
    Limits(#[from] server_journal::PublicationLimitError),
    #[error(transparent)]
    Publisher(#[from] server_journal::PublicationOpenError),
    #[error("semantic image measurement failed")]
    Measure(#[from] compiler_ir::SemanticImageEncodeError),
    #[error("the fused compiler output did not reach a stable semantic publication")]
    Publish(#[from] compiler_publication::PublishSemanticError),
    #[error("the stable semantic publication did not reopen")]
    Open(#[from] compiler_publication::OpenPublishedError),
    #[error("semantic discovery found an invalid pooled reference")]
    Discovery(#[from] compiler_ir::SemanticDiscoveryError),
    #[error("canonical semantic type rendering failed")]
    Render(#[from] compiler_ir::CanonicalTypeRenderError),
    #[error("the publisher did not shut down cleanly")]
    Shutdown(#[from] server_journal::ShutdownError),
    #[error("the stable publication contained no semantic package")]
    MissingPackage,
    #[error("the semantic package contained no artifact")]
    MissingArtifact,
    #[error("the semantic artifact cursor rejected its first paired artifact")]
    Artifact(#[from] compiler_publication::OpenedSemanticArtifactError),
    #[error("the semantic image contained no canonical declaration")]
    MissingEntity,
    #[error("the semantic declaration omitted its admitted type coordinate")]
    MissingType,
    #[error("the canonical type-render depth fixture was zero")]
    TypeDepth,
    #[error("the exact index key did not decode as a typed document authority")]
    Document(#[from] server_index_core::EntityDocumentIdError),
    #[error("owned and reopened semantic censuses diverged")]
    CensusMismatch {
        owned: SemanticImageCensus,
        reopened: SemanticImageCensus,
    },
    #[error("owned and reopened semantic entity counts diverged")]
    EntityCountMismatch { owned: usize, reopened: usize },
    #[error("owned and reopened semantic image authority diverged")]
    ImageAuthorityMismatch {
        owned: SemanticImageFacts,
        reopened: SemanticImageFacts,
    },
    #[error("owned and reopened intrinsic semantic entity row {ordinal} diverged")]
    EntityIntrinsicMismatch {
        ordinal: usize,
        owned: SemanticEntity,
        reopened: SemanticEntity,
    },
    #[error("semantic entity row {ordinal} named an absent atom")]
    MissingEntityName { ordinal: usize, atom: AtomId },
    #[error("owned and reopened semantic entity name row {ordinal} diverged")]
    EntityNameMismatch {
        ordinal: usize,
        owned: Vec<u8>,
        reopened: Vec<u8>,
    },
    #[error("owned and reopened canonical type rendering diverged")]
    TypeRenderMismatch { owned: String, reopened: String },
    #[error("semantic index output was not bound to the reopened image")]
    IndexMismatch,
}

#[test]
#[allow(
    clippy::result_large_err,
    reason = "the one public journey retains exact owned publication, reopen, and render causes"
)]
fn authority_truth_survives_publication_reopen_discovery_render_and_index()
-> Result<(), SemanticJourneyError> {
    let authority = go_authority_image(SOURCE);
    let toolchain = ResolvedToolchain::from_version(
        NativeTool::GoCompiler,
        Path::new("/usr/bin/true"),
        b"semantic-publication-go-authority",
    )?;
    let cancelled = AtomicBool::new(false);
    let mut diagnostic = [];
    let mut fragment_output = [0xa5_u8; FRAGMENT_BYTES];
    let compiled = match compile_semantic(
        CompileRequest {
            profile: LanguageProfile::Go(GoVersion::Go125),
            stage: Stage::LowerIr,
            source: SOURCE,
            declaration_scope: compiler_driver::DeclarationScope::fixture(),
            toolchain: ToolchainSelection::ResolvedNative(toolchain),
            authority: SemanticAuthorityInput::Go { image: &authority },
            control: CompileControl {
                deadline: Instant::now() + Duration::from_secs(2),
                cancelled: &cancelled,
            },
        },
        CompileScratch {
            diagnostic_output: &mut diagnostic,
            native_work: Path::new("/private/tmp"),
        },
        CompileOutput {
            fragment_output: &mut fragment_output,
        },
    ) {
        Ok(compiled) => compiled,
        Err(cause) => panic!("configured Go authority did not compile: {cause:?}"),
    };
    let owned_census = SemanticImageDiscovery::new(&compiled.ir).census()?;
    let semantic_length = full_semantic_image_len(&compiled.ir)?;
    let fragment_length = compiled.artifact.fragment.as_ref().len();

    let directory = JourneyDirectory::create()?;
    let paths = PublicationPaths::in_directory(&directory.path.join("durable"));
    let limits = PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?;
    let publisher = DurablePublisher::create(&paths, limits)?;
    let artifacts = directory.path.join("artifacts");
    let inputs = [compiled];
    let mut manifest_output = [0_u8; MANIFEST_BYTES];
    let mut manifest_facts = [None; 1];
    let mut ordinals = [0_usize; 1];
    let mut semantic_image_plan = [SemanticImageRegion::EMPTY; 1];
    let mut semantic_output = vec![0_u8; semantic_length];
    let mut locality_output = [0_u8; ROOT_WORK_BYTES];
    let mut binding_output = [0_u8; COMPILATION_BINDING_BYTES];
    let published = publish_semantic(
        &publisher,
        &artifacts,
        &inputs,
        PublishControl::Continue,
        SemanticPublicationScratch {
            manifest_output: &mut manifest_output,
            manifest_facts: &mut manifest_facts,
            ordinals: &mut ordinals,
            semantic_image_plan: &mut semantic_image_plan,
            semantic_image_output: &mut semantic_output,
            locality_output: &mut locality_output,
            binding_output: &mut binding_output,
        },
    )?;
    assert_eq!(
        published.publication.generation,
        published.binding.generation
    );
    publisher.shutdown()?;

    let reopened_publisher = DurablePublisher::reopen(&paths, limits)?;
    let mut reopened_manifest = [0_u8; MANIFEST_BYTES];
    let mut reopened_facts = [None; 1];
    let mut reopened_fragment = vec![0_u8; fragment_length];
    let mut reopened_semantic = vec![0_u8; semantic_length];
    let mut reopened_locality = [0_u8; ROOT_WORK_BYTES];
    let opened = open_published_semantic(
        &reopened_publisher,
        &artifacts,
        OpenSemanticPublicationScratch {
            manifest_output: &mut reopened_manifest,
            manifest_facts: &mut reopened_facts,
            fragment_output: &mut reopened_fragment,
            semantic_image_output: &mut reopened_semantic,
            locality_output: &mut reopened_locality,
        },
    )?
    .ok_or(SemanticJourneyError::MissingPackage)?;
    let mut reopened_artifacts = opened.artifacts();
    let artifact = reopened_artifacts
        .next()
        .ok_or(SemanticJourneyError::MissingArtifact)??;
    assert!(reopened_artifacts.next().is_none());

    let mut reopened_census = SemanticImageDiscovery::new(&artifact.semantic_image).census()?;
    if !same_image_authority(owned_census.image, reopened_census.image) {
        return Err(SemanticJourneyError::ImageAuthorityMismatch {
            owned: owned_census.image,
            reopened: reopened_census.image,
        });
    }
    reopened_census.image = owned_census.image;
    let owned_entities: Vec<_> = inputs[0].ir.canonical_entities().collect();
    let reopened_entities: Vec<_> = artifact.semantic_image.canonical_entities().collect();
    if reopened_census != owned_census {
        return Err(SemanticJourneyError::CensusMismatch {
            owned: owned_census,
            reopened: reopened_census,
        });
    }
    if reopened_entities.len() != owned_entities.len() {
        return Err(SemanticJourneyError::EntityCountMismatch {
            owned: owned_entities.len(),
            reopened: reopened_entities.len(),
        });
    }
    for (ordinal, (owned, reopened)) in owned_entities
        .iter()
        .copied()
        .zip(reopened_entities.iter().copied())
        .enumerate()
    {
        if owned.version != reopened.version
            || owned.kind != reopened.kind
            || owned.visibility != reopened.visibility
            || owned.authority != reopened.authority
            || owned.source != reopened.source
            || owned.parent.is_some() != reopened.parent.is_some()
            || owned.semantic_type.is_some() != reopened.semantic_type.is_some()
        {
            return Err(SemanticJourneyError::EntityIntrinsicMismatch {
                ordinal,
                owned,
                reopened,
            });
        }
        let owned_name =
            inputs[0]
                .ir
                .atom(owned.name)
                .ok_or(SemanticJourneyError::MissingEntityName {
                    ordinal,
                    atom: owned.name,
                })?;
        let reopened_name = artifact.semantic_image.atom(reopened.name).ok_or(
            SemanticJourneyError::MissingEntityName {
                ordinal,
                atom: reopened.name,
            },
        )?;
        if owned_name != reopened_name {
            return Err(SemanticJourneyError::EntityNameMismatch {
                ordinal,
                owned: owned_name.to_vec(),
                reopened: reopened_name.to_vec(),
            });
        }
    }
    let entity = reopened_entities
        .first()
        .copied()
        .ok_or(SemanticJourneyError::MissingEntity)?;
    let semantic_type = entity
        .semantic_type
        .ok_or(SemanticJourneyError::MissingType)?;
    let owned_semantic_type = owned_entities[0]
        .semantic_type
        .ok_or(SemanticJourneyError::MissingType)?;
    let limits = CanonicalTypeRenderLimits::new(
        NonZeroUsize::new(64).ok_or(SemanticJourneyError::TypeDepth)?,
    );
    let owned_render = prepare_canonical_type(&inputs[0].ir, owned_semantic_type, limits)?;
    let reopened_render = prepare_canonical_type(&artifact.semantic_image, semantic_type, limits)?;
    let mut owned_type = [0_u8; TYPE_RENDER_BYTES];
    let mut reopened_type = [0_u8; TYPE_RENDER_BYTES];
    let owned_type = owned_render.write_into(&mut owned_type)?;
    let reopened_type = reopened_render.write_into(&mut reopened_type)?;
    if owned_type != reopened_type {
        return Err(SemanticJourneyError::TypeRenderMismatch {
            owned: owned_type.to_owned(),
            reopened: reopened_type.to_owned(),
        });
    }

    let mut entity_output = [const { MaybeUninit::uninit() }; 4];
    let mut exact_output = [const { MaybeUninit::uninit() }; 4];
    let mut lexical_output = [const { MaybeUninit::uninit() }; 4];
    let index = match build_semantic(
        &artifact,
        SemanticIndexBuildScratch {
            entities: &mut entity_output,
            exact_rows: &mut exact_output,
            lexical_rows: &mut lexical_output,
        },
    ) {
        Ok(index) => index,
        Err(cause) => panic!("reopened semantic index admission failed: {cause:?}"),
    };
    let indexed = index
        .entities
        .first()
        .ok_or(SemanticJourneyError::MissingEntity)?;
    let document = EntityDocumentId::try_from(indexed.exact_key.as_ref())?;
    let exact_value = match indexed.exact_value.semantic_view() {
        Ok(value) => value,
        Err(cause) => panic!("semantic exact value did not decode: {cause:?}"),
    };
    let Some(image) = artifact.fragment.facts.semantic_image else {
        return Err(SemanticJourneyError::IndexMismatch);
    };
    let semantic_class = artifact
        .semantic_image
        .ty(semantic_type)
        .ok_or(SemanticJourneyError::MissingType)?
        .tag();
    if document.artifact != EntityArtifactIdentity::Semantic(image.identity)
        || document.entity != indexed.entity
        || exact_value.semantic_type
            != IndexedType::Semantic(Some(SemanticTypeFact {
                coordinate: semantic_type,
                class: semantic_class,
            }))
        || indexed.name != b"Brew"
    {
        return Err(SemanticJourneyError::IndexMismatch);
    }
    reopened_publisher.shutdown()?;
    Ok(())
}

fn same_image_authority(owned: SemanticImageFacts, reopened: SemanticImageFacts) -> bool {
    if owned.authority != reopened.authority {
        return false;
    }
    match (owned.provenance, reopened.provenance) {
        (ImageProvenance::Unavailable, ImageProvenance::Unavailable) => true,
        (
            ImageProvenance::Captured {
                source: owned_source,
                recipe: owned_recipe,
                claim: owned_claim,
                ..
            },
            ImageProvenance::Captured {
                source: reopened_source,
                recipe: reopened_recipe,
                claim: reopened_claim,
                ..
            },
        ) => {
            owned_source == reopened_source
                && owned_recipe == reopened_recipe
                && owned_claim == reopened_claim
        }
        _ => false,
    }
}

/// Minimal fully checksummed format-v5 Go authority image for `SOURCE`.
fn go_authority_image(source: &[u8]) -> Vec<u8> {
    const HEADER: usize = 136;
    const DECLARATION_AT: usize = HEADER;
    const PACKAGE_AT: usize = DECLARATION_AT + 56;
    const ATOMS_AT: usize = PACKAGE_AT + 28;
    const BODY: usize = ATOMS_AT + 8 - HEADER;
    const DIGEST_DOMAIN: &[u8] = b"nudox.go.authority.image.sha256.v5\0";
    const NAMES: &[u8] = b"demoBrew";
    const NONE: u32 = u32::MAX;

    let mut image = vec![0_u8; HEADER + BODY];
    image[..4].copy_from_slice(b"NGAI");
    image[4..6].copy_from_slice(&5_u16.to_le_bytes());
    image[6..8].copy_from_slice(&(HEADER as u16).to_le_bytes());
    image[8..12].copy_from_slice(&1_u32.to_le_bytes());
    image[12..16].copy_from_slice(&(NAMES.len() as u32).to_le_bytes());
    image[16..20].copy_from_slice(&(BODY as u32).to_le_bytes());
    image[20..52].copy_from_slice(Sha256::digest(source).as_slice());
    image[116..120].copy_from_slice(&0_u32.to_le_bytes());
    image[120..124].copy_from_slice(&1_u32.to_le_bytes());

    image[PACKAGE_AT..PACKAGE_AT + 4].copy_from_slice(&0_u32.to_le_bytes());
    image[PACKAGE_AT + 4..PACKAGE_AT + 8].copy_from_slice(&4_u32.to_le_bytes());
    image[PACKAGE_AT + 8..PACKAGE_AT + 12].copy_from_slice(&0_u32.to_le_bytes());
    image[PACKAGE_AT + 12..PACKAGE_AT + 16].copy_from_slice(&4_u32.to_le_bytes());
    image[PACKAGE_AT + 24..PACKAGE_AT + 28].copy_from_slice(&0_u32.to_le_bytes());

    image[DECLARATION_AT] = 3;
    image[DECLARATION_AT + 1] = 1;
    image[DECLARATION_AT + 4..DECLARATION_AT + 8].copy_from_slice(&4_u32.to_le_bytes());
    image[DECLARATION_AT + 8..DECLARATION_AT + 12].copy_from_slice(&4_u32.to_le_bytes());
    image[DECLARATION_AT + 16..DECLARATION_AT + 20].copy_from_slice(&4_u32.to_le_bytes());
    image[DECLARATION_AT + 20..DECLARATION_AT + 24].copy_from_slice(&NONE.to_le_bytes());
    image[DECLARATION_AT + 24..DECLARATION_AT + 28].copy_from_slice(&NONE.to_le_bytes());
    image[DECLARATION_AT + 28..DECLARATION_AT + 32].copy_from_slice(&NONE.to_le_bytes());
    image[ATOMS_AT..].copy_from_slice(NAMES);

    let mut digest = Sha256::new();
    digest.update(DIGEST_DOMAIN);
    digest.update(&image[..52]);
    digest.update(&image[84..HEADER]);
    digest.update(&image[HEADER..]);
    image[52..84].copy_from_slice(digest.finalize().as_slice());
    image
}

struct JourneyDirectory {
    path: PathBuf,
}

impl JourneyDirectory {
    fn create() -> Result<Self, SemanticJourneyError> {
        let ordinal = NEXT_JOURNEY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "nudox-semantic-publication-{}-{ordinal}",
            std::process::id()
        ));
        fs::create_dir(&path).map_err(SemanticJourneyError::Create)?;
        Ok(Self { path })
    }
}

impl Drop for JourneyDirectory {
    fn drop(&mut self) {
        let _removed = fs::remove_dir_all(&self.path);
    }
}
