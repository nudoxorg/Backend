//! The compiled worker profile.
//!
//! The default profile is a bounded, deterministic product composition: its
//! recipe and read manifest are versioned identities, its semantic contract
//! contains positive, negative, and range reads, and every result is signed
//! by the configured authority.  `builtin-echo` remains an explicitly named
//! compatibility profile for older clients and tests.

use crate::input_cas::InputCas;
use crate::process::{AUTHORITY_SECRET_ENV, WorkerProcessConfig, WorkerProcessError};
use crate::service::{JobAdmission, JobCancellation, WorkerJob, WorkerJobBindings, WorkerService};
use backend_engine::{
    AdmittedAuthority, AuthorityEpoch, AuthorityExpectation, AuthorityVersion,
    CompleteSemanticCoverage, CoverageWitness, DependencyManifest, ExecutionRequestExpectation,
    ExpectedIdentity, IdContext, ImmutableObjectSchema, ObjectKey, ObjectSummaryExpectation,
    ObjectVersion, OutputEquivalence, PureRecipeExecutor, ReadManifestId, RecipeId, Relation,
    RelationState, RevocationVersion, RootSummaryExpectation, SemanticCoverageAdmissionError,
    SemanticCoverageBinding, SemanticCoverageValidator, SparseCoverage, TransportLimits,
    TransportMessage, UntrustedSemanticCoverageClaim, WireAuthority, WireAuthorityPolicy,
    WireIdentity, WireRecipeRequest, WorkerCapabilities, WorkerError,
    schema_object_key_identity_claim, schema_object_version_identity_claim,
};
use std::process::ExitCode;

/// Deterministic key retained only for the explicitly named compatibility
/// profile. The production profile always loads its key from the host.
const ECHO_AUTHORITY_SECRET: [u8; 32] = [0x5a; 32];

fn address_hint(config: &WorkerProcessConfig) -> String {
    config.tcp_listen.map_or_else(
        || "default".to_owned(),
        |address| address.to_string().replace(':', "-"),
    )
}

#[path = "builtin/profile.rs"]
mod profile;
pub(super) use backend_engine::builtin::ProductSourceRelation as ProductRelation;
use profile::{
    BuiltinExecutor, BuiltinInputSchema, BuiltinProfile, BuiltinSemanticAuthority,
    ProductInputLoader, ProductSourceRecord, execution_manifest, execution_resources,
    product_dependency_manifest, profile_descriptor, profile_ids,
};

#[path = "builtin/admission.rs"]
mod admission;
mod page_proof;
use admission::BuiltinAdmission;

fn product_source_row_bytes(key: &[u8; 32], value: &ProductSourceRecord) -> Vec<u8> {
    let mut key_bytes = Vec::with_capacity(32);
    ProductRelation::encode_key(key, &mut key_bytes);
    let mut value_bytes = Vec::new();
    ProductRelation::encode_value(value, &mut value_bytes);
    backend_engine::canonical_relation_row(&key_bytes, &value_bytes)
}

pub(super) fn complete_coverage(
    authority: AuthorityVersion,
    admitted: AdmittedAuthority,
) -> Result<CoverageWitness, WorkerError> {
    backend_engine::coverage_from_admitted_authority(authority, &admitted)
        .map_err(|_| WorkerError::Capability)
}

