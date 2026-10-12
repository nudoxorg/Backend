//! Descriptor controls and a real process protocol fixture, not a pretrained model.

use super::*;
use std::num::{NonZeroU16, NonZeroU32};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn contract() -> LocalQueryRecipeContract {
    LocalQueryRecipeContract {
        metric: vector::Metric::CosineDistance,
        pooling: vector::EmbeddingPooling::Mean,
        encoding: vector::EmbeddingEncoding::Float32,
        query_treatment: LocalEmbeddingTreatment::UnmodifiedUtf8,
        document_treatment: LocalEmbeddingTreatment::UnmodifiedUtf8,
        provider_options: *blake3::hash(b"explicit protocol fixture options").as_bytes(),
    }
}

fn spec(
    contract: LocalQueryRecipeContract,
) -> Result<EmbeddingRuntimeSpecV1, LocalQueryRecipeError> {
    Ok(EmbeddingRuntimeSpecV1::new(
        [1; 32],
        [2; 32],
        [3; 32],
        [4; 32],
        NonZeroU16::new(2).ok_or(LocalQueryRecipeError::DescriptorFormat)?,
        EmbeddingNormalization::L2,
        NonZeroU32::new(1024).ok_or(LocalQueryRecipeError::DescriptorFormat)?,
        contract.options_digest(),
    ))
}

#[test]
fn local_recipe_descriptor_refuses_opaque_and_changed_semantic_options() -> TestResult {
    let contract = contract();
    let spec = spec(contract)?;
    let descriptor = LocalQueryRecipeDescriptor::new(spec, contract)?;
    assert_eq!(
        LocalQueryRecipeDescriptor::decode(&descriptor.canonical_bytes())?,
        descriptor
    );
    let legacy = EmbeddingRuntimeSpecV1::new(
        spec.model(),
        spec.model_version(),
        spec.tokenizer(),
        spec.executable(),
        spec.dimension(),
        spec.normalization(),
        spec.maximum_text_bytes(),
        [0; 32],
    );
    assert_eq!(
        LocalQueryRecipeDescriptor::new(legacy, contract),
        Err(LocalQueryRecipeError::SemanticOptionsMismatch)
    );
    for changed in [
        LocalQueryRecipeContract {
            pooling: vector::EmbeddingPooling::LastToken,
            ..contract
        },
        LocalQueryRecipeContract {
            metric: vector::Metric::EuclideanSquared,
            ..contract
        },
        LocalQueryRecipeContract {
            provider_options: [9; 32],
            ..contract
        },
    ] {
        assert_ne!(changed.options_digest(), contract.options_digest());
        assert_eq!(
            LocalQueryRecipeDescriptor::new(spec, changed),
            Err(LocalQueryRecipeError::SemanticOptionsMismatch)
        );
    }
    let unsupported = LocalQueryRecipeContract {
        encoding: vector::EmbeddingEncoding::SignedInt8,
        ..contract
    };
    assert_eq!(
        LocalQueryRecipeDescriptor::new(spec, unsupported),
        Err(LocalQueryRecipeError::UnsupportedEncoding)
    );
    Ok(())
}

