//! Source declarations from Python packaging files, independent of registry authority.

use crate::DependencyScope;
use serde::{Deserialize, Serialize};

/// Static extraction semantics bound into metadata and local input identities.
/// Changes to manifest selection or interpretation require a new policy ID.
pub const PYTHON_PROJECT_EXTRACTION_POLICY: &[u8] = b"nudox.python-project.static.v2";

/// Content evidence for a static packaging observation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonMetadataEvidence {
    /// Admitted path relative to the project root.
    pub path: String,
    /// BLAKE3 of the exact source bytes parsed.
    pub digest: [u8; 32],
    /// Exact source byte length, bounded before parsing.
    pub bytes: u64,
}

/// A packaging declaration with explicit dynamic and omitted states.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case", deny_unknown_fields)]
pub enum PythonMetadataFact<T> {
    /// A static declaration was admitted; this is not registry publication authority.
    Recorded {
        /// Declared value.
        value: T,
        /// Source files needed to establish this declaration.
        evidence: Vec<PythonMetadataEvidence>,
    },
    /// Some declarations were observed, but the complete value is unresolved.
    Partial {
        /// Admitted subset of original declarations.
        value: T,
        /// Reason the observations do not establish completeness.
        reason: String,
        /// Sources of this incomplete observation.
        evidence: Vec<PythonMetadataEvidence>,
    },
    /// The declaration requires code execution or unsupported interpretation.
    Dynamic {
        /// Bounded reason or original expression.
        reason: String,
        /// Source that advertises this dynamic declaration.
        evidence: Vec<PythonMetadataEvidence>,
    },
    /// The selected packaging grammar does not declare this field.
    Omitted {
        /// Source files inspected for this field.
        evidence: Vec<PythonMetadataEvidence>,
    },
}

impl<T> PythonMetadataFact<T> {
    /// Returns only statically established declarations.
    #[must_use]
    pub const fn recorded(&self) -> Option<&T> {
        match self {
            Self::Recorded { value, .. } => Some(value),
            Self::Partial { .. } | Self::Dynamic { .. } | Self::Omitted { .. } => None,
        }
    }

    /// Returns the observed value, retaining incomplete subsets for inspection.
    #[must_use]
    pub const fn declared(&self) -> Option<&T> {
        match self {
            Self::Recorded { value, .. } | Self::Partial { value, .. } => Some(value),
            Self::Dynamic { .. } | Self::Omitted { .. } => None,
        }
    }
}

/// One original requirement declaration; resolution is a separate capability.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonDependencyDeclaration {
    /// Original PEP 508 declaration, retaining extras and environment markers.
    pub requirement: String,
    /// Runtime, optional, or build declaration class.
    pub scope: DependencyScope,
    /// Extra group when declared in an optional dependency table.
    pub group: Option<String>,
}

/// Unified static observations from pyproject.toml, setup.cfg, and setup.py.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PythonProjectMetadata {
    /// Selected packaging manifest path relative to the project root.
    pub manifest_path: String,
    /// All source documents consumed, including literal attribute sources.
    pub evidence: Vec<PythonMetadataEvidence>,
    /// Package name declaration.
    pub name: PythonMetadataFact<String>,
    /// Exact declared version; arbitrary dynamic values never become a fake version.
    pub version: PythonMetadataFact<String>,
    /// Package description.
    pub description: PythonMetadataFact<String>,
    /// Homepage declaration.
    pub homepage: PythonMetadataFact<String>,
    /// Documentation URL declaration.
    pub documentation: PythonMetadataFact<String>,
    /// Repository URL declaration.
    pub repository: PythonMetadataFact<String>,
    /// Interpreter requirement declaration, independent of compiler capability.
    pub requires_python: PythonMetadataFact<String>,
    /// Complete static dependency declarations or explicit incomplete availability.
    pub dependencies: PythonMetadataFact<Vec<PythonDependencyDeclaration>>,
}