/// Constructs and runs the selected compiled profile.
#[must_use]
pub fn run(config: WorkerProcessConfig) -> ExitCode {
    let profile = match config.profile.as_deref() {
        None | Some("builtin") => BuiltinProfile::Product,
        Some("builtin-echo") => BuiltinProfile::EchoFixture,
        Some(other) => {
            eprintln!("backend-worker: unknown compiled profile {other}");
            return ExitCode::from(64);
        }
    };
    let result = (|| -> Result<ExitCode, WorkerProcessError> {
        let ids = profile_ids(&profile).map_err(WorkerProcessError::Profile)?;
        let limits = config.limits;
        // Keep the worker's receiving namespace beside its endpoint. This
        // gives Unix and TCP compositions the same durable reuse semantics;
        // the directory is owner-scoped and contains only canonical objects.
        let cas_path = config.endpoint.as_ref().map_or_else(
            || {
                std::env::temp_dir()
                    .join(format!("backend-worker-inputs-{}", address_hint(&config)))
            },
            |endpoint| endpoint.as_path().with_extension("inputs"),
        );
        let product_inputs =
            (profile == BuiltinProfile::Product).then(|| ProductInputLoader::new(cas_path.clone()));
        let admission = BuiltinAdmission::new(profile, limits.transport, &cas_path)?;
        let descriptor = profile_descriptor(profile).map_err(WorkerProcessError::Profile)?;
        let manifest = execution_manifest(limits.transport, &descriptor)
            .map_err(WorkerProcessError::Profile)?;
        let resources = execution_resources(ids);
        let capabilities = WorkerCapabilities {
            recipes: vec![admission.recipe],
            max_scope: 1,
            max_resources: resources,
        };
        let secret = match profile {
            BuiltinProfile::Product => config
                .authority_secret
                .as_deref()
                .ok_or_else(|| {
                    WorkerProcessError::Profile(
                        "production profile requires --authority-secret-file (or ".to_owned()
                            + AUTHORITY_SECRET_ENV
                            + ")",
                    )
                })
                .and_then(|path| {
                    backend_engine::read_authority_secret(path).map_err(|error| {
                        WorkerProcessError::Profile(format!(
                            "load authority credential {}: {error}",
                            path.display()
                        ))
                    })
                })?,
            BuiltinProfile::EchoFixture => ECHO_AUTHORITY_SECRET,
        };
        let signer =
            backend_engine::Blake3WorkerSigner::new(admission.authority.to_bytes(), secret);
        let worker = WorkerService::with_signer(
            capabilities,
            BuiltinExecutor {
                ids,
                product_inputs,
            },
            signer,
            manifest,
            limits,
        )
        .map_err(WorkerProcessError::Protocol)?;
        Ok(crate::process::run_process::<_, _, ProductRelation, _>(
            worker, admission, config,
        ))
    })();
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("backend-worker: {error}");
            ExitCode::from(70)
        }
    }
}

fn builtin_workspace_root(
    relation: &RelationState<ProductRelation>,
    authority: AuthorityVersion,
) -> Result<backend_engine::WorkspaceRoot, WorkerProcessError> {
    backend_engine::builtin::execution_input_basis(relation, authority)
        .map_err(WorkerProcessError::Profile)
}

fn relation_state(
    expanded: bool,
    authority: AuthorityVersion,
) -> Result<RelationState<ProductRelation>, WorkerError> {
    let expected =
        AuthorityExpectation::from_typed(&authority, AuthorityEpoch(1), RevocationVersion(1));
    let admitted = expected
        .admit_capability(
            WireAuthority::from_typed(&authority, AuthorityEpoch(1)),
            RevocationVersion(1),
        )
        .map_err(|_| WorkerError::Capability)?;
    let coverage = complete_coverage(authority, admitted)?;
    let builtin =
        ProductSourceRecord::new("backend-builtin").map_err(|_| WorkerError::Capability)?;
    let entries = if expanded {
        let extra =
            ProductSourceRecord::new("backend-extra").map_err(|_| WorkerError::Capability)?;
        vec![
            (
                backend_engine::package_key("backend-builtin").to_bytes(),
                builtin,
            ),
            (
                backend_engine::package_key("backend-extra").to_bytes(),
                extra,
            ),
        ]
    } else {
        vec![(
            backend_engine::package_key("backend-builtin").to_bytes(),
            builtin,
        )]
    };
    RelationState::from_entries(entries, coverage).map_err(|_| WorkerError::Capability)
}
