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
fn cargo_publish_time_requires_exact_valid_utc_calendar() {
    let valid = CargoPublishTime::parse("2024-02-29T23:59:59Z").expect("leap day");
    assert_eq!(valid.as_str(), "2024-02-29T23:59:59Z");
    for invalid in [
        "2023-02-29T23:59:59Z",
        "2024-02-30T23:59:59Z",
        "2024-2-09T23:59:59Z",
        "2024-02-09T3:59:59Z",
        "2024-02-09T23:59:59.1Z",
        "2024-02-09T24:00:00Z",
        "2024-02-09T23:60:00Z",
        "2024-02-09T23:59:60Z",
        "0000-01-01T00:00:00Z",
    ] {
        assert!(CargoPublishTime::parse(invalid).is_none(), "accepted {invalid}");
    }
}

#[test]
fn cargo_native_admission_rejects_impossible_publication_date() {
    let value = recorded(RegistryNativeDetails::Cargo(RegistryCargoMetadata {
        artifacts: Box::new([]),
        features: Box::new([]),
        features2: Box::new([]),
        published_at: Some("2024-02-30T23:59:59Z".to_owned()),
        rust_version: None,
    }));
    assert_eq!(value.admit(), Err(ProductAdmissionError::NativeMetadata));
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
            members: vec!["dep:serde".to_owned(), "serde?/alloc".to_owned()]
                .into_boxed_slice(),
        }]
        .into_boxed_slice(),
        features2: vec![RegistryNativeFeature {
            name: "default".to_owned(),
            members: vec!["serde?/alloc".to_owned()].into_boxed_slice(),
        }]
        .into_boxed_slice(),
        published_at: Some("2025-11-12T19:30:12Z".to_owned()),
        rust_version: Some("1.60".to_owned()),
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
        features2: Box::new([]),
        published_at: None,
        rust_version: None,
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