impl PythonProjectMetadata {
    /// Validates field bounds and the source evidence graph before transport admission.
    ///
    /// # Errors
    /// Returns a product shape/bound error for malformed or detached declarations.
    pub fn admit(&self) -> Result<(), crate::ProductAdmissionError> {
        use crate::ProductAdmissionError as Error;
        if !safe_path(&self.manifest_path)
            || !matches!(
                self.manifest_path.as_str(),
                "pyproject.toml" | "setup.cfg" | "setup.py"
            )
            || self.evidence.is_empty()
            || self.evidence.len() > 8
            || !self
                .evidence
                .iter()
                .any(|source| source.path == self.manifest_path)
        {
            return Err(Error::PythonMetadata);
        }
        let mut paths = std::collections::BTreeSet::new();
        for source in &self.evidence {
            if !safe_path(&source.path)
                || source.bytes > 4 * 1024 * 1024
                || !paths.insert(&source.path)
            {
                return Err(Error::PythonMetadata);
            }
        }
        for fact in [
            &self.name,
            &self.version,
            &self.description,
            &self.homepage,
            &self.documentation,
            &self.repository,
            &self.requires_python,
        ] {
            admit_fact(fact, &self.evidence)?;
            // Partial observations are useful only for dependency collections.
            if matches!(fact, PythonMetadataFact::Partial { .. }) {
                return Err(Error::PythonMetadata);
            }
            if fact.recorded().is_some_and(|value| {
                value.len() > crate::MAX_PRODUCT_TEXT_BYTES || value.contains('\0')
            }) {
                return Err(Error::PythonMetadata);
            }
        }
        admit_fact(&self.dependencies, &self.evidence)?;
        if let Some(declarations) = self.dependencies.declared() {
            if declarations.len() > crate::MAX_PRODUCT_ROWS {
                return Err(Error::RowBound);
            }
            for declaration in declarations {
                if declaration.requirement.is_empty()
                    || declaration.requirement.len() > 8192
                    || declaration.requirement.contains(['\n', '\r', '\0'])
                    || !matches!(
                        declaration.scope,
                        DependencyScope::Runtime
                            | DependencyScope::Build
                            | DependencyScope::Development
                            | DependencyScope::Optional
                    )
                    || declaration.group.as_ref().is_some_and(|group| {
                        group.is_empty()
                            || group.len() > 256
                            || group.contains('\0')
                            || declaration.scope != DependencyScope::Optional
                    })
                {
                    return Err(Error::PythonMetadata);
                }
            }
        }
        Ok(())
    }

    /// Identity of selection, extraction policy and exact consumed documents.
    #[must_use]
    pub fn digest(&self) -> [u8; 32] {
        self.digest_with_policy(PYTHON_PROJECT_EXTRACTION_POLICY)
    }

    fn digest_with_policy(&self, policy: &[u8]) -> [u8; 32] {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"nudox.python-project.metadata.v2\0");
        hasher.update(&(policy.len() as u64).to_le_bytes());
        hasher.update(policy);
        hasher.update(&(self.manifest_path.len() as u64).to_le_bytes());
        hasher.update(self.manifest_path.as_bytes());
        for evidence in &self.evidence {
            hasher.update(&(evidence.path.len() as u64).to_le_bytes());
            hasher.update(evidence.path.as_bytes());
            hasher.update(&evidence.digest);
            hasher.update(&evidence.bytes.to_le_bytes());
        }
        *hasher.finalize().as_bytes()
    }
}

fn safe_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 4096
        && !path.starts_with('/')
        && !path.contains(['\\', '\0', ':'])
        && !path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}

