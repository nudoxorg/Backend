//! Proves that a package source frontier becomes one reopened semantic generation.

use std::{
    fs, num::NonZeroUsize, path::PathBuf, process::Command, sync::atomic::AtomicBool,
    time::Duration,
};

use backend_engine::application::{
    CompilerPackageTargetV2, DocumentationSession, LocalCompiler, LocalCompilerClient,
    LocalCompilerConfig, LocalCompilerControl, LocalCompilerRuntimeConfiguration,
    LocalCompilerRuntimePaths, LocalCompilerScratch, LocalCompilerTimeout,
    LocalRuntimePackageAuthority, LocalRuntimeRustAuthority, LocalRuntimeToolchain,
    LocalToolchainSet, OwnedPackageSource, OwnedPackageSourceSet, PackageSource, PackageSourceSet,
    StagedEmbeddingStatus,
};
use backend_engine::driver::{ResolvedToolchain, ToolchainSelection};
use backend_frontend_rust::legacy::{RustToolchain, SourceByteLimit};
use backend_library::interface::{
    CorrelationId, GenerateTarget, PackageCompilePhase, PackageCompileRequest, PackageUrl,
};
use backend_semantic::ir::{
    CanonicalPlaneStreamError, CanonicalSemanticPlaneSegmentRef, CanonicalSemanticPlaneSegmentSink,
    CoreDeclarationRows, MAX_SEMANTIC_SEGMENT_BYTES, SemanticImageView, SemanticIrPlane,
    SemanticPlaneKind, SemanticPlaneManifest, SemanticPlaneRecordError, SemanticSegmentId,
    reset_semantic_image_validations, semantic_image_validations, stream_canonical_plane_family,
};
use backend_semantic::vocabulary::{CStandard, LanguageProfile, NativeTool, Stage};
use backend_store::journal::PublicationLimits;

#[derive(Default)]
struct SegmentInventory {
    ids: Vec<SemanticSegmentId>,
    output_bytes: u64,
    maximum_payload_bytes: usize,
}

impl CanonicalSemanticPlaneSegmentSink for SegmentInventory {
    type Error = SemanticPlaneRecordError;

    fn write_segment(
        &mut self,
        segment: CanonicalSemanticPlaneSegmentRef<'_>,
    ) -> Result<(), Self::Error> {
        let descriptor = segment.metadata()?;
        let id = descriptor
            .admitted_id()
            .ok_or(SemanticPlaneRecordError::MissingAdmittedId)?;
        self.ids
            .try_reserve(1)
            .map_err(SemanticPlaneRecordError::Allocation)?;
        self.ids.push(id);
        self.output_bytes = self
            .output_bytes
            .checked_add(
                u64::try_from(segment.bytes().len())
                    .map_err(|_| SemanticPlaneRecordError::MetricsOverflow)?,
            )
            .ok_or(SemanticPlaneRecordError::MetricsOverflow)?;
        self.maximum_payload_bytes = self.maximum_payload_bytes.max(segment.bytes().len());
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("test sink stopped accepting semantic segments")]
struct SinkStopped;

struct StoppedSink;

impl CanonicalSemanticPlaneSegmentSink for StoppedSink {
    type Error = SinkStopped;

