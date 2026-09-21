use super::*;

pub(super) fn read_availability(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryNativeAvailability, RegistryNativeMetadataCodecError> {
    Ok(match reader.take_u8()? {
        0 => RegistryNativeAvailability::Recorded,
        1 => RegistryNativeAvailability::NotRecorded(reader.take_text()?),
        _ => return Err(RegistryNativeMetadataCodecError::Tag),
    })
}

pub(super) fn read_provenance(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryNativeProvenance, RegistryNativeMetadataCodecError> {
    Ok(match reader.take_u8()? {
        0 => RegistryNativeProvenance::SourceDigest(reader.take_array()?),
        1 => RegistryNativeProvenance::NotRecorded(reader.take_text()?),
        _ => return Err(RegistryNativeMetadataCodecError::Tag),
    })
}

pub(super) fn read_details(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryNativeDetails, RegistryNativeMetadataCodecError> {
    Ok(match reader.take_u8()? {
        0 => {
            let artifacts = read_artifacts(reader)?;
            let features = reader.take_count()?;
            let mut values = Vec::with_capacity(features);
            for _ in 0..features {
                let name = reader.take_text()?;
                let members = reader.take_count()?;
                let mut entries = Vec::with_capacity(members);
                for _ in 0..members {
                    entries.push(reader.take_text()?);
                }
                values.push(RegistryNativeFeature {
                    name,
                    members: entries.into_boxed_slice(),
                });
            }
            RegistryNativeDetails::Cargo(RegistryCargoMetadata {
                artifacts,
                features: values.into_boxed_slice(),
            })
        }
        1 => {
            let artifacts = read_artifacts(reader)?;
            let tags = reader.take_count()?;
            let mut values = Vec::with_capacity(tags);
            for _ in 0..tags {
                values.push(RegistryNativeDistTag {
                    name: reader.take_text()?,
                    version: reader.take_text()?,
                });
            }
            RegistryNativeDetails::Npm(RegistryNpmMetadata {
                artifacts,
                dist_tags: values.into_boxed_slice(),
                deprecation: reader.take_optional_text()?,
            })
        }
        2 => RegistryNativeDetails::Pypi(RegistryPypiMetadata {
            artifacts: read_artifacts(reader)?,
            requires_python: reader.take_optional_text()?,
            standing_reason: reader.take_optional_text()?,
        }),
        3 => RegistryNativeDetails::Maven(RegistryMavenMetadata {
            artifacts: read_artifacts(reader)?,
            checksum: read_observation(reader, read_maven_checksum)?,
            signature: read_observation(reader, read_evidence_claim)?,
            pom: read_observation(reader, read_evidence_claim)?,
            dependencies: read_dependency_facts_observation(reader)?,
        }),
        4 => {
            let artifacts = read_artifacts(reader)?;
            let vulnerabilities = read_observation(reader, |reader| {
                let count = reader.take_count()?;
                let mut values = Vec::with_capacity(count);
                for _ in 0..count {
                    values.push(RegistryNativeVulnerability {
                        advisory_url: reader.take_optional_text()?,
                        severity: reader.take_u8()?,
                    });
                }
                Ok(values.into_boxed_slice())
            })?;
            RegistryNativeDetails::Nuget(RegistryNugetMetadata {
                artifacts,
                vulnerabilities,
                dependencies: read_dependency_facts_observation(reader)?,
                deprecation: reader.take_optional_text()?,
            })
        }
        5 => {
            let artifacts = read_artifacts(reader)?;
            let count = reader.take_count()?;
            let mut retracts = Vec::with_capacity(count);
            for _ in 0..count {
                retracts.push(RegistryGoRetract {
                    lower: reader.take_text()?,
                    upper: reader.take_text()?,
                });
            }
            RegistryNativeDetails::Golang(RegistryGoMetadata {
                artifacts,
                retracts: retracts.into_boxed_slice(),
                source: read_observation(reader, read_go_source)?,
            })
        }
        6 => RegistryNativeDetails::Cpp(RegistryConanMetadata {
            artifacts: read_artifacts(reader)?,
            source: match reader.take_u8()? {
                0 => RegistryConanSourceAvailability::Archive,
                1 => RegistryConanSourceAvailability::RecipeOnly,
                2 => RegistryConanSourceAvailability::Unavailable,
                _ => return Err(RegistryNativeMetadataCodecError::Tag),
            },
            source_url: reader.take_optional_text()?,
        }),
        7 => RegistryNativeDetails::Unavailable {
            ecosystem: RegistryEcosystem::try_from(reader.take_u8()?)
                .map_err(|_| RegistryNativeMetadataCodecError::Tag)?,
            reason: reader.take_text()?,
        },
        _ => return Err(RegistryNativeMetadataCodecError::Tag),
    })
}

fn read_artifacts(
    reader: &mut CanonicalReader<'_>,
) -> Result<Box<[RegistryNativeArtifact]>, RegistryNativeMetadataCodecError> {
    let count = reader.take_count()?;
    let mut values = Vec::with_capacity(count);
    for _ in 0..count {
        values.push(RegistryNativeArtifact {
            filename: reader.take_text()?,
            url: reader.take_text()?,
            checksum: read_checksum(reader)?,
            kind: match reader.take_u8()? {
                0 => RegistryNativeArtifactKind::CargoCrate,
                1 => RegistryNativeArtifactKind::NpmTarball,
                2 => RegistryNativeArtifactKind::PythonSdist,
                3 => RegistryNativeArtifactKind::PythonWheel,
                4 => RegistryNativeArtifactKind::PythonSignature,
                5 => RegistryNativeArtifactKind::MavenJar,
                6 => RegistryNativeArtifactKind::MavenSources,
                7 => RegistryNativeArtifactKind::MavenPom,
                8 => RegistryNativeArtifactKind::MavenSignature,
                9 => RegistryNativeArtifactKind::MavenChecksum,
                10 => RegistryNativeArtifactKind::NugetPackage,
                11 => RegistryNativeArtifactKind::GoSource,
                12 => RegistryNativeArtifactKind::ConanSource,
                13 => RegistryNativeArtifactKind::ConanRecipe,
                14 => RegistryNativeArtifactKind::Other,
                _ => return Err(RegistryNativeMetadataCodecError::Tag),
            },
            requires_python: reader.take_optional_text()?,
            size: match reader.take_u8()? {
                0 => None,
                1 => Some(reader.take_u64()?),
                _ => return Err(RegistryNativeMetadataCodecError::Tag),
            },
            yanked: match reader.take_u8()? {
                0 => None,
                1 => Some(false),
                2 => Some(true),
                _ => return Err(RegistryNativeMetadataCodecError::Tag),
            },
            yanked_reason: reader.take_optional_text()?,
        });
    }
    Ok(values.into_boxed_slice())
}

fn read_checksum(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryNativeChecksum, RegistryNativeMetadataCodecError> {
    let algorithm = match reader.take_u8()? {
        0 => RegistryNativeChecksumAlgorithm::Sha1,
        1 => RegistryNativeChecksumAlgorithm::Sha256,
        2 => RegistryNativeChecksumAlgorithm::Sha512,
        3 => RegistryNativeChecksumAlgorithm::GoModule,
        _ => return Err(RegistryNativeMetadataCodecError::Tag),
    };
    let digest = reader.take_bytes()?;
    Ok(RegistryNativeChecksum {
        algorithm,
        digest: digest.into(),
    })
}

fn read_maven_checksum(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryMavenChecksum, RegistryNativeMetadataCodecError> {
    Ok(RegistryMavenChecksum {
        url: reader.take_text()?,
        checksum: read_checksum(reader)?,
    })
}

fn read_evidence_claim(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryNativeEvidenceClaim, RegistryNativeMetadataCodecError> {
    Ok(RegistryNativeEvidenceClaim {
        url: reader.take_text()?,
        digest: reader.take_array()?,
        bytes: reader.take_u64()?,
    })
}

fn read_go_source(
    reader: &mut CanonicalReader<'_>,
) -> Result<RegistryGoSourceFacts, RegistryNativeMetadataCodecError> {
    Ok(RegistryGoSourceFacts {
        module: reader.take_text()?,
        version: reader.take_text()?,
        info: read_evidence_claim(reader)?,
        module_file: read_evidence_claim(reader)?,
        checksum: read_evidence_claim(reader)?,
    })
}

fn read_observation<T>(
    reader: &mut CanonicalReader<'_>,
    read_recorded: impl Fn(&mut CanonicalReader<'_>) -> Result<T, RegistryNativeMetadataCodecError>,
) -> Result<RegistryNativeObservation<T>, RegistryNativeMetadataCodecError> {
    Ok(match reader.take_u8()? {
        0 => RegistryNativeObservation::Recorded(read_recorded(reader)?),
        1 => RegistryNativeObservation::NotRecorded(reader.take_text()?),
        2 => RegistryNativeObservation::Unavailable(reader.take_text()?),
        _ => return Err(RegistryNativeMetadataCodecError::Tag),
    })
}

fn read_dependency_facts_observation(
    reader: &mut CanonicalReader<'_>,
) -> Result<
    RegistryNativeObservation<DependencyFacts<Box<[PackageDependencyRecord]>>>,
    RegistryNativeMetadataCodecError,
> {
    read_observation(reader, read_dependency_facts)
}

fn read_dependency_facts(
    reader: &mut CanonicalReader<'_>,
) -> Result<DependencyFacts<Box<[PackageDependencyRecord]>>, RegistryNativeMetadataCodecError> {
    match reader.take_u8()? {
        0 => {
            let count = reader.take_count()?;
            let mut rows = Vec::with_capacity(count);
            for _ in 0..count {
                let source = read_package_reference(reader)?;
                let ecosystem = RegistryEcosystem::try_from(reader.take_u8()?)
                    .map_err(|_| RegistryNativeMetadataCodecError::Tag)?;
                let target = PackageDependencyTarget::new(
                    ecosystem,
                    reader.take_text()?,
                    reader.take_text()?,
                    match reader.take_u8()? {
                        0 => None,
                        1 => Some(read_package_reference(reader)?),
                        _ => return Err(RegistryNativeMetadataCodecError::Tag),
                    },
                )
                .map_err(|_| RegistryNativeMetadataCodecError::Admission)?;
                let scope = match reader.take_u8()? {
                    0 => DependencyScope::Runtime,
                    1 => DependencyScope::Optional,
                    2 => DependencyScope::Development,
                    3 => DependencyScope::Build,
                    4 => DependencyScope::Peer,
                    _ => return Err(RegistryNativeMetadataCodecError::Tag),
                };
                let optional = match reader.take_u8()? {
                    0 => false,
                    1 => true,
                    _ => return Err(RegistryNativeMetadataCodecError::Tag),
                };
                let authority = match reader.take_u8()? {
                    0 => DependencyAuthority::RegistryMetadata,
                    1 => DependencyAuthority::ArchiveManifest,
                    2 => DependencyAuthority::ForgeManifest,
                    3 => DependencyAuthority::LocalManifest,
                    _ => return Err(RegistryNativeMetadataCodecError::Tag),
                };
                rows.push(PackageDependencyRecord {
                    source,
                    target,
                    scope,
                    optional,
                    evidence: DependencyEvidence {
                        authority,
                        frontier: reader.take_array()?,
                        provenance: reader.take_array()?,
                    },
                    facts_version: reader.take_array()?,
                });
            }
            let canonical = crate::admit_dependency_rows(rows.clone())
                .map_err(|_| RegistryNativeMetadataCodecError::Admission)?;
            if canonical.as_ref() != rows.as_slice() {
                return Err(RegistryNativeMetadataCodecError::Admission);
            }
            Ok(DependencyFacts::Known(canonical))
        }
        1 => Ok(DependencyFacts::Unknown(
            ProductText::new(reader.take_text()?)
                .map_err(|_| RegistryNativeMetadataCodecError::Text)?,
        )),
        2 => Ok(DependencyFacts::Unavailable(
            ProductText::new(reader.take_text()?)
                .map_err(|_| RegistryNativeMetadataCodecError::Text)?,
        )),
        _ => Err(RegistryNativeMetadataCodecError::Tag),
    }
}

fn read_package_reference(
    reader: &mut CanonicalReader<'_>,
) -> Result<PackageReference, RegistryNativeMetadataCodecError> {
    let tag = reader.take_u8()?;
    let text = reader.take_text()?;
    match tag {
        0 => PackageReference::parse(text)
            .map_err(|_| RegistryNativeMetadataCodecError::Admission)
            .and_then(|reference| match reference {
                PackageReference::Purl(_) => Ok(reference),
                PackageReference::Local(_) => Err(RegistryNativeMetadataCodecError::Admission),
            }),
        1 => Ok(PackageReference::Local(
            ProductText::new(text).map_err(|_| RegistryNativeMetadataCodecError::Text)?,
        )),
        _ => Err(RegistryNativeMetadataCodecError::Tag),
    }
}

pub(super) struct CanonicalReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> CanonicalReader<'a> {
    pub(super) fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub(super) fn is_empty(&self) -> bool {
        self.offset == self.bytes.len()
    }

    pub(super) fn take_exact(
        &mut self,
        length: usize,
    ) -> Result<&'a [u8], RegistryNativeMetadataCodecError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(RegistryNativeMetadataCodecError::Bounds)?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(RegistryNativeMetadataCodecError::Truncated)?;
        self.offset = end;
        Ok(value)
    }

    pub(super) fn take_u8(&mut self) -> Result<u8, RegistryNativeMetadataCodecError> {
        Ok(*self
            .take_exact(1)?
            .first()
            .ok_or(RegistryNativeMetadataCodecError::Truncated)?)
    }

    pub(super) fn take_u16(&mut self) -> Result<u16, RegistryNativeMetadataCodecError> {
        Ok(u16::from_be_bytes(self.take_exact(2)?.try_into().map_err(
            |_| RegistryNativeMetadataCodecError::Truncated,
        )?))
    }

    fn take_u32(&mut self) -> Result<u32, RegistryNativeMetadataCodecError> {
        Ok(u32::from_be_bytes(self.take_exact(4)?.try_into().map_err(
            |_| RegistryNativeMetadataCodecError::Truncated,
        )?))
    }

    fn take_u64(&mut self) -> Result<u64, RegistryNativeMetadataCodecError> {
        Ok(u64::from_be_bytes(self.take_exact(8)?.try_into().map_err(
            |_| RegistryNativeMetadataCodecError::Truncated,
        )?))
    }

    fn take_array<const N: usize>(&mut self) -> Result<[u8; N], RegistryNativeMetadataCodecError> {
        self.take_exact(N)?
            .try_into()
            .map_err(|_| RegistryNativeMetadataCodecError::Truncated)
    }

    fn take_bytes(&mut self) -> Result<Box<[u8]>, RegistryNativeMetadataCodecError> {
        let length = usize::try_from(self.take_u32()?)
            .map_err(|_| RegistryNativeMetadataCodecError::Bounds)?;
        if length > MAX_REGISTRY_NATIVE_TEXT_BYTES {
            return Err(RegistryNativeMetadataCodecError::Bounds);
        }
        Ok(self.take_exact(length)?.into())
    }

    fn take_count(&mut self) -> Result<usize, RegistryNativeMetadataCodecError> {
        let count = usize::try_from(self.take_u32()?)
            .map_err(|_| RegistryNativeMetadataCodecError::Bounds)?;
        if count > MAX_REGISTRY_NATIVE_ROWS {
            return Err(RegistryNativeMetadataCodecError::Bounds);
        }
        Ok(count)
    }

    fn take_text(&mut self) -> Result<String, RegistryNativeMetadataCodecError> {
        let length = usize::try_from(self.take_u32()?)
            .map_err(|_| RegistryNativeMetadataCodecError::Bounds)?;
        if length > MAX_REGISTRY_NATIVE_TEXT_BYTES {
            return Err(RegistryNativeMetadataCodecError::Bounds);
        }
        String::from_utf8(self.take_exact(length)?.to_vec())
            .map_err(|_| RegistryNativeMetadataCodecError::Text)
    }

    fn take_optional_text(&mut self) -> Result<Option<String>, RegistryNativeMetadataCodecError> {
        match self.take_u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.take_text()?)),
            _ => Err(RegistryNativeMetadataCodecError::Tag),
        }
    }
}
