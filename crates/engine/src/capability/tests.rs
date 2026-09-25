use super::*;
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Copy)]
struct Accept;
impl<K: CapabilityKind> CapabilityManifestVerifier<K> for Accept {
    type Error = &'static str;
    fn verify(&self, _: &UntrustedCapabilityManifest<K>, _: &[u8]) -> Result<(), Self::Error> {
        Ok(())
    }
}

impl<K: CapabilityKind> CapabilityAcquisitionVerifier<K> for Accept {
    type Error = &'static str;
    fn verify(
        &self,
        _: &CapabilityManifest<K>,
        _: &AcquiredCapabilityArtifact,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

fn acquired<K: CapabilityKind>(
    available: AvailableCapability<K>,
    object: Arc<TypedObject>,
) -> VerifiedAcquisition<K> {
    let authority = authority();
    match available.verify_acquisition(
        AcquiredCapabilityArtifact::new(object, authority.source, authority.authorization),
        &Accept,
    ) {
        Ok(acquired) => acquired,
        Err(error) => panic!("fixture acquisition must verify: {error:?}"),
    }
}

fn host() -> CapabilityHost {
    CapabilityHost {
        os: OperatingSystem::Linux,
        architecture: Architecture::X86_64,
        managed: &[ManagedRuntime::Onnx],
        protocol_abi: 1,
    }
}

fn authority() -> ArtifactAuthority {
    ArtifactAuthority {
        source: ArtifactSourceId::new([1; 32]),
        authorization: Some(AuthorizationScopeId::new([2; 32])),
        trust_root: TrustRootId::new([3; 32]),
    }
}

fn model_recipe(dimensions: u16) -> EmbeddingModelRecipe {
    EmbeddingModelRecipe::new(
        TokenizerId::new([4; 32]),
        SourceExtraction::SemanticDeclaration,
        Chunking {
            maximum_tokens: NonZeroU32::new(128).expect("nonzero"),
            overlap_tokens: 16,
        },
        Pooling::Mean,
        Normalization::L2,
        QueryTreatment::ModelPrefix(TreatmentId::new([5; 32])),
        DocumentTreatment::ModelPrefix(TreatmentId::new([6; 32])),
        NonZeroU16::new(dimensions).expect("nonzero"),
        NumericRepresentation::F32,
        EmbeddingMetric::Cosine,
    )
}

fn model_manifest(bytes: &[u8]) -> UntrustedCapabilityManifest<EmbeddingModel> {
    UntrustedCapabilityManifest {
        artifact: ObjectVersion::from_value(bytes),
        bytes: NonZeroU64::new(bytes.len() as u64).expect("nonempty fixture"),
        authority: authority(),
        target: CapabilityTarget::Managed(ManagedRuntime::Onnx),
        protocol_abi: 1,
        dependencies: Vec::new(),
        recipe: model_recipe(384),
    }
}

static PROBES: AtomicUsize = AtomicUsize::new(0);
static REVOKES: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct ModelRuntime {
    dimensions: NonZeroU16,
}
impl CapabilityRuntime<EmbeddingModel> for ModelRuntime {
    type Loader = ();
    type Error = &'static str;
    fn activate(
        resident: &ResidentCapability<EmbeddingModel>,
        (): &(),
    ) -> Result<Self, Self::Error> {
        if resident.bytes().is_empty() {
            return Err("empty model");
        }
        Ok(Self {
            dimensions: resident.manifest().recipe().dimensions,
        })
    }
    fn probe_ready(
        &mut self,
        resident: &ResidentCapability<EmbeddingModel>,
    ) -> Result<(), Self::Error> {
        PROBES.fetch_add(1, Ordering::SeqCst);
        if self.dimensions != resident.manifest().recipe().dimensions {
            return Err("dimension drift");
        }
        Ok(())
    }
    fn revoke(&mut self) -> Result<(), Self::Error> {
        REVOKES.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn readiness_requires_verified_residence_activation_and_a_fresh_probe() {
    PROBES.store(0, Ordering::SeqCst);
    REVOKES.store(0, Ordering::SeqCst);
    let bytes = b"real-model-weights";
    let available =
        AvailableCapability::admit(model_manifest(bytes), host(), &Accept).expect("manifest");
    let object = capability_artifact_object(b"model", bytes);
    let installed = acquired(available, object).install(&[]).expect("installed");
    let mut active = installed
        .resident()
        .activate::<ModelRuntime>(&())
        .expect("activated");
    assert_eq!(PROBES.load(Ordering::SeqCst), 1);
    let ready = active.execution_ready().expect("fresh readiness");
    assert_eq!(ready.artifact_bytes(), bytes);
    let client_status = ready.inventory_status(backend_library::CapabilityId::new([9; 32]));
    assert_eq!(
        client_status.lifecycle(),
        backend_library::CapabilityLifecycle::Ready
    );
    assert_eq!(
        client_status.manifest(),
        Some(ready.manifest().identity().to_bytes())
    );
    assert_eq!(PROBES.load(Ordering::SeqCst), 2);
    let Ok(revoked) = active.revoke() else {
        panic!("revocation must succeed");
    };
    assert_eq!(REVOKES.load(Ordering::SeqCst), 1);
    assert_eq!(revoked.into_resident().bytes(), bytes);
}

#[test]
fn wrong_or_incomplete_store_object_cannot_be_installed() {
    let bytes = b"expected-model";
    let available =
        AvailableCapability::admit(model_manifest(bytes), host(), &Accept).expect("manifest");
    let wrong = capability_artifact_object(b"model", b"different-model");
    assert!(matches!(
        available.verify_acquisition(
            AcquiredCapabilityArtifact::new(wrong, authority().source, authority().authorization,),
            &Accept,
        ),
        Err(AcquisitionVerificationError::Artifact { .. })
    ));

    let dependency_bytes = b"tokenizer";
    let dependency = ObjectVersion::<CapabilityArtifactSchema>::from_value(dependency_bytes);
    let mut manifest = model_manifest(bytes);
    manifest.dependencies.push(ArtifactDependency {
        artifact: dependency,
        role: DependencyRole::Tokenizer,
    });
    let available = AvailableCapability::admit(manifest, host(), &Accept).expect("manifest");
    let object = capability_artifact_object(b"model", bytes);
    assert!(matches!(
        acquired(available, object).install(&[]),
        Err(InstallError::Dependency { .. })
    ));
}

#[test]
fn acquisition_source_and_authorization_are_not_self_asserting_installation_labels() {
    let bytes = b"model";
    let object = capability_artifact_object(b"model", bytes);
    let available =
        AvailableCapability::admit(model_manifest(bytes), host(), &Accept).expect("manifest");
    assert!(matches!(
        available.verify_acquisition(
            AcquiredCapabilityArtifact::new(
                Arc::clone(&object),
                ArtifactSourceId::new([99; 32]),
                authority().authorization,
            ),
            &Accept,
        ),
        Err(AcquisitionVerificationError::Source { .. })
    ));

    let available =
        AvailableCapability::admit(model_manifest(bytes), host(), &Accept).expect("manifest");
    assert!(matches!(
        available.verify_acquisition(
            AcquiredCapabilityArtifact::new(object, authority().source, None),
            &Accept,
        ),
        Err(AcquisitionVerificationError::Authorization { .. })
    ));
}

#[test]
fn embedding_space_identity_changes_for_execution_relevant_facts() {
    assert_ne!(model_recipe(384).identity(), model_recipe(768).identity());
    let first = model_recipe(384);
    let second = model_recipe(384);
    assert_eq!(first.identity(), second.identity());
}

#[test]
fn target_and_authentication_are_checked_before_installation() {
    struct Reject;
    impl CapabilityManifestVerifier<EmbeddingModel> for Reject {
        type Error = &'static str;
        fn verify(
            &self,
            _: &UntrustedCapabilityManifest<EmbeddingModel>,
            _: &[u8],
        ) -> Result<(), Self::Error> {
            Err("signature")
        }
    }
    let bytes = b"weights";
    assert!(matches!(
        AvailableCapability::admit(model_manifest(bytes), host(), &Reject),
        Err(ManifestAdmissionError::Authentication("signature"))
    ));
    let mut unsupported = model_manifest(bytes);
    unsupported.target = CapabilityTarget::Native {
        os: OperatingSystem::MacOs,
        architecture: Architecture::Aarch64,
    };
    assert!(matches!(
        AvailableCapability::admit(unsupported, host(), &Accept),
        Err(ManifestAdmissionError::NativeTarget)
    ));
}

#[test]
fn manifest_identity_reuses_artifact_across_recipe_changes_without_snapshot_input() {
    let bytes = b"shared-weights";
    let first = AvailableCapability::admit(model_manifest(bytes), host(), &Accept).expect("first");
    let mut changed = model_manifest(bytes);
    changed.recipe = model_recipe(768);
    let second = AvailableCapability::admit(changed, host(), &Accept).expect("second");
    assert_eq!(first.manifest.artifact, second.manifest.artifact);
    assert_ne!(first.identity(), second.identity());
}

#[cfg(unix)]
#[test]
fn verified_model_and_tokenizer_drive_the_external_runtime_end_to_end()
-> Result<(), Box<dyn std::error::Error>> {
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!(
        "backend-engine-embedding-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let program = root.join("fixture.sh");
    let shell = std::env::var_os("NUDOX_PROCESS_SHELL")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/sh"));
    let cat = std::env::var_os("NUDOX_TEST_COREUTILS_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin"))
        .join("cat");
    let script = format!(
        "#!{}\n{} >/dev/null\nprintf '\\102\\105\\103\\061\\000\\002\\000\\000\\200\\077\\000\\000\\000\\000'\n",
        shell.display(),
        cat.display()
    );
    fs::write(&program, script)?;
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700))?;

    let model_bytes = b"verified-model";
    let tokenizer_bytes: Arc<[u8]> = Arc::from(&b"verified-tokenizer"[..]);
    let tokenizer_id =
        ObjectVersion::<CapabilityArtifactSchema>::from_value(tokenizer_bytes.as_ref());
    let helper_bytes: Arc<[u8]> = Arc::from(fs::read(&program)?);
    let helper_id = ObjectVersion::<CapabilityArtifactSchema>::from_value(helper_bytes.as_ref());
    let mut manifest = model_manifest(model_bytes);
    manifest.recipe = model_recipe(2);
    manifest.dependencies.push(ArtifactDependency {
        artifact: tokenizer_id,
        role: DependencyRole::Tokenizer,
    });
    manifest.dependencies.push(ArtifactDependency {
        artifact: helper_id,
        role: DependencyRole::Runtime,
    });
    manifest.dependencies.sort_unstable();
    let available = AvailableCapability::admit(manifest, host(), &Accept)?;
    let installed = acquired(available, capability_artifact_object(b"model", model_bytes))
        .install(&[tokenizer_id, helper_id])
        .map_err(|_| "installation")?;
    let loader = ExternalEmbeddingLoader {
        program: program.clone(),
        arguments: Vec::new(),
        workspace: root.clone(),
        environment: backend_compile::ProcessEnvironment::new(vec![(
            "PATH".into(),
            "/usr/bin:/bin".into(),
        )])?,
        process_limits: backend_compile::ProcessLimits::new(64, 64, Duration::from_secs(2), 128)?
            .with_input_bytes_limit(512)?,
        maximum_text_bytes: 128,
        executable: backend_compile::ToolchainArtifact::from_path(&program, Vec::new())?,
        tokenizer: tokenizer_bytes,
        helper: helper_bytes,
    };
    let mut active = installed
        .resident()
        .activate::<ExternalEmbeddingCapability>(&loader)
        .map_err(|_| "activation")?;
    {
        let mut ready = active.execution_ready().map_err(|_| "readiness")?;
        let coordinates = ready
            .runtime()
            .infer(backend_compile::EmbeddingInvocation {
                purpose: backend_compile::EmbeddingPurpose::Document,
                text: "fn parse() {}",
            })?;
        assert_eq!(coordinates.values(), &[1.0, 0.0]);
        assert_eq!(
            coordinates.purpose(),
            backend_compile::EmbeddingPurpose::Document
        );
    }
    let _revoked = active.revoke().map_err(|_| "revocation")?;
    fs::remove_dir_all(root)?;
    Ok(())
}