fn admit_fact<T>(
    fact: &PythonMetadataFact<T>,
    sources: &[PythonMetadataEvidence],
) -> Result<(), crate::ProductAdmissionError> {
    let evidence = match fact {
        PythonMetadataFact::Recorded { evidence, .. }
        | PythonMetadataFact::Omitted { evidence } => evidence,
        PythonMetadataFact::Dynamic { reason, evidence }
        | PythonMetadataFact::Partial {
            reason, evidence, ..
        } => {
            if reason.is_empty() || reason.len() > 1024 || reason.contains('\0') {
                return Err(crate::ProductAdmissionError::PythonMetadata);
            }
            evidence
        }
    };
    if evidence.is_empty()
        || evidence.len() > sources.len()
        || evidence.iter().any(|source| !sources.contains(source))
        || evidence
            .iter()
            .enumerate()
            .any(|(index, source)| evidence[..index].contains(source))
    {
        return Err(crate::ProductAdmissionError::PythonMetadata);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metadata() -> PythonProjectMetadata {
        let evidence = PythonMetadataEvidence {
            path: "setup.cfg".to_owned(),
            digest: [7; 32],
            bytes: 42,
        };
        let omitted = PythonMetadataFact::Omitted {
            evidence: vec![evidence.clone()],
        };
        PythonProjectMetadata {
            manifest_path: "setup.cfg".to_owned(),
            evidence: vec![evidence.clone()],
            name: omitted.clone(),
            version: omitted.clone(),
            description: omitted.clone(),
            homepage: omitted.clone(),
            documentation: omitted.clone(),
            repository: omitted.clone(),
            requires_python: omitted,
            dependencies: PythonMetadataFact::Dynamic {
                reason: "unresolved declarations".to_owned(),
                evidence: vec![evidence],
            },
        }
    }

    #[test]
    fn metadata_identity_binds_manifest_selection_and_extraction_policy() {
        let mut cfg = metadata();
        cfg.evidence.push(PythonMetadataEvidence {
            path: "setup.py".to_owned(),
            digest: [8; 32],
            bytes: 43,
        });
        let mut setup = cfg.clone();
        setup.manifest_path = "setup.py".to_owned();
        cfg.admit().expect("cfg selection admitted");
        setup
            .admit()
            .expect("setup selection admitted with identical byte evidence");
        assert_ne!(cfg.digest(), setup.digest());
        assert_ne!(
            cfg.digest(),
            cfg.digest_with_policy(b"nudox.python-project.static.v1")
        );
        assert_eq!(
            cfg.digest(),
            cfg.digest_with_policy(PYTHON_PROJECT_EXTRACTION_POLICY)
        );
    }

    #[test]
    fn detached_unsafe_oversized_evidence_is_rejected() {
        let value = metadata();
        value.admit().expect("bounded source facts");
        let mut wrong = value.clone();
        wrong.evidence[0].path = "../outside.py".to_owned();
        assert!(wrong.admit().is_err());
        let mut wrong = value.clone();
        wrong.evidence[0].bytes = 4 * 1024 * 1024 + 1;
        assert!(wrong.admit().is_err());
        let mut wrong = value.clone();
        wrong.version = PythonMetadataFact::Recorded {
            value: "1".to_owned(),
            evidence: vec![PythonMetadataEvidence {
                path: "missing.py".to_owned(),
                digest: [8; 32],
                bytes: 4,
            }],
        };
        assert!(wrong.admit().is_err());
        let mut wrong = value.clone();
        wrong.description = PythonMetadataFact::Recorded {
            value: "x".repeat(crate::MAX_PRODUCT_TEXT_BYTES + 1),
            evidence: value.evidence.clone(),
        };
        assert!(wrong.admit().is_err());
        let mut wrong = value.clone();
        wrong.evidence.push(value.evidence[0].clone());
        assert!(wrong.admit().is_err());
        let mut wrong = value.clone();
        wrong.dependencies = PythonMetadataFact::Recorded {
            value: vec![PythonDependencyDeclaration {
                requirement: "bad\nrequirement".to_owned(),
                scope: DependencyScope::Runtime,
                group: None,
            }],
            evidence: value.evidence.clone(),
        };
        assert!(wrong.admit().is_err());
    }

    #[test]
    fn every_metadata_fact_variant_rejects_unknown_variant_fields() {
        let source = metadata().evidence;
        for fact in [
            PythonMetadataFact::Recorded {
                value: "literal".to_owned(),
                evidence: source.clone(),
            },
            PythonMetadataFact::Partial {
                value: "subset".to_owned(),
                reason: "incomplete".to_owned(),
                evidence: source.clone(),
            },
            PythonMetadataFact::Dynamic {
                reason: "computed".to_owned(),
                evidence: source.clone(),
            },
            PythonMetadataFact::Omitted {
                evidence: source.clone(),
            },
        ] {
            let mut value = serde_json::to_value(&fact).expect("typed fact");
            value["future_field"] = serde_json::json!(true);
            assert!(serde_json::from_value::<PythonMetadataFact<String>>(value).is_err());
        }
        let mut payload = serde_json::to_value(metadata()).expect("typed metadata");
        payload["name"]["future_field"] = serde_json::json!(true);
        assert!(serde_json::from_value::<PythonProjectMetadata>(payload).is_err());
    }

    #[test]
    fn partial_dependencies_retain_bounds_and_cannot_masquerade_as_scalar_facts() {
        let mut value = metadata();
        value.dependencies = PythonMetadataFact::Partial {
            value: vec![PythonDependencyDeclaration {
                requirement: "pytest>=8".to_owned(),
                scope: DependencyScope::Development,
                group: None,
            }],
            reason: "runtime declarations omitted".to_owned(),
            evidence: value.evidence.clone(),
        };
        value.admit().expect("bounded incomplete observations");
        assert!(value.dependencies.recorded().is_none());
        assert_eq!(
            value.dependencies.declared().expect("observations").len(),
            1
        );
        let mut invalid = value.clone();
        if let PythonMetadataFact::Partial { value, .. } = &mut invalid.dependencies {
            value[0].requirement = "bad\nrequirement".to_owned();
        }
        assert!(invalid.admit().is_err());
        value.version = PythonMetadataFact::Partial {
            value: "1".to_owned(),
            reason: "incomplete scalar".to_owned(),
            evidence: value.evidence.clone(),
        };
        assert!(value.admit().is_err());
    }

    #[test]
    fn old_profile_payload_defaults_source_metadata_and_new_payload_size_counts_it() {
        let old = r#"{"result":"package-profile","data":{"latest":null,"versions":0,"candidate_authority":null}}"#;
        let reply: crate::SurfaceReply = serde_json::from_str(old).expect("old additive payload");
        assert!(matches!(
            reply,
            crate::SurfaceReply::PackageProfile {
                source_metadata: None,
                ..
            }
        ));
        let new = crate::SurfaceReply::PackageProfile {
            latest: None,
            versions: 0,
            candidate_authority: None,
            source_metadata: Some(metadata()),
        };
        new.admit(crate::CommandId::PackageProfile)
            .expect("dynamic source metadata remains valid");
        assert!(new.encoded_size_bound() >= serde_json::to_vec(&new).expect("payload").len());
        let redecoded: crate::SurfaceReply =
            serde_json::from_slice(&serde_json::to_vec(&new).expect("serialize"))
                .expect("round-trip typed fields");
        assert_eq!(new, redecoded);
    }
}