#[test]
fn local_recipe_descriptor_grammar_is_exact_and_treatment_is_closed() -> TestResult {
    let descriptor = LocalQueryRecipeDescriptor::new(spec(contract())?, contract())?;
    let bytes = descriptor.canonical_bytes();
    for end in 0..bytes.len() {
        assert!(LocalQueryRecipeDescriptor::decode(&bytes[..end]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(LocalQueryRecipeDescriptor::decode(&trailing).is_err());
    for offset in 0..DESCRIPTOR_MAGIC.len() {
        let mut changed = bytes.clone();
        changed[offset] ^= 1;
        assert!(LocalQueryRecipeDescriptor::decode(&changed).is_err());
    }
    let tail = bytes.len() - CONTRACT_BYTES;
    for (offset, error) in [
        (2, LocalQueryRecipeError::UnsupportedEncoding),
        (3, LocalQueryRecipeError::UnsupportedTreatment),
        (4, LocalQueryRecipeError::UnsupportedTreatment),
    ] {
        let mut changed = bytes.clone();
        changed[tail + offset] = 255;
        assert_eq!(LocalQueryRecipeDescriptor::decode(&changed), Err(error));
    }
    Ok(())
}

#[test]
fn local_recipe_vector_model_revision_includes_real_spec_and_launch_configuration() -> TestResult {
    // Scalar identity controls; these bytes are not an activated runtime proof.
    let contract = contract();
    let spec = spec(contract)?;
    let model = local_model_version(spec, [7; 32]);
    assert_ne!(model, local_model_version(spec, [8; 32]));
    let changed = EmbeddingRuntimeSpecV1::new(
        spec.model(),
        spec.model_version(),
        spec.tokenizer(),
        [255; 32],
        spec.dimension(),
        spec.normalization(),
        spec.maximum_text_bytes(),
        spec.options_digest(),
    );
    assert_ne!(model, local_model_version(changed, [7; 32]));
    Ok(())
}

#[cfg(unix)]
#[test]
fn local_recipe_admission_reuses_the_actual_activated_arc_without_probing() -> TestResult {
    use backend_engine::application::{
        EmbeddingRequirement, EmbeddingRuntimeInstall, EmbeddingRuntimeLimits,
        EmbeddingRuntimeProvision, install_embedding_runtime,
    };
    use std::os::unix::fs::PermissionsExt;

    let directory = crate::test_support::private_temp_dir("local-recipe")?;
    let program = directory.path().join("protocol-fixture.py");
    let counter = directory.path().join("calls");
    std::fs::write(
        &program,
        br"#!/usr/bin/env python3
import os, struct, sys
frame = sys.stdin.buffer.read()
if frame[:4] != b'BEM2' or len(frame) < 108:
    sys.exit(3)
count = struct.unpack('>I', frame[104:108])[0]
offset = 108
identities = []
for _ in range(count):
    identities.append(frame[offset:offset + 32])
    length = struct.unpack('>I', frame[offset + 32:offset + 36])[0]
    offset += 36 + length
if offset != len(frame):
    sys.exit(4)
with open(os.environ['FIXTURE_COUNTER'], 'ab') as counter:
    counter.write(b'x')
response = bytearray(b'BEC2' + struct.pack('>HI', 2, count))
for identity in identities:
    response.extend(identity)
    response.extend(struct.pack('<ff', 1.0, 0.0))
sys.stdout.buffer.write(response)
",
    )?;
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))?;
    let contract = contract();
    let config = directory.path().join("embedding.config");
    let spec = install_embedding_runtime(
        &config,
        EmbeddingRuntimeInstall {
            requirement: EmbeddingRequirement::Optional,
            program,
            arguments: Vec::new(),
            dependencies: Vec::new(),
            environment: vec![(
                "FIXTURE_COUNTER".to_owned(),
                counter.to_string_lossy().into_owned(),
            )],
            limits: EmbeddingRuntimeLimits {
                stdout_bytes: 256,
                stderr_bytes: 128,
                output_bytes: 512,
                input_bytes: 2048,
                wall_time_ms: 2000,
                workspace_bytes: 1024 * 1024,
                process_count: 8,
                memory_bytes: None,
                cpu_time_ms: None,
                resident_credit_bytes: 128 * 1024 * 1024,
            },
            model_bytes: b"protocol fixture model".to_vec(),
            tokenizer_bytes: b"protocol fixture tokenizer".to_vec(),
            dimension: NonZeroU16::new(2).ok_or(LocalQueryRecipeError::DescriptorFormat)?,
            normalization: EmbeddingNormalization::L2,
            maximum_text_bytes: NonZeroU32::new(1024)
                .ok_or(LocalQueryRecipeError::DescriptorFormat)?,
            options_digest: contract.options_digest(),
        },
    )?;
    let provision = EmbeddingRuntimeProvision::open(&config)?;
    let runtime = provision
        .runtime()
        .ok_or("protocol fixture failed real activation")?;
    let before = std::fs::read(&counter)?;
    assert!(
        !before.is_empty(),
        "the fixture must actually activate before admission"
    );
    let descriptor = LocalQueryRecipeDescriptor::new(spec, contract)?;
    let admitted = descriptor.admit(Arc::clone(&runtime))?;
    assert!(Arc::ptr_eq(admitted.runtime(), &runtime));
    assert_eq!(
        std::fs::read(&counter)?,
        before,
        "admission must not probe/start another model"
    );
    assert_eq!(admitted.execution_identity(), runtime.execution_identity());
    assert_eq!(admitted.recipe().dimensions.get(), 2);
    assert_eq!(admitted.recipe().pooling, vector::EmbeddingPooling::Mean);
    assert_eq!(
        admitted.recipe().normalization,
        vector::EmbeddingNormalization::UnitL2
    );
    let different_dimension = NonZeroU16::new(3).ok_or(LocalQueryRecipeError::DescriptorFormat)?;
    let different_text = NonZeroU32::new(512).ok_or(LocalQueryRecipeError::DescriptorFormat)?;
    for (field, mismatched) in [
        (
            "model",
            EmbeddingRuntimeSpecV1::new(
                [255; 32],
                spec.model_version(),
                spec.tokenizer(),
                spec.executable(),
                spec.dimension(),
                spec.normalization(),
                spec.maximum_text_bytes(),
                spec.options_digest(),
            ),
        ),
        (
            "model_version",
            EmbeddingRuntimeSpecV1::new(
                spec.model(),
                [255; 32],
                spec.tokenizer(),
                spec.executable(),
                spec.dimension(),
                spec.normalization(),
                spec.maximum_text_bytes(),
                spec.options_digest(),
            ),
        ),
        (
            "tokenizer",
            EmbeddingRuntimeSpecV1::new(
                spec.model(),
                spec.model_version(),
                [255; 32],
                spec.executable(),
                spec.dimension(),
                spec.normalization(),
                spec.maximum_text_bytes(),
                spec.options_digest(),
            ),
        ),
        (
            "executable",
            EmbeddingRuntimeSpecV1::new(
                spec.model(),
                spec.model_version(),
                spec.tokenizer(),
                [255; 32],
                spec.dimension(),
                spec.normalization(),
                spec.maximum_text_bytes(),
                spec.options_digest(),
            ),
        ),
        (
            "dimension",
            EmbeddingRuntimeSpecV1::new(
                spec.model(),
                spec.model_version(),
                spec.tokenizer(),
                spec.executable(),
                different_dimension,
                spec.normalization(),
                spec.maximum_text_bytes(),
                spec.options_digest(),
            ),
        ),
        (
            "normalization",
            EmbeddingRuntimeSpecV1::new(
                spec.model(),
                spec.model_version(),
                spec.tokenizer(),
                spec.executable(),
                spec.dimension(),
                EmbeddingNormalization::None,
                spec.maximum_text_bytes(),
                spec.options_digest(),
            ),
        ),
        (
            "maximum_text_bytes",
            EmbeddingRuntimeSpecV1::new(
                spec.model(),
                spec.model_version(),
                spec.tokenizer(),
                spec.executable(),
                spec.dimension(),
                spec.normalization(),
                different_text,
                spec.options_digest(),
            ),
        ),
    ] {
        assert!(matches!(
            LocalQueryRecipeDescriptor::new(mismatched, contract)?.admit(Arc::clone(&runtime)),
            Err(LocalQueryRecipeError::RuntimeSpec(EmbeddingRuntimeSpecError::ObservedMismatch { field: actual })) if actual == field
        ));
    }
    let different_contract = LocalQueryRecipeContract {
        pooling: vector::EmbeddingPooling::LastToken,
        ..contract
    };
    let different_options = EmbeddingRuntimeSpecV1::new(
        spec.model(),
        spec.model_version(),
        spec.tokenizer(),
        spec.executable(),
        spec.dimension(),
        spec.normalization(),
        spec.maximum_text_bytes(),
        different_contract.options_digest(),
    );
    assert!(matches!(
        LocalQueryRecipeDescriptor::new(different_options, different_contract)?
            .admit(Arc::clone(&runtime)),
        Err(LocalQueryRecipeError::RuntimeSpec(
            EmbeddingRuntimeSpecError::ObservedMismatch {
                field: "options_digest"
            }
        ))
    ));
    assert_eq!(std::fs::read(&counter)?, before);
    Ok(())
}
