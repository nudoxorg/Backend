use super::*;

pub(super) fn put_u16(output: &mut Vec<u8>, value: u16) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_u32(output: &mut Vec<u8>, value: usize) {
    output.extend_from_slice(&u32::try_from(value).unwrap_or(u32::MAX).to_be_bytes());
}

fn put_u64(output: &mut Vec<u8>, value: u64) {
    output.extend_from_slice(&value.to_be_bytes());
}

fn put_text(output: &mut Vec<u8>, value: &str) {
    put_u32(output, value.len());
    output.extend_from_slice(value.as_bytes());
}

fn put_optional_text(output: &mut Vec<u8>, value: Option<&str>) {
    match value {
        Some(value) => {
            output.push(1);
            put_text(output, value);
        }
        None => output.push(0),
    }
}

fn put_optional_bool(output: &mut Vec<u8>, value: Option<bool>) {
    match value {
        None => output.push(0),
        Some(false) => output.push(1),
        Some(true) => output.push(2),
    }
}

pub(super) fn write_availability(output: &mut Vec<u8>, value: &RegistryNativeAvailability) {
    match value {
        RegistryNativeAvailability::Recorded => output.push(0),
        RegistryNativeAvailability::NotRecorded(reason) => {
            output.push(1);
            put_text(output, reason);
        }
    }
}

pub(super) fn write_provenance(output: &mut Vec<u8>, value: &RegistryNativeProvenance) {
    match value {
        RegistryNativeProvenance::SourceDigest(digest) => {
            output.push(0);
            output.extend_from_slice(digest);
        }
        RegistryNativeProvenance::NotRecorded(reason) => {
            output.push(1);
            put_text(output, reason);
        }
    }
}

pub(super) fn write_details(output: &mut Vec<u8>, value: &RegistryNativeDetails) {
    match value {
        RegistryNativeDetails::Cargo(value) => {
            output.push(0);
            write_artifacts(output, &value.artifacts);
            put_u32(output, value.features.len());
            for feature in &value.features {
                put_text(output, &feature.name);
                put_u32(output, feature.members.len());
                for member in &feature.members {
                    put_text(output, member);
                }
            }
        }
        RegistryNativeDetails::Npm(value) => {
            output.push(1);
            write_artifacts(output, &value.artifacts);
            put_u32(output, value.dist_tags.len());
            for tag in &value.dist_tags {
                put_text(output, &tag.name);
                put_text(output, &tag.version);
            }
            put_optional_text(output, value.deprecation.as_deref());
        }
        RegistryNativeDetails::Pypi(value) => {
            output.push(2);
            write_artifacts(output, &value.artifacts);
            put_optional_text(output, value.requires_python.as_deref());
            put_optional_text(output, value.standing_reason.as_deref());
        }
        RegistryNativeDetails::Maven(value) => {
            output.push(3);
            write_artifacts(output, &value.artifacts);
            write_observation(output, &value.checksum, write_maven_checksum);
            write_observation(output, &value.signature, write_evidence_claim);
            write_observation(output, &value.pom, write_evidence_claim);
            write_dependency_facts_observation(output, &value.dependencies);
        }
        RegistryNativeDetails::Nuget(value) => {
            output.push(4);
            write_artifacts(output, &value.artifacts);
            write_observation(output, &value.vulnerabilities, |output, rows| {
                put_u32(output, rows.len());
                for row in rows {
                    put_optional_text(output, row.advisory_url.as_deref());
                    output.push(row.severity);
                }
            });
            write_dependency_facts_observation(output, &value.dependencies);
            put_optional_text(output, value.deprecation.as_deref());
        }
        RegistryNativeDetails::Golang(value) => {
            output.push(5);
            write_artifacts(output, &value.artifacts);
            put_u32(output, value.retracts.len());
            for retract in &value.retracts {
                put_text(output, &retract.lower);
                put_text(output, &retract.upper);
            }
            write_observation(output, &value.source, write_go_source);
        }
        RegistryNativeDetails::Cpp(value) => {
            output.push(6);
            write_artifacts(output, &value.artifacts);
            output.push(match value.source {
                RegistryConanSourceAvailability::Archive => 0,
                RegistryConanSourceAvailability::RecipeOnly => 1,
                RegistryConanSourceAvailability::Unavailable => 2,
            });
            put_optional_text(output, value.source_url.as_deref());
        }
        RegistryNativeDetails::Unavailable { ecosystem, reason } => {
            output.push(7);
            output.push(*ecosystem as u8);
            put_text(output, reason);
        }
    }
}