    fn write_segment(
        &mut self,
        _segment: CanonicalSemanticPlaneSegmentRef<'_>,
    ) -> Result<(), Self::Error> {
        Err(SinkStopped)
    }
}

#[test]
fn two_sources_publish_as_one_reopened_package_generation() -> Result<(), Box<dyn std::error::Error>>
{
    let clang = find_clang().ok_or("clang is required for the package semantic journey")?;
    let version = Command::new(&clang).arg("--version").output()?;
    if !version.status.success() {
        return Err("clang version probe failed".into());
    }
    let root = unique_directory()?;
    let package_root = root.join("package");
    let artifacts = root.join("artifacts");
    let journal = root.join("journal");
    let native_work = root.join("native-work");
    fs::create_dir_all(package_root.join("src"))?;
    fs::create_dir(&native_work)?;
    let first = "int alpha(void) { return 1; }\n";
    let second = "int beta(void) { return 2; }\n";
    fs::write(package_root.join("src/alpha.c"), first)?;
    fs::write(package_root.join("src/beta.c"), second)?;

    let toolchains = [ToolchainSelection::ResolvedNative(
        ResolvedToolchain::from_version(NativeTool::Clang, &clang, &version.stdout)?,
    )];
    let selected = LocalToolchainSet::validate(&toolchains)?;
    let cancelled = AtomicBool::new(false);
    let mut scratch = LocalCompilerScratch::with_fragment_capacity(
        NonZeroUsize::new(16 * 1024 * 1024).ok_or("fragment capacity is zero")?,
    )?;
    let mut compiler = LocalCompiler::create(
        LocalCompilerConfig {
            toolchains: selected,
            artifact_directory: &artifacts,
            journal_directory: &journal,
            native_work_directory: &native_work,
            control: LocalCompilerControl {
                timeout: LocalCompilerTimeout::new(Duration::from_secs(30))?,
                cancelled: &cancelled,
            },
        },
        PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?,
        &mut scratch,
    )?;
    let package_url = PackageUrl::try_from("pkg:generic/sample@1.0.0".to_owned())
        .map_err(|error| fixture_error(format!("package URL rejected: {error:?}")))?;
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(31),
            profile: LanguageProfile::C(CStandard::C23),
            stage: Stage::LowerIr,
        },
        package_url,
    )
    .map_err(|error| fixture_error(format!("package profile rejected: {error:?}")))?;
    let sources = [
        PackageSource::new("src/alpha.c", first)?,
        PackageSource::new("src/beta.c", second)?,
    ];
    let package = PackageSourceSet::new(&request, &package_root, &sources)?;
    let mut staged_phases = Vec::new();
    let staged = compiler
        .compile_package_sources_staged(package.clone(), &mut |phase| staged_phases.push(phase))?;
    assert_eq!(
        staged.embedding_status(),
        StagedEmbeddingStatus::NotConfigured
    );

    assert_eq!(staged.output_object_count(), 5);
    assert_eq!(staged.generation_facts(), staged.binding_facts().generation);
    assert_eq!(
        staged.manifest_facts().identity,
        staged.binding_facts().manifest
    );
    assert_eq!(
        staged.output_object(0).unwrap().bytes(),
        staged.manifest_bytes()
    );
    assert!(
        !artifacts.exists(),
        "staging must not create local immutable artifacts"
    );
    assert_eq!(
        staged_phases,
        [
            PackageCompilePhase::Authority,
            PackageCompilePhase::Lower,
            PackageCompilePhase::Authority,
            PackageCompilePhase::Lower,
            PackageCompilePhase::Publish,
        ]
    );

    let mut phases = Vec::new();
    let published = compiler.compile_package_sources(package, &mut |phase| phases.push(phase))?;

    assert_eq!(published.publication.manifest.fragment_count, 2);
    assert_eq!(published.images.len(), 2);
    assert_eq!(
        staged.generation_facts(),
        published.publication.binding.generation
    );
    assert_eq!(staged.manifest_facts(), published.publication.manifest);
    assert_eq!(staged.binding_facts(), published.publication.binding);
    assert_eq!(
        staged.output_object(2).unwrap().bytes(),
        published.images[0].as_ref()
    );
    assert_eq!(
        staged.output_object(4).unwrap().bytes(),
        published.images[1].as_ref()
    );
    assert_eq!(
        phases,
        [
            PackageCompilePhase::Authority,
            PackageCompilePhase::Lower,
            PackageCompilePhase::Authority,
            PackageCompilePhase::Lower,
            PackageCompilePhase::Publish,
            PackageCompilePhase::Reopen,
        ]
    );
    let mut names = Vec::new();
    for image in &published.images {
        let view = SemanticImageView::reopen(image.as_ref())?;
        let session = DocumentationSession::new(&view);
        for entity in session.canonical_entities() {
            names.push(entity?.name.to_vec());
        }
    }
    assert!(names.iter().any(|name| name.as_slice() == b"alpha"));
    assert!(names.iter().any(|name| name.as_slice() == b"beta"));
    assert!(
        published
            .publication
            .publication
            .immutable
            .checksum
            .iter()
            .any(|byte| *byte != 0)
    );

    compiler.shutdown()?;
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn rust_package_staging_keeps_detached_sources_out_of_artifact_and_coverage_accounting()
-> Result<(), Box<dyn std::error::Error>> {
    let rustc = std::env::var_os("RUSTC").map_or_else(|| PathBuf::from("rustc"), PathBuf::from);
    let rust_toolchain = RustToolchain::discover(rustc)?;
    let version = Command::new(&rust_toolchain.tool)
        .arg("--version")
        .output()?;
    if !version.status.success() {
        return Err("rustc version probe failed".into());
    }
    let root = unique_directory()?;
    let package_root = root.join("package");
    let native_work = root.join("native-work");
    fs::create_dir_all(package_root.join("src"))?;
    fs::create_dir(&native_work)?;
    fs::write(
        package_root.join("Cargo.toml"),
        "[package]\nname = \"package_scope_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )?;
    let library =
        "pub mod gated;\npub mod sibling;\npub fn active_root() -> u32 { sibling::value() }\n";
    let gated = "#[cfg(any())] pub fn hidden() {}\n";
    let sibling = "pub fn value() -> u32 { 42 }\n";
    let detached = "pub fn detached() -> u32 { 0 }\n";
    fs::write(package_root.join("src/lib.rs"), library)?;
    fs::write(package_root.join("src/gated.rs"), gated)?;
    fs::write(package_root.join("src/sibling.rs"), sibling)?;
    fs::write(package_root.join("src/detached.rs"), detached)?;

    let package_url = PackageUrl::try_from("pkg:cargo/package-scope-fixture@0.1.0".to_owned())
        .map_err(|error| fixture_error(format!("package URL rejected: {error:?}")))?;
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(34),
            profile: LanguageProfile::Rust(backend_semantic::vocabulary::RustEdition::Rust2021),
            stage: Stage::LowerIr,
        },
        package_url,
    )
    .map_err(|error| fixture_error(format!("package profile rejected: {error:?}")))?;
    let authority = LocalRuntimeRustAuthority {
        toolchain: rust_toolchain.clone(),
        maximum_source_bytes: SourceByteLimit::from(8 * 1024),
        all_features: false,
        no_default_features: false,
        features: Box::new([]),
    };
    let configuration = LocalCompilerRuntimeConfiguration::new(
        LocalCompilerRuntimePaths::new(root.join("artifacts"), root.join("journal"), native_work)?,
        vec![LocalRuntimeToolchain::resolved(
            NativeTool::Rustc,
            rust_toolchain.tool.clone(),
            &version.stdout,
        )?]
        .into_boxed_slice(),
        Box::new([]),
        LocalRuntimePackageAuthority {
            rust: Some(authority),
            ..LocalRuntimePackageAuthority::default()
        },
        LocalCompilerTimeout::new(Duration::from_secs(180))?,
        PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?,
        LocalCompilerScratch::with_fragment_capacity(
            NonZeroUsize::new(16 * 1024 * 1024).ok_or("fragment capacity is zero")?,
        )?,
    )?;
    let client = LocalCompilerClient::start(configuration)?;
    let sources = vec![
        OwnedPackageSource::new("src/detached.rs", detached)?,
        OwnedPackageSource::new("src/gated.rs", gated)?,
        OwnedPackageSource::new("src/lib.rs", library)?,
        OwnedPackageSource::new("src/sibling.rs", sibling)?,
    ]
    .into_boxed_slice();
    let staged = client.compile_package_sources_staged(OwnedPackageSourceSet::new(
        request.clone(),
        package_root.clone(),
        sources.clone(),
    )?)?;
    assert_eq!(staged.artifacts().len(), 3);
    assert_eq!(staged.coverage_gaps().len(), 1);
    assert_eq!(staged.coverage_gaps()[0].relative_path(), "src/detached.rs");
    assert_eq!(
        staged.coverage_gaps()[0].source().byte_len as usize,
        detached.len()
    );
    assert_eq!(
        staged.input_witness().coverage().state(),
        backend_version::Coverage::Partial
    );
    assert_eq!(staged.output_object_count(), 7);
    assert_eq!(staged.versioned_planes()?.artifacts().len(), 3);
    assert_eq!(
        staged.embedding_status(),
        StagedEmbeddingStatus::NotConfigured
    );

    let published = client.compile_package_sources(OwnedPackageSourceSet::new(
        request,
        package_root.clone(),
        sources,
    )?)?;
    assert_eq!(published.images.len(), 3);
    assert_eq!(published.coverage_gaps().len(), 1);
    assert_eq!(
        published.coverage_gaps()[0].relative_path(),
        "src/detached.rs"
    );
    drop(client);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn owned_runtime_frontier_reaches_the_package_publication_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let clang = find_clang().ok_or("clang is required for the package runtime journey")?;
    let version = Command::new(&clang).arg("--version").output()?;
    if !version.status.success() {
        return Err("clang version probe failed".into());
    }
    let root = unique_directory()?;
    let package_root = root.join("package");
    let native_work = root.join("native-work");
    fs::create_dir_all(package_root.join("src"))?;
    fs::create_dir(&native_work)?;
    let package_url = PackageUrl::try_from("pkg:generic/sample@1.0.0".to_owned())
        .map_err(|error| fixture_error(format!("package URL rejected: {error:?}")))?;
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(32),
            profile: LanguageProfile::C(CStandard::C23),
            stage: Stage::LowerIr,
        },
        package_url,
    )
    .map_err(|error| fixture_error(format!("package profile rejected: {error:?}")))?;
    let configuration = LocalCompilerRuntimeConfiguration::new(
        LocalCompilerRuntimePaths::new(root.join("artifacts"), root.join("journal"), native_work)?,
        vec![LocalRuntimeToolchain::resolved(
            NativeTool::Clang,
            clang,
            &version.stdout,
        )?]
        .into_boxed_slice(),
        Box::new([]),
        LocalRuntimePackageAuthority::default(),
        LocalCompilerTimeout::new(Duration::from_secs(30))?,
        PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?,
        LocalCompilerScratch::with_fragment_capacity(
            NonZeroUsize::new(16 * 1024 * 1024).ok_or("fragment capacity is zero")?,
        )?,
    )?;
    let client = LocalCompilerClient::start(configuration)?;
    // Clang package authority opens the exact translation unit beneath the
    // package root (ce74f843e), so each owned source is also staged on disk.
    let alpha = "int alpha(void) { return 1; }\n";
    let beta = "int beta(void) { return 2; }\n";
    let gamma = "int gamma(void) { return 3; }\n";
    fs::write(package_root.join("src/alpha.c"), alpha)?;
    fs::write(package_root.join("src/beta.c"), beta)?;
    fs::create_dir_all(root.join("replacement/src"))?;
    fs::write(root.join("replacement/src/gamma.c"), gamma)?;
    let sources = vec![
        OwnedPackageSource::new("src/alpha.c", alpha)?,
        OwnedPackageSource::new("src/beta.c", beta)?,
    ]
    .into_boxed_slice();
    let staged = client.compile_package_sources_staged(OwnedPackageSourceSet::new(
        request.clone(),
        package_root.clone(),
        sources.clone(),
    )?)?;
    let package_target = CompilerPackageTargetV2::for_package(request.as_ref().clone());
    let runtime_identity = client
        .execution_identity_for_unit(
            &package_target,
            request.target.profile,
            request.target.stage,
        )
        .ok_or("opened Clang capability must expose its exact runtime identity")?;
    let first_image = staged
        .semantic_output_object(0)
        .ok_or("first staged semantic image is present")?;
    let image_pointer = first_image.bytes().as_ptr();
    let image_length = first_image.bytes().len();
    reset_semantic_image_validations();
    let (stream_metrics, inventory, reader_metrics) =
        staged.with_semantic_reader(0, |reader, metrics| {
            assert_eq!(
                reader.as_ref().as_ptr(),
                image_pointer,
                "reader borrows the exact staged NXFI allocation"
            );
            assert_eq!(reader.as_ref().len(), image_length);
            let mut sink = SegmentInventory::default();
            let streamed = stream_canonical_plane_family(
                reader,
                &CoreDeclarationRows,
                staged.input_witness(),
                MAX_SEMANTIC_SEGMENT_BYTES,
                &mut sink,
            )?;
            Ok::<_, CanonicalPlaneStreamError<SemanticPlaneRecordError>>((
                streamed, sink, metrics,
            ))
        })?;
    assert_eq!(reader_metrics.image_validation_count(), 1);
    assert_eq!(
        reader_metrics.full_image_validation_input_bytes(),
        u64::try_from(image_length)?
    );
    assert_eq!(reader_metrics.canonical_image_copy_bytes(), 0);
    assert_eq!(semantic_image_validations(), 1);
    assert_eq!(inventory.output_bytes, stream_metrics.output_bytes());
    assert_eq!(
        inventory.ids.len(),
        usize::try_from(stream_metrics.segment_count())?
    );
    assert!(inventory.maximum_payload_bytes <= MAX_SEMANTIC_SEGMENT_BYTES);
    assert!(
        stream_metrics.peak_segment_scratch_capacity_bytes()
            <= u64::try_from(MAX_SEMANTIC_SEGMENT_BYTES)?
    );
    assert!(
        stream_metrics.peak_row_scratch_capacity_bytes()
            <= u64::try_from(MAX_SEMANTIC_SEGMENT_BYTES)?
    );
    assert!(stream_metrics.row_index_capacity_bytes() > 0);
    let peak_scratch_capacity = stream_metrics
        .row_index_capacity_bytes()
        .checked_add(stream_metrics.peak_row_scratch_capacity_bytes())
        .and_then(|bytes| bytes.checked_add(stream_metrics.peak_segment_scratch_capacity_bytes()))
        .ok_or("stream scratch counter fits u64")?;
    assert!(peak_scratch_capacity > 0);
    assert_eq!(
        stream_metrics.row_count(),
        stream_metrics.row_encode_calls()
    );
    // The streaming callback is the complete backpressure/transaction boundary:
    // a sink error returns before any result-envelope or selected-root operation.
    let failed_stream = staged.with_semantic_reader(0, |reader, _| {
        let mut sink = StoppedSink;
        stream_canonical_plane_family(
            reader,
            &CoreDeclarationRows,
            staged.input_witness(),
            MAX_SEMANTIC_SEGMENT_BYTES,
            &mut sink,
        )
    });
    assert!(matches!(
        failed_stream,
        Err(
            backend_engine::application::StagedSemanticReaderError::Callback(
                CanonicalPlaneStreamError::Sink(SinkStopped)
            )
        )
    ));

    let planes = staged.versioned_planes()?;
    assert_eq!(planes.artifacts().len(), 2);
    for (ordinal, artifact_planes) in planes.artifacts().iter().enumerate() {
        assert_eq!(artifact_planes.artifact_ordinal(), ordinal);
        let manifest = SemanticPlaneManifest::decode(artifact_planes.manifest_bytes())?;
        assert_eq!(
            manifest.semantic_generation(),
            staged
                .semantic_vcs_generation(ordinal)
                .ok_or("staged image generation is present")?
        );
        assert_eq!(
            manifest.build().target(),
            runtime_identity.target().as_ref()
        );
        assert_eq!(
            manifest.build().environment(),
            &runtime_identity.environment_identity()
        );
        assert_eq!(
            manifest.input().coverage().state(),
            backend_version::Coverage::Partial
        );
        let core = manifest
            .plane(SemanticPlaneKind::Ir(SemanticIrPlane::Core))
            .ok_or("one Core IR plane is present per semantic image")?;
        assert_eq!(artifact_planes.segment_count(), core.segments().len());
        let mut payload_bytes = 0_usize;
        for segment_ordinal in 0..artifact_planes.segment_count() {
            let staged_segment = artifact_planes
                .segment(segment_ordinal)
                .ok_or("each declared plane segment has a payload")?;
            assert_eq!(
                staged_segment.kind(),
                SemanticPlaneKind::Ir(SemanticIrPlane::Core)
            );
            assert!(staged_segment.payload().len() <= MAX_SEMANTIC_SEGMENT_BYTES);
            assert_eq!(
                core.segments()[segment_ordinal]
                    .admit(staged_segment.kind(), staged_segment.payload(),)?,
                staged_segment.id()
            );
            payload_bytes = payload_bytes
                .checked_add(staged_segment.payload().len())
                .ok_or("plane payload byte count is representable")?;
        }
        assert_eq!(
            payload_bytes,
            staged
                .semantic_output_object(ordinal)
                .ok_or("image object exists")?
                .bytes()
                .len()
        );
    }
    let published = client.compile_package_sources(OwnedPackageSourceSet::new(
        request.clone(),
        package_root,
        sources,
    )?)?;

    assert_eq!(published.publication.manifest.fragment_count, 2);
    assert_eq!(published.images.len(), 2);
    let replacement = client.compile_package_sources(OwnedPackageSourceSet::new(
        request,
        root.join("replacement"),
        vec![OwnedPackageSource::new("src/gamma.c", gamma)?].into_boxed_slice(),
    )?)?;
    assert_ne!(
        replacement.publication.binding.generation,
        published.publication.binding.generation
    );
    let activated = client.activate_semantic_generation(
        LanguageProfile::C(CStandard::C23),
        published.publication.manifest,
        published.publication.binding,
    )?;
    assert_eq!(activated.manifest, published.publication.manifest);
    assert_eq!(activated.binding, published.publication.binding);
    assert_eq!(activated.images.len(), 2);
    drop(client);
    fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn staged_package_emits_a_typed_embedding_plane_with_borrowed_payloads()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{
        num::{NonZeroU16, NonZeroU32},
        os::unix::fs::PermissionsExt,
        sync::Arc,
    };

    use backend_compile::{
        EmbeddingArtifact, EmbeddingExecutable, EmbeddingNormalization, EmbeddingRuntimeSpecV1,
        ProcessEnvironment, ProcessLimits, ToolchainArtifact,
    };

    use backend_engine::application::{EmbeddingProvisioningFailure, EmbeddingRequirement};

    let clang = find_clang().ok_or("clang is required for the embedding-plane journey")?;
    let version = Command::new(&clang).arg("--version").output()?;
    if !version.status.success() {
        return Err("clang version probe failed".into());
    }
    let root = unique_directory()?;
    let package_root = root.join("package");
    let native_work = root.join("native-work");
    let embedding_workspace = root.join("embedding-work");
    fs::create_dir_all(package_root.join("src"))?;
    fs::create_dir(&native_work)?;
    fs::create_dir(&embedding_workspace)?;
    fs::set_permissions(&embedding_workspace, fs::Permissions::from_mode(0o700))?;
    let relative_path = "src/alpha.c";
    let source = "int alpha(void) { return 1; }\n";
    fs::write(package_root.join(relative_path), source)?;

    let model_bytes = br#"{"<unk>":[1.0,0.0],"alpha":[1.0,0.0],"int":[0.0,1.0]}"#;
    let tokenizer_bytes = br#"{"lowercase":true,"split":"whitespace"}"#;
    let hex = |bytes: &[u8]| {
        bytes.iter().fold(String::new(), |mut output, byte| {
            output.push_str(&format!("{byte:02x}"));
            output
        })
    };
    let embedding_program = embedding_workspace.join("embedder.py");
    let script = format!(
        r#"#!/usr/bin/env python3
import json, math, os, struct, sys
EXPECTED_MODEL = bytes.fromhex("{}")
EXPECTED_TOKENIZER = bytes.fromhex("{}")
EXPECTED_MODEL_ID = "{}"
EXPECTED_TOKENIZER_ID = "{}"
model_bytes = open(os.environ["BACKEND_EMBEDDING_MODEL_FILE"], "rb").read()
tokenizer_bytes = open(os.environ["BACKEND_EMBEDDING_TOKENIZER_FILE"], "rb").read()
if model_bytes != EXPECTED_MODEL or tokenizer_bytes != EXPECTED_TOKENIZER:
    sys.exit(71)
frame = sys.stdin.buffer.read()
if len(frame) < 108 or frame[:4] != b"BEM1":
    sys.exit(72)
if frame[40:72].hex() != EXPECTED_MODEL_ID or frame[72:104].hex() != EXPECTED_TOKENIZER_ID:
    sys.exit(73)
text_length = struct.unpack(">I", frame[104:108])[0]
text = frame[108:]
if len(text) != text_length:
    sys.exit(74)
dimension = struct.unpack(">H", frame[6:8])[0]
model = json.loads(model_bytes)
tokenizer = json.loads(tokenizer_bytes)
text = text.decode("utf-8")
if tokenizer.get("lowercase"):
    text = text.lower()
tokens = text.split()
if not tokens:
    sys.exit(75)
vectors = [model.get(token, model["<unk>"]) for token in tokens]
if any(len(vector) != dimension for vector in vectors):
    sys.exit(76)
values = [sum(vector[index] for vector in vectors) / len(vectors) for index in range(dimension)]
if frame[5] == 1:
    norm = math.sqrt(sum(value * value for value in values))
    if norm == 0:
        sys.exit(77)
    values = [value / norm for value in values]
sys.stdout.buffer.write(b"BEC1" + struct.pack(">H", dimension) + struct.pack("<" + "f" * dimension, *values))
"#,
        hex(model_bytes),
        hex(tokenizer_bytes),
        blake3::hash(model_bytes).to_hex(),
        blake3::hash(tokenizer_bytes).to_hex(),
    );
    fs::write(&embedding_program, script)?;
    fs::set_permissions(&embedding_program, fs::Permissions::from_mode(0o700))?;
    let embedding_tool = ToolchainArtifact::from_path(&embedding_program, Vec::new())?;
    let model = EmbeddingArtifact::new(Arc::from(&model_bytes[..]));
    let tokenizer = EmbeddingArtifact::new(Arc::from(&tokenizer_bytes[..]));
    let embedding_spec = EmbeddingRuntimeSpecV1::new(
        model.identity().as_bytes(),
        model.identity().as_bytes(),
        tokenizer.identity().as_bytes(),
        embedding_tool.identity().to_bytes(),
        NonZeroU16::new(2).ok_or("embedding dimension is zero")?,
        EmbeddingNormalization::L2,
        NonZeroU32::new(256).ok_or("text bound is zero")?,
        [77; 32],
    );
    let embedding = EmbeddingExecutable::activate_with_spec(
        embedding_spec,
        embedding_program,
        Vec::new(),
        embedding_workspace.clone(),
        ProcessEnvironment::new(vec![("PATH".into(), "/usr/bin:/bin".into())])?,
        ProcessLimits::new(64, 64, Duration::from_secs(2), 128)?.with_input_bytes_limit(512)?,
        embedding_tool,
        model,
        tokenizer,
    )?;

    let package_url = PackageUrl::try_from("pkg:generic/embedding@1.0.0".to_owned())
        .map_err(|error| fixture_error(format!("package URL rejected: {error:?}")))?;
    let request = PackageCompileRequest::new(
        GenerateTarget {
            correlation: CorrelationId(33),
            profile: LanguageProfile::C(CStandard::C23),
            stage: Stage::LowerIr,
        },
        package_url,
    )
    .map_err(|error| fixture_error(format!("package profile rejected: {error:?}")))?;
    let configuration = LocalCompilerRuntimeConfiguration::new(
        LocalCompilerRuntimePaths::new(root.join("artifacts"), root.join("journal"), native_work)?,
        vec![LocalRuntimeToolchain::resolved(
            NativeTool::Clang,
            clang.clone(),
            &version.stdout,
        )?]
        .into_boxed_slice(),
        Box::new([]),
        LocalRuntimePackageAuthority::default(),
        LocalCompilerTimeout::new(Duration::from_secs(30))?,
        PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?,
        LocalCompilerScratch::with_fragment_capacity(
            NonZeroUsize::new(16 * 1024 * 1024).ok_or("fragment capacity is zero")?,
        )?,
    )?
    .with_embedding_runtime(Arc::new(embedding), EmbeddingRequirement::Required);
    let client = LocalCompilerClient::start(configuration)?;
    let staged = client.compile_package_sources_staged(OwnedPackageSourceSet::new(
        request.clone(),
        package_root.clone(),
        vec![OwnedPackageSource::new(relative_path, source)?].into_boxed_slice(),
    )?)?;
    let planes = staged.versioned_planes()?;
    let StagedEmbeddingStatus::Available { identity } = planes.embedding_status() else {
        return Err("required embedding output did not report availability".into());
    };
    let artifact = planes
        .artifacts()
        .first()
        .ok_or("one canonical output artifact is present")?;
    let manifest = SemanticPlaneManifest::decode(artifact.manifest_bytes())?;
    let embedding_kind = SemanticPlaneKind::Embeddings(identity);
    let embedding_plane = manifest
        .plane(embedding_kind)
        .ok_or("embedding plane is in the exact staged manifest")?;
    assert_eq!(embedding_plane.segments().len(), 1);
    let segment = artifact
        .segment(artifact.segment_count() - 1)
        .ok_or("embedding segment is present")?;
    assert_eq!(segment.kind(), embedding_kind);
    assert_eq!(&segment.payload()[..6], b"BVE1\0\x02");
    assert_eq!(segment.payload().len(), 14);
    assert_eq!(
        embedding_plane.segments()[0].admit(segment.kind(), segment.payload())?,
        segment.id()
    );
    assert_eq!(
        *identity.model(),
        EmbeddingArtifact::new(Arc::from(&b"model-v1"[..]))
            .identity()
            .as_bytes()
    );
    assert_eq!(
        *identity.tokenizer(),
        EmbeddingArtifact::new(Arc::from(&b"tokenizer-v1"[..]))
            .identity()
            .as_bytes()
    );
    assert_eq!(identity.dimension(), 2);
    assert_eq!(identity.recipe(), &embedding_spec.recipe_identity());

    drop(client);

    let unavailable_native_work = root.join("unavailable-native-work");
    fs::create_dir(&unavailable_native_work)?;
    let unavailable_configuration = LocalCompilerRuntimeConfiguration::new(
        LocalCompilerRuntimePaths::new(
            root.join("unavailable-artifacts"),
            root.join("unavailable-journal"),
            unavailable_native_work,
        )?,
        vec![LocalRuntimeToolchain::resolved(
            NativeTool::Clang,
            clang,
            &version.stdout,
        )?]
        .into_boxed_slice(),
        Box::new([]),
        LocalRuntimePackageAuthority::default(),
        LocalCompilerTimeout::new(Duration::from_secs(30))?,
        PublicationLimits::new(NonZeroUsize::MIN, NonZeroUsize::MIN)?,
        LocalCompilerScratch::with_fragment_capacity(
            NonZeroUsize::new(16 * 1024 * 1024).ok_or("fragment capacity is zero")?,
        )?,
    )?
    .with_embedding_provisioning_failure(
        EmbeddingProvisioningFailure::ArtifactIdentityMismatch,
        EmbeddingRequirement::Optional,
    );
    let unavailable_client = LocalCompilerClient::start(unavailable_configuration)?;
    let unavailable_staged =
        unavailable_client.compile_package_sources_staged(OwnedPackageSourceSet::new(
            request,
            package_root,
            vec![OwnedPackageSource::new(relative_path, source)?].into_boxed_slice(),
        )?)?;
    let unavailable_planes = unavailable_staged.versioned_planes()?;
    assert_eq!(
        unavailable_planes.embedding_status(),
        StagedEmbeddingStatus::ProvisioningUnavailable {
            cause: EmbeddingProvisioningFailure::ArtifactIdentityMismatch,
        }
    );
    drop(unavailable_client);
    fs::remove_dir_all(root)?;
    Ok(())
}

fn fixture_error(message: String) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}

fn find_clang() -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|paths| std::env::split_paths(&paths).collect::<Vec<_>>())
        .map(|directory| directory.join("clang"))
        .find(|candidate| candidate.is_file())
        .and_then(|candidate| candidate.canonicalize().ok())
}

fn unique_directory() -> Result<PathBuf, std::io::Error> {
    for ordinal in 0_u16..64 {
        let path = std::env::temp_dir().join(format!(
            "compiler-package-batch-{}-{ordinal}",
            std::process::id()
        ));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "package batch fixture capacity exhausted",
    ))
}
