use super::*;
use crate::{ProductAdmissionError, RegistryEcosystem};

fn recorded(details: RegistryNativeDetails) -> RegistryNativeMetadata {
    RegistryNativeMetadata {
        version: REGISTRY_NATIVE_METADATA_VERSION,
        availability: RegistryNativeAvailability::Recorded,
        provenance: RegistryNativeProvenance::SourceDigest([7; 32]),
        details,
    }
}

#[test]
fn unavailable_metadata_round_trips_and_has_stable_identity() {
    let value = RegistryNativeMetadata::unavailable(
        RegistryEcosystem::Cargo,
        "canonical feed omits native details",
    );
    value.admit().expect("bounded unavailable metadata");
    let canonical = value.encode_canonical();
    assert_eq!(
        RegistryNativeMetadata::decode_canonical(&canonical),
        Ok(value.clone())
    );
    let encoded = serde_json::to_vec(&value).expect("encode metadata");
    let decoded: RegistryNativeMetadata =
        serde_json::from_slice(&encoded).expect("decode metadata");
    assert_eq!(decoded, value);
    assert_eq!(decoded.identity(), value.identity());
}

#[test]
fn recorded_metadata_round_trips_the_canonical_binary_shape() {
    let value = recorded(RegistryNativeDetails::Cargo(RegistryCargoMetadata {
        artifacts: vec![RegistryNativeArtifact {
            filename: "demo-1.0.0.crate".to_owned(),
            url: "https://registry.example/demo-1.0.0.crate".to_owned(),
            checksum: RegistryNativeChecksum {
                algorithm: RegistryNativeChecksumAlgorithm::Sha256,
                digest: Box::new([3; 32]),
            },
            kind: RegistryNativeArtifactKind::CargoCrate,
            requires_python: None,
            size: Some(42),
            yanked: Some(false),
            yanked_reason: None,
        }]
        .into_boxed_slice(),
        features: vec![RegistryNativeFeature {
            name: "default".to_owned(),
            members: vec!["dep:serde".to_owned()].into_boxed_slice(),
        }]
        .into_boxed_slice(),
    }));
    let canonical = value.encode_canonical();
    assert_eq!(
        RegistryNativeMetadata::decode_canonical(&canonical),
        Ok(value.clone())
    );
    assert_eq!(
        value.identity(),
        RegistryNativeMetadata::decode_canonical(&canonical)
            .expect("recorded metadata decodes")
            .identity()
    );
}

#[test]
fn cargo_features_must_be_lexically_sorted() {
    let value = recorded(RegistryNativeDetails::Cargo(RegistryCargoMetadata {
        artifacts: Box::new([]),
        features: vec![
            RegistryNativeFeature {
                name: "z".to_owned(),
                members: Box::new([]),
            },
            RegistryNativeFeature {
                name: "a".to_owned(),
                members: Box::new([]),
            },
        ]
        .into_boxed_slice(),
    }));
    assert_eq!(value.admit(), Err(ProductAdmissionError::NativeMetadata));
}

#[test]
fn checksum_width_is_admitted_before_publication() {
    let value = recorded(RegistryNativeDetails::Npm(RegistryNpmMetadata {
        artifacts: vec![RegistryNativeArtifact {
            filename: "demo.tgz".to_owned(),
            url: "https://registry.example/demo.tgz".to_owned(),
            checksum: RegistryNativeChecksum {
                algorithm: RegistryNativeChecksumAlgorithm::Sha256,
                digest: Box::new([0; 31]),
            },
            kind: RegistryNativeArtifactKind::NpmTarball,
            requires_python: None,
            size: None,
            yanked: Some(false),
            yanked_reason: None,
        }]
        .into_boxed_slice(),
        dist_tags: Box::new([]),
        deprecation: None,
    }));
    assert_eq!(value.admit(), Err(ProductAdmissionError::NativeMetadata));
}

#[test]
fn identity_changes_when_a_native_fact_changes() {
    let first = RegistryNativeMetadata::unavailable(RegistryEcosystem::Npm, "missing");
    let second = RegistryNativeMetadata::unavailable(RegistryEcosystem::Npm, "withheld");
    assert_ne!(first.identity(), second.identity());
}