fn write_artifacts(output: &mut Vec<u8>, values: &[RegistryNativeArtifact]) {
    put_u32(output, values.len());
    for value in values {
        put_text(output, &value.filename);
        put_text(output, &value.url);
        write_checksum(output, &value.checksum);
        output.push(value.kind as u8);
        put_optional_text(output, value.requires_python.as_deref());
        match value.size {
            Some(size) => {
                output.push(1);
                put_u64(output, size);
            }
            None => output.push(0),
        }
        put_optional_bool(output, value.yanked);
        put_optional_text(output, value.yanked_reason.as_deref());
    }
}

fn write_checksum(output: &mut Vec<u8>, value: &RegistryNativeChecksum) {
    output.push(match value.algorithm {
        RegistryNativeChecksumAlgorithm::Sha1 => 0,
        RegistryNativeChecksumAlgorithm::Sha256 => 1,
        RegistryNativeChecksumAlgorithm::Sha512 => 2,
        RegistryNativeChecksumAlgorithm::GoModule => 3,
    });
    put_u32(output, value.digest.len());
    output.extend_from_slice(&value.digest);
}

fn write_maven_checksum(output: &mut Vec<u8>, value: &RegistryMavenChecksum) {
    put_text(output, &value.url);
    write_checksum(output, &value.checksum);
}

fn write_evidence_claim(output: &mut Vec<u8>, value: &RegistryNativeEvidenceClaim) {
    put_text(output, &value.url);
    output.extend_from_slice(&value.digest);
    put_u64(output, value.bytes);
}

fn write_go_source(output: &mut Vec<u8>, value: &RegistryGoSourceFacts) {
    put_text(output, &value.module);
    put_text(output, &value.version);
    write_evidence_claim(output, &value.info);
    write_evidence_claim(output, &value.module_file);
    write_evidence_claim(output, &value.checksum);
}

fn write_observation<T>(
    output: &mut Vec<u8>,
    value: &RegistryNativeObservation<T>,
    write_recorded: impl Fn(&mut Vec<u8>, &T),
) {
    match value {
        RegistryNativeObservation::Recorded(value) => {
            output.push(0);
            write_recorded(output, value);
        }
        RegistryNativeObservation::NotRecorded(reason) => {
            output.push(1);
            put_text(output, reason);
        }
        RegistryNativeObservation::Unavailable(reason) => {
            output.push(2);
            put_text(output, reason);
        }
    }
}

fn write_dependency_facts_observation(
    output: &mut Vec<u8>,
    value: &RegistryNativeObservation<DependencyFacts<Box<[PackageDependencyRecord]>>>,
) {
    write_observation(output, value, write_dependency_facts);
}

fn write_dependency_facts(
    output: &mut Vec<u8>,
    value: &DependencyFacts<Box<[PackageDependencyRecord]>>,
) {
    match value {
        DependencyFacts::Known(rows) => {
            output.push(0);
            put_u32(output, rows.len());
            for row in rows {
                write_package_reference(output, &row.source);
                output.push(row.target.ecosystem as u8);
                put_text(output, row.target.name.as_str());
                put_text(output, row.target.requirement.as_str());
                match &row.target.resolved {
                    Some(reference) => {
                        output.push(1);
                        write_package_reference(output, reference);
                    }
                    None => output.push(0),
                }
                output.push(match row.scope {
                    DependencyScope::Runtime => 0,
                    DependencyScope::Optional => 1,
                    DependencyScope::Development => 2,
                    DependencyScope::Build => 3,
                    DependencyScope::Peer => 4,
                });
                output.push(u8::from(row.optional));
                output.push(match row.evidence.authority {
                    DependencyAuthority::RegistryMetadata => 0,
                    DependencyAuthority::ArchiveManifest => 1,
                    DependencyAuthority::ForgeManifest => 2,
                    DependencyAuthority::LocalManifest => 3,
                });
                output.extend_from_slice(&row.evidence.frontier);
                output.extend_from_slice(&row.evidence.provenance);
                output.extend_from_slice(&row.facts_version);
            }
        }
        DependencyFacts::Unknown(reason) => {
            output.push(1);
            put_text(output, reason.as_str());
        }
        DependencyFacts::Unavailable(reason) => {
            output.push(2);
            put_text(output, reason.as_str());
        }
    }
}

fn write_package_reference(output: &mut Vec<u8>, value: &PackageReference) {
    match value {
        PackageReference::Purl(value) => {
            output.push(0);
            put_text(output, value.as_str());
        }
        PackageReference::Local(value) => {
            output.push(1);
            put_text(output, value.as_str());
        }
    }
}
