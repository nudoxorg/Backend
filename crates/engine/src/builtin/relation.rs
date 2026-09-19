//! Typed source relations and owner-held lazy source snapshots.

use super::complete_coverage;
use crate::workspace::{TransitionWork, WorkspaceRelationHandle, WorkspaceSnapshot};
pub use backend_compile::{DeclarationKind, SourceDeclaration, SourceLanguage, SourceLocation};
use backend_execution::AuthorityVersion;
use backend_replication::ImmutableObjectSchema;
use backend_version::{
    CanonicalRelation, CoverageWitness, Relation, RelationState, StateRoot, WorkspaceRoot,
};
use std::sync::Arc;

/// Authoritative package/source coordinates retained by the product
/// workspace. Product input identities are always rooted in this relation;
/// the echo fixture's numeric relation is kept separate above.
#[derive(Debug)]
pub struct ProductSourceRelation;

/// Canonical project or file metadata retained by the product source relation.
///
/// A project record owns the sorted file-key frontier for O(changed files)
/// reconciliation. Each file is an independent relation value and therefore
/// changes without rewriting the other files or their declaration payloads.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProductSourceRecord {
    /// One indexed project and the exact set of file rows it selects.
    Project {
        /// Canonical user-facing project coordinate.
        label: String,
        /// Digest over the sorted file identities and content versions.
        source_version: [u8; 32],
        /// Digest over canonical package manifest inputs.
        graph_version: [u8; 32],
        /// Sorted relation keys for every selected source file.
        files: Arc<[[u8; 32]]>,
        /// Sorted relation keys for package, dependency, and repository facts.
        facts: Arc<[[u8; 32]]>,
    },
    /// One independently versioned source file.
    File {
        /// Project key that owns this file.
        project: [u8; 32],
        /// Canonical project-relative path.
        path: String,
        /// Closed language authority.
        language: SourceLanguage,
        /// Content digest of the exact source bytes.
        content_version: [u8; 32],
        /// Identity of the parser, grammar, and extraction contract.
        analysis_version: [u8; 32],
        /// Ordered declarations extracted from this file.
        declarations: Arc<[SourceDeclaration]>,
    },
    /// One package manifest in a project or workspace.
    Package {
        /// Project key that owns this package.
        project: [u8; 32],
        /// Stable project-relative manifest path.
        manifest_path: Arc<str>,
        /// Registry/forge package name.
        name: Arc<str>,
        /// Exact declared package version.
        version: Arc<str>,
        /// Package description retained for search and package surfaces.
        description: Arc<str>,
        /// Declared license expression.
        license: Arc<str>,
        /// Canonical repository coordinate when declared.
        repository: Arc<str>,
    },
    /// One directed dependency edge. The edge is independently versioned so
    /// changing a requirement never rewrites its package or sibling edges.
    Dependency {
        /// Project key that owns the workspace.
        project: [u8; 32],
        /// Package fact key that owns this outgoing edge.
        package: [u8; 32],
        /// Name used by source code (including Cargo renames).
        alias: Arc<str>,
        /// Upstream package name.
        name: Arc<str>,
        /// Exact requested version requirement.
        requirement: Arc<str>,
        /// Cargo dependency lane.
        kind: ProductDependencyKind,
        /// Optional target predicate.
        target: Arc<str>,
        /// Interned registry, git, or path source fact.
        source: Option<[u8; 32]>,
        /// Whether the dependency is feature-gated.
        optional: bool,
        /// Whether its default feature set is enabled.
        default_features: bool,
        /// Sorted, deduplicated explicitly enabled features.
        features: Arc<[String]>,
    },
    /// One content-addressed registry, git, or path coordinate shared by all
    /// dependency edges in a project that use it.
    DependencySource {
        /// Project key that owns this source coordinate.
        project: [u8; 32],
        /// Canonical source coordinate.
        coordinate: Arc<str>,
    },
}

/// Closed dependency lane retained in canonical package graph edges.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ProductDependencyKind {
    /// Runtime and compile-time dependency.
    Normal,
    /// Build-script dependency.
    Build,
    /// Test/example/benchmark dependency.
    Development,
}

impl ProductDependencyKind {
    const fn wire_tag(self) -> u8 {
        match self {
            Self::Normal => 1,
            Self::Build => 2,
            Self::Development => 3,
        }
    }

    const fn from_wire_tag(tag: u8) -> Option<Self> {
        match tag {
            1 => Some(Self::Normal),
            2 => Some(Self::Build),
            3 => Some(Self::Development),
            _ => None,
        }
    }
}

/// Borrowed project frontier exposed without tuple-position coupling.
#[derive(Clone, Copy, Debug)]
pub struct ProductProjectRef<'a> {
    /// Canonical user-facing project coordinate.
    pub label: &'a str,
    /// Digest over the selected file identities and content versions.
    pub source_version: [u8; 32],
    /// Digest over canonical package manifest inputs.
    pub graph_version: [u8; 32],
    /// Sorted relation keys for the selected files.
    pub files: &'a [[u8; 32]],
    /// Sorted relation keys for canonical package graph facts.
    pub facts: &'a [[u8; 32]],
}

/// Borrowed source-file fields exposed without cloning declaration storage.
#[derive(Clone, Copy, Debug)]
pub struct ProductFileRef<'a> {
    /// Project relation key that owns the file.
    pub project: [u8; 32],
    /// Canonical project-relative path.
    pub path: &'a str,
    /// Closed language authority.
    pub language: SourceLanguage,
    /// Digest of the exact source bytes.
    pub content_version: [u8; 32],
    /// Parser, grammar, and extraction contract identity.
    pub analysis_version: [u8; 32],
    /// Shared declaration storage.
    pub declarations: &'a Arc<[SourceDeclaration]>,
}

impl ProductSourceRecord {
    /// Maximum admitted coordinate or relative-path length.
    pub const MAX_LABEL_BYTES: usize = 4096;
    /// Maximum source files retained by one project record.
    pub const MAX_PROJECT_FILES: usize = 100_000;
    /// Maximum independently versioned package graph facts per project.
    pub const MAX_PROJECT_FACTS: usize = 250_000;
    /// Maximum dependency features retained on one edge.
    pub const MAX_DEPENDENCY_FEATURES: usize = 4096;
    /// Maximum declarations retained in one source file.
    pub const MAX_FILE_DECLARATIONS: usize = 16_384;

    /// Constructs a checked empty project record for compatibility package
    /// coordinates.
    /// # Errors
    ///
    /// Returns an error when the coordinate is empty or oversized.
    pub fn new(label: impl Into<String>) -> Result<Self, String> {
        Self::project(label, [0; 32], Vec::new())
    }

    /// Constructs a checked project frontier.
    ///
    /// # Errors
    /// Returns an error for an invalid label or unsorted, duplicate, or
    /// oversized file-key frontier.
    pub fn project(
        label: impl Into<String>,
        source_version: [u8; 32],
        files: impl Into<Vec<[u8; 32]>>,
    ) -> Result<Self, String> {
        Self::project_with_graph_facts(label, source_version, [0; 32], files, Vec::new())
    }

    /// Constructs a checked project with independent source and graph
    /// frontiers.
    ///
    /// # Errors
    /// Returns an error for invalid text or a duplicate, unordered, or
    /// oversized frontier.
    pub fn project_with_facts(
        label: impl Into<String>,
        source_version: [u8; 32],
        files: impl Into<Vec<[u8; 32]>>,
        facts: impl Into<Vec<[u8; 32]>>,
    ) -> Result<Self, String> {
        Self::project_with_graph_facts(label, source_version, [0; 32], files, facts)
    }

    /// Constructs a checked project with independently invalidated source and
    /// package graph frontiers.
    ///
    /// # Errors
    /// Returns an error for invalid text or a duplicate, unordered, or
    /// oversized frontier.
    pub fn project_with_graph_facts(
        label: impl Into<String>,
        source_version: [u8; 32],
        graph_version: [u8; 32],
        files: impl Into<Vec<[u8; 32]>>,
        facts: impl Into<Vec<[u8; 32]>>,
    ) -> Result<Self, String> {
        let label = label.into();
        let files = files.into();
        let facts = facts.into();
        if label.is_empty() || label.len() > Self::MAX_LABEL_BYTES {
            return Err("project source coordinate is empty or oversized".to_owned());
        }
        if files.len() > Self::MAX_PROJECT_FILES
            || files.windows(2).any(|window| window[0] >= window[1])
        {
            return Err("project file frontier is unordered or oversized".to_owned());
        }
        if facts.len() > Self::MAX_PROJECT_FACTS
            || facts.windows(2).any(|window| window[0] >= window[1])
        {
            return Err("project fact frontier is unordered or oversized".to_owned());
        }
        Ok(Self::Project {
            label,
            source_version,
            graph_version,
            files: Arc::from(files.into_boxed_slice()),
            facts: Arc::from(facts.into_boxed_slice()),
        })
    }

    /// Constructs one checked package manifest fact.
    ///
    /// # Errors
    /// Returns an error when identity fields are empty or any text is too
    /// large for the canonical relation contract.
    pub fn package(
        project: [u8; 32],
        manifest_path: impl Into<String>,
        name: impl Into<String>,
        version: impl Into<String>,
        description: impl Into<String>,
        license: impl Into<String>,
        repository: impl Into<String>,
    ) -> Result<Self, String> {
        let values = [
            manifest_path.into(),
            name.into(),
            version.into(),
            description.into(),
            license.into(),
            repository.into(),
        ];
        if values[0].is_empty()
            || values[1].is_empty()
            || values
                .iter()
                .any(|value| value.len() > Self::MAX_LABEL_BYTES)
        {
            return Err("package fact contains empty identity or oversized text".to_owned());
        }
        let [
            manifest_path,
            name,
            version,
            description,
            license,
            repository,
        ] = values;
        Ok(Self::Package {
            project,
            manifest_path: Arc::from(manifest_path),
            name: Arc::from(name),
            version: Arc::from(version),
            description: Arc::from(description),
            license: Arc::from(license),
            repository: Arc::from(repository),
        })
    }

    /// Constructs one checked dependency edge.
    ///
    /// # Errors
    /// Returns an error when identity fields, feature names, or counts exceed
    /// the canonical relation contract.
    #[allow(clippy::too_many_arguments)]
    pub fn dependency(
        project: [u8; 32],
        package: [u8; 32],
        alias: impl Into<String>,
        name: impl Into<String>,
        requirement: impl Into<String>,
        kind: ProductDependencyKind,
        target: impl Into<String>,
        source: Option<[u8; 32]>,
        optional: bool,
        default_features: bool,
        features: impl Into<Vec<String>>,
    ) -> Result<Self, String> {
        let values = [alias.into(), name.into(), requirement.into(), target.into()];
        let mut features = features.into();
        features.sort();
        features.dedup();
        if values[0].is_empty()
            || values[1].is_empty()
            || values
                .iter()
                .any(|value| value.len() > Self::MAX_LABEL_BYTES)
            || features.len() > Self::MAX_DEPENDENCY_FEATURES
            || features
                .iter()
                .any(|feature| feature.is_empty() || feature.len() > Self::MAX_LABEL_BYTES)
        {
            return Err("dependency edge contains invalid or oversized data".to_owned());
        }
        let [alias, name, requirement, target] = values;
        Ok(Self::Dependency {
            project,
            package,
            alias: Arc::from(alias),
            name: Arc::from(name),
            requirement: Arc::from(requirement),
            kind,
            target: Arc::from(target),
            source,
            optional,
            default_features,
            features: Arc::from(features.into_boxed_slice()),
        })
    }

    /// Constructs one interned dependency source coordinate.
    ///
    /// # Errors
    /// Returns an error when the coordinate is empty or oversized.
    pub fn dependency_source(
        project: [u8; 32],
        coordinate: impl Into<String>,
    ) -> Result<Self, String> {
        let coordinate = coordinate.into();
        if coordinate.is_empty() || coordinate.len() > Self::MAX_LABEL_BYTES {
            return Err("dependency source coordinate is empty or oversized".to_owned());
        }
        Ok(Self::DependencySource {
            project,
            coordinate: Arc::from(coordinate),
        })
    }

    /// Constructs a checked file record.
    ///
    /// # Errors
    /// Returns an error for an invalid path or oversized declaration set.
    pub fn file(
        project: [u8; 32],
        path: impl Into<String>,
        language: SourceLanguage,
        content_version: [u8; 32],
        analysis_version: [u8; 32],
        declarations: impl Into<Arc<[SourceDeclaration]>>,
    ) -> Result<Self, String> {
        let path = path.into();
        let declarations = declarations.into();
        if path.is_empty() || path.len() > Self::MAX_LABEL_BYTES {
            return Err("source file path is empty or oversized".to_owned());
        }
        if declarations.len() > Self::MAX_FILE_DECLARATIONS {
            return Err("source file declaration set is oversized".to_owned());
        }
        Ok(Self::File {
            project,
            path,
            language,
            content_version,
            analysis_version,
            declarations,
        })
    }

    /// Returns the display coordinate for this project or file.
    #[must_use]
    pub fn label(&self) -> &str {
        match self {
            Self::Project { label, .. } => label,
            Self::File { path, .. } => path,
            Self::Package { name, .. } | Self::Dependency { name, .. } => name,
            Self::DependencySource { coordinate, .. } => coordinate,
        }
    }

    /// Returns the project frontier fields when this is a project record.
    #[must_use]
    pub fn project_fields(&self) -> Option<ProductProjectRef<'_>> {
        match self {
            Self::Project {
                label,
                source_version,
                graph_version,
                files,
                facts,
            } => Some(ProductProjectRef {
                label,
                source_version: *source_version,
                graph_version: *graph_version,
                files,
                facts,
            }),
            Self::File { .. }
            | Self::Package { .. }
            | Self::Dependency { .. }
            | Self::DependencySource { .. } => None,
        }
    }

    /// Returns the file fields when this is a file record.
    #[must_use]
    pub fn file_fields(&self) -> Option<ProductFileRef<'_>> {
        match self {
            Self::File {
                project,
                path,
                language,
                content_version,
                analysis_version,
                declarations,
            } => Some(ProductFileRef {
                project: *project,
                path,
                language: *language,
                content_version: *content_version,
                analysis_version: *analysis_version,
                declarations,
            }),
            Self::Project { .. }
            | Self::Package { .. }
            | Self::Dependency { .. }
            | Self::DependencySource { .. } => None,
        }
    }
}

impl Relation for ProductSourceRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 2;
    type Key = [u8; 32];
    type Value = ProductSourceRecord;

    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }

    #[allow(clippy::too_many_lines)]
    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(b"PSR5");
        match value {
            ProductSourceRecord::Project {
                label,
                source_version,
                graph_version,
                files,
                facts,
            } => {
                output.push(1);
                push_text(output, label);
                output.extend_from_slice(source_version);
                output.extend_from_slice(graph_version);
                push_count(output, files.len());
                for file in files.iter() {
                    output.extend_from_slice(file);
                }
                push_count(output, facts.len());
                for fact in facts.iter() {
                    output.extend_from_slice(fact);
                }
            }
            ProductSourceRecord::File {
                project,
                path,
                language,
                content_version,
                analysis_version,
                declarations,
            } => {
                output.push(2);
                output.extend_from_slice(project);
                push_text(output, path);
                output.push(language.wire_tag());
                output.extend_from_slice(content_version);
                output.extend_from_slice(analysis_version);
                push_count(output, declarations.len());
                for declaration in declarations.iter() {
                    output.extend_from_slice(&declaration.line().to_be_bytes());
                    if declaration.location().path() == path {
                        push_text(output, declaration.location().path());
                    } else {
                        // The file relation remains the authority for the
                        // selected path.  Retaining a declaration's typed
                        // location is useful for callers, but an inconsistent
                        // path must not silently alter the file identity.
                        push_text(output, path);
                    }
                    push_text(output, declaration.name());
                    output.push(declaration.kind().wire_tag());
                    push_text(output, declaration.signature());
                    push_text(output, declaration.documentation());
                }
            }
            ProductSourceRecord::Package {
                project,
                manifest_path,
                name,
                version,
                description,
                license,
                repository,
            } => {
                output.push(3);
                output.extend_from_slice(project);
                push_text(output, manifest_path);
                push_text(output, name);
                push_text(output, version);
                push_text(output, description);
                push_text(output, license);
                push_text(output, repository);
            }
            ProductSourceRecord::Dependency {
                project,
                package,
                alias,
                name,
                requirement,
                kind,
                target,
                source,
                optional,
                default_features,
                features,
            } => {
                output.push(4);
                output.extend_from_slice(project);
                output.extend_from_slice(package);
                push_text(output, alias);
                push_text(output, name);
                push_text(output, requirement);
                output.push(kind.wire_tag());
                push_text(output, target);
                match source {
                    Some(source) => {
                        output.push(1);
                        output.extend_from_slice(source);
                    }
                    None => output.push(0),
                }
                output.push(u8::from(*optional));
                output.push(u8::from(*default_features));
                push_count(output, features.len());
                for feature in features.iter() {
                    push_text(output, feature);
                }
            }
            ProductSourceRecord::DependencySource {
                project,
                coordinate,
            } => {
                output.push(5);
                output.extend_from_slice(project);
                push_text(output, coordinate);
            }
        }
    }
}

impl CanonicalRelation for ProductSourceRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, backend_version::RelationDecodeError> {
        bytes
            .try_into()
            .map_err(|_| backend_version::RelationDecodeError::Malformed)
    }

    fn decode_value(bytes: &[u8]) -> Result<Self::Value, backend_version::RelationDecodeError> {
        decode_source_record(bytes).map_err(|()| backend_version::RelationDecodeError::Malformed)
    }
}

/// Derives the relation key for a project-relative source file.
#[must_use]
pub fn product_source_file_key(project: [u8; 32], path: &str) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.product-source.file.v1\0");
    hasher.update(&project);
    hasher.update(&(path.len() as u64).to_be_bytes());
    hasher.update(path.as_bytes());
    *hasher.finalize().as_bytes()
}

/// Derives the stable key for a package manifest within a project.
#[must_use]
pub fn product_package_key(project: [u8; 32], manifest_path: &str) -> [u8; 32] {
    product_fact_key(
        b"backend.product.package.v1\0",
        &[&project, manifest_path.as_bytes()],
    )
}

/// Derives the stable key for a dependency identity. Requirement, source,
/// flags, and features deliberately stay out of the key so their edits become
/// one-row replacements.
#[must_use]
pub fn product_dependency_key(
    project: [u8; 32],
    package: [u8; 32],
    alias: &str,
    kind: ProductDependencyKind,
    target: &str,
) -> [u8; 32] {
    let kind = [kind.wire_tag()];
    product_fact_key(
        b"backend.product.dependency.v1\0",
        &[
            &project,
            &package,
            alias.as_bytes(),
            &kind,
            target.as_bytes(),
        ],
    )
}

/// Derives the stable key for an interned dependency source coordinate.
#[must_use]
pub fn product_dependency_source_key(project: [u8; 32], coordinate: &str) -> [u8; 32] {
    product_fact_key(
        b"backend.product.dependency-source.v1\0",
        &[&project, coordinate.as_bytes()],
    )
}

fn product_fact_key(domain: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(domain);
    for part in parts {
        hasher.update(&(part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    *hasher.finalize().as_bytes()
}

fn push_count(output: &mut Vec<u8>, count: usize) {
    output.extend_from_slice(&u32::try_from(count).unwrap_or(u32::MAX).to_be_bytes());
}

fn push_text(output: &mut Vec<u8>, value: &str) {
    push_count(output, value.len());
    output.extend_from_slice(value.as_bytes());
}

#[allow(clippy::too_many_lines)]
fn decode_source_record(bytes: &[u8]) -> Result<ProductSourceRecord, ()> {
    let mut reader = SourceReader::new(bytes);
    let format = reader.take(4)?;
    if format != b"PSR2" && format != b"PSR3" && format != b"PSR4" && format != b"PSR5" {
        return Err(());
    }
    let record = match reader.byte()? {
        1 => {
            let label = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let source_version = reader.array()?;
            let graph_version = if format == b"PSR5" {
                reader.array()?
            } else {
                [0; 32]
            };
            let count = reader.count(ProductSourceRecord::MAX_PROJECT_FILES)?;
            let mut files = Vec::with_capacity(count);
            for _ in 0..count {
                files.push(reader.array()?);
            }
            let facts = if format == b"PSR5" {
                let count = reader.count(ProductSourceRecord::MAX_PROJECT_FACTS)?;
                let mut facts = Vec::with_capacity(count);
                for _ in 0..count {
                    facts.push(reader.array()?);
                }
                facts
            } else {
                Vec::new()
            };
            ProductSourceRecord::project_with_graph_facts(
                label,
                source_version,
                graph_version,
                files,
                facts,
            )
            .map_err(|_| ())?
        }
        2 => {
            let project = reader.array()?;
            let path = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let language = SourceLanguage::from_wire_tag(reader.byte()?).ok_or(())?;
            let content_version = reader.array()?;
            let analysis_version = if format == b"PSR3" || format == b"PSR4" || format == b"PSR5" {
                reader.array()?
            } else {
                [0; 32]
            };
            let count = reader.count(ProductSourceRecord::MAX_FILE_DECLARATIONS)?;
            let mut declarations = Vec::with_capacity(count);
            for _ in 0..count {
                let line = reader.u32()?;
                let declaration_path = if format == b"PSR4" || format == b"PSR5" {
                    reader.text(SourceLocation::MAX_PATH_BYTES)?
                } else {
                    path.clone()
                };
                if declaration_path != path {
                    return Err(());
                }
                let name = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
                let kind = if format == b"PSR4" || format == b"PSR5" {
                    DeclarationKind::from_wire_tag(reader.byte()?).ok_or(())?
                } else {
                    DeclarationKind::from_name(&reader.text(SourceDeclaration::MAX_TEXT_BYTES)?)
                };
                let signature = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
                let documentation = reader.text(SourceDeclaration::MAX_TEXT_BYTES)?;
                declarations.push(
                    SourceDeclaration::with_location(
                        SourceLocation::new(declaration_path, line).map_err(|_| ())?,
                        name,
                        kind.name(),
                        signature,
                        documentation,
                    )
                    .map_err(|_| ())?,
                );
            }
            ProductSourceRecord::file(
                project,
                path,
                language,
                content_version,
                analysis_version,
                declarations,
            )
            .map_err(|_| ())?
        }
        3 if format == b"PSR5" => ProductSourceRecord::package(
            reader.array()?,
            reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?,
            reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?,
            reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?,
            reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?,
            reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?,
            reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?,
        )
        .map_err(|_| ())?,
        4 if format == b"PSR5" => {
            let project = reader.array()?;
            let package = reader.array()?;
            let alias = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let name = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let requirement = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let kind = ProductDependencyKind::from_wire_tag(reader.byte()?).ok_or(())?;
            let target = reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?;
            let source = match reader.byte()? {
                0 => None,
                1 => Some(reader.array()?),
                _ => return Err(()),
            };
            let optional = reader.boolean()?;
            let default_features = reader.boolean()?;
            let count = reader.count(ProductSourceRecord::MAX_DEPENDENCY_FEATURES)?;
            let mut features = Vec::with_capacity(count);
            for _ in 0..count {
                features.push(reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?);
            }
            ProductSourceRecord::dependency(
                project,
                package,
                alias,
                name,
                requirement,
                kind,
                target,
                source,
                optional,
                default_features,
                features,
            )
            .map_err(|_| ())?
        }
        5 if format == b"PSR5" => ProductSourceRecord::dependency_source(
            reader.array()?,
            reader.text(ProductSourceRecord::MAX_LABEL_BYTES)?,
        )
        .map_err(|_| ())?,
        _ => return Err(()),
    };
    reader.finish()?;
    Ok(record)
}

struct SourceReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> SourceReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], ()> {
        let end = self.at.checked_add(length).ok_or(())?;
        let value = self.bytes.get(self.at..end).ok_or(())?;
        self.at = end;
        Ok(value)
    }

    fn byte(&mut self) -> Result<u8, ()> {
        self.take(1)?.first().copied().ok_or(())
    }

    fn boolean(&mut self) -> Result<bool, ()> {
        match self.byte()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(()),
        }
    }

    fn u32(&mut self) -> Result<u32, ()> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| ())?,
        ))
    }

    fn count(&mut self, maximum: usize) -> Result<usize, ()> {
        let count = self.u32()? as usize;
        (count <= maximum).then_some(count).ok_or(())
    }

    fn array(&mut self) -> Result<[u8; 32], ()> {
        self.take(32)?.try_into().map_err(|_| ())
    }

    fn text(&mut self, maximum: usize) -> Result<String, ()> {
        let length = self.count(maximum)?;
        String::from_utf8(self.take(length)?.to_vec()).map_err(|_| ())
    }

    fn finish(self) -> Result<(), ()> {
        (self.at == self.bytes.len()).then_some(()).ok_or(())
    }
}

/// Exact compact source-change facts retained with a checked workspace head.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProductSourceDeltaFacts {
    /// Number of logical source items changed by the selected transition.
    pub changed_items: u64,
    /// Number of canonical nodes rebuilt by the selected transition.
    pub changed_nodes: u64,
    /// Bytes emitted in the changed canonical frontier.
    pub changed_bytes: u64,
}

impl ProductSourceDeltaFacts {
    pub(crate) fn from_transition(work: TransitionWork) -> Self {
        Self {
            changed_items: work.changed_items(),
            changed_nodes: work.changed_nodes(),
            changed_bytes: work.changed_bytes(),
        }
    }
}

/// Authenticated local-retention facts carried by a checked product source.
///
/// These facts describe the selected source arrangement and closure that the
/// owner can actually retain.  They are read from authenticated root
/// summaries, so placement code does not substitute request cardinalities or
/// scan the complete relation before choosing a route.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ProductSourceRetentionFacts {
    /// Rows available from the selected product relation root.
    pub relation_rows: u64,
    /// Height of the selected relation root.
    pub relation_level: u16,
    /// Object descriptors retained by the selected workspace closure.
    pub retained_objects: u64,
}

/// Exact source-lane transition roots selected by an admitted workspace
/// snapshot. The raw bytes stay private; consumers can only ask whether both
/// roots match their already typed prior/current roots.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductSourceTransition {
    base: [u8; backend_version::ID_BYTES],
    target: [u8; backend_version::ID_BYTES],
}

impl ProductSourceTransition {
    /// Returns whether this checked transition is adjacent to the supplied
    /// prior and current product relation roots.
    #[must_use]
    pub fn binds(
        self,
        prior: StateRoot<ProductSourceRelation>,
        target: StateRoot<ProductSourceRelation>,
    ) -> bool {
        self.base == *prior.as_bytes() && self.target == *target.as_bytes()
    }
}

/// A checked source relation selected by an owner workspace head.
///
/// The handle keeps the store and authenticated relation root alive while
/// callers perform bounded page reads. It does not copy relation rows.
#[derive(Clone, Debug)]
pub struct ProductSourceSnapshot {
    workspace: WorkspaceRoot,
    transition: ProductSourceTransition,
    relation: WorkspaceRelationHandle<ProductSourceRelation>,
    manifest: Arc<[u8]>,
    authority: [u8; backend_version::ID_BYTES],
    delta: ProductSourceDeltaFacts,
    retention: ProductSourceRetentionFacts,
}

impl ProductSourceSnapshot {
    /// Opens the product source selected by a checked workspace snapshot.
    /// # Errors
    ///
    /// Returns an error when validation, persistence, or admission of the
    /// supplied value fails.
    pub fn from_workspace(snapshot: &WorkspaceSnapshot) -> Result<Self, String> {
        snapshot
            .manifest()
            .validate()
            .map_err(|error| error.to_string())?;
        let relation = snapshot
            .relation::<ProductSourceRelation>()
            .map_err(|error| error.to_string())?;
        let relation_root = relation.root_node().map_err(|error| error.to_string())?;
        let (base, target) = snapshot
            .transition_relation_roots::<ProductSourceRelation>()
            .ok_or_else(|| "selected workspace has no product source transition".to_owned())?;
        if target != *relation.root().as_bytes() {
            return Err(
                "product source transition target differs from selected relation".to_owned(),
            );
        }
        Ok(Self {
            workspace: snapshot.root(),
            transition: ProductSourceTransition { base, target },
            relation,
            manifest: Arc::from(snapshot.manifest().encode().into_boxed_slice()),
            authority: *snapshot.manifest().authority(),
            delta: ProductSourceDeltaFacts::from_transition(snapshot.transition_work()),
            retention: ProductSourceRetentionFacts {
                relation_rows: relation_root.row_count(),
                relation_level: relation_root.level(),
                retained_objects: snapshot.closure().manifest().object_count(),
            },
        })
    }

    /// Returns the checked workspace root that selected this source.
    #[must_use]
    pub const fn workspace_root(&self) -> WorkspaceRoot {
        self.workspace
    }

    /// Returns the exact checked source relation transition.
    #[must_use]
    pub const fn transition(&self) -> ProductSourceTransition {
        self.transition
    }

    /// Returns the checked product relation root.
    #[must_use]
    pub fn relation_root(&self) -> StateRoot<ProductSourceRelation> {
        self.relation.root()
    }

    /// Returns the owner-held lazy relation handle for bounded page reads.
    #[must_use]
    pub const fn relation(&self) -> &WorkspaceRelationHandle<ProductSourceRelation> {
        &self.relation
    }

    /// Returns exact source change facts carried by the selected head.
    #[must_use]
    pub const fn delta_facts(&self) -> ProductSourceDeltaFacts {
        self.delta
    }

    /// Returns authenticated local arrangement and closure retention facts.
    #[must_use]
    pub const fn retention_facts(&self) -> ProductSourceRetentionFacts {
        self.retention
    }

    /// Returns the exact checked workspace manifest selected with this source.
    #[must_use]
    pub fn manifest_bytes(&self) -> &[u8] {
        &self.manifest
    }

    /// Returns the authority identity admitted by the selected workspace
    /// manifest.
    #[must_use]
    pub const fn authority_bytes(&self) -> &[u8; backend_version::ID_BYTES] {
        &self.authority
    }

    /// Returns the typed immutable input capability bound to this source.
    #[must_use]
    pub fn input(&self) -> ProductInput {
        ProductInput {
            root: self.relation.root(),
        }
    }
}

/// Typed product input capability derived from an admitted source relation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProductInput {
    root: StateRoot<ProductSourceRelation>,
}

impl ProductInput {
    /// Creates an input capability from a checked relation state.
    #[must_use]
    pub fn from_checked_relation(relation: &RelationState<ProductSourceRelation>) -> Self {
        Self {
            root: relation.root(),
        }
    }

    /// Returns the checked source relation root.
    #[must_use]
    pub const fn root(&self) -> StateRoot<ProductSourceRelation> {
        self.root
    }
}

/// Immutable object schema used for product relation input transfer.
pub type BuiltinInputSchema = ImmutableObjectSchema;

/// Builds a checked Product-shaped source fixture for compatibility adapters.
pub(super) fn product_source_fixture(
    expanded: bool,
) -> Result<RelationState<ProductSourceRelation>, String> {
    let entries = source_entries(expanded);
    RelationState::from_entries(
        entries,
        CoverageWitness::Partial(backend_version::partial_coverage(1)),
    )
    .map_err(|error| error.to_string())
}

/// Builds a Product-shaped source fixture with complete authority coverage.
pub(super) fn product_source_fixture_with_authority(
    expanded: bool,
    authority: AuthorityVersion,
) -> Result<RelationState<ProductSourceRelation>, String> {
    RelationState::from_entries(source_entries(expanded), complete_coverage(authority)?)
        .map_err(|error| error.to_string())
}

fn source_entries(expanded: bool) -> Vec<([u8; 32], ProductSourceRecord)> {
    let mut entries = vec![(
        backend_library::package_key("backend-builtin").to_bytes(),
        ProductSourceRecord::Project {
            label: "backend-builtin".to_owned(),
            source_version: [0; 32],
            graph_version: [0; 32],
            files: Arc::from([]),
            facts: Arc::from([]),
        },
    )];
    if expanded {
        entries.push((
            backend_library::package_key("backend-extra").to_bytes(),
            ProductSourceRecord::Project {
                label: "backend-extra".to_owned(),
                source_version: [0; 32],
                graph_version: [0; 32],
                files: Arc::from([]),
                facts: Arc::from([]),
            },
        ));
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::{
        DeclarationKind, ProductDependencyKind, ProductSourceRecord, ProductSourceRelation,
        Relation, SourceDeclaration, SourceLocation, product_dependency_key,
        product_dependency_source_key, product_package_key,
    };
    use backend_version::CanonicalRelation;
    use std::sync::Arc;

    #[test]
    fn source_relation_round_trips_typed_declaration_metadata() {
        let declaration = SourceDeclaration::at_path(
            "src/lib.rs",
            "answer",
            "function",
            7,
            "fn answer() -> u32",
            "the answer",
        )
        .expect("declaration");
        let record = ProductSourceRecord::file(
            [3; 32],
            "src/lib.rs",
            super::SourceLanguage::Rust,
            [4; 32],
            [5; 32],
            Arc::from([declaration]),
        )
        .expect("file");
        let mut encoded = Vec::new();
        ProductSourceRelation::encode_value(&record, &mut encoded);
        assert_eq!(&encoded[..4], b"PSR5");
        let decoded = ProductSourceRelation::decode_value(&encoded).expect("decode");
        let fields = decoded.file_fields().expect("file fields");
        let declaration = &fields.declarations[0];
        assert_eq!(declaration.kind(), DeclarationKind::Function);
        assert_eq!(declaration.location().path(), "src/lib.rs");
        assert_eq!(declaration.location().start_line(), 7);
        assert_eq!(
            SourceLocation::new("src/lib.rs", 7).expect("location"),
            *declaration.location()
        );
    }

    #[test]
    fn package_graph_round_trips_without_coupling_edge_value_to_identity() {
        let project = [7; 32];
        let package = product_package_key(project, "Cargo.toml");
        let source = product_dependency_source_key(project, "registry+crates.io");
        let key = product_dependency_key(
            project,
            package,
            "wire",
            ProductDependencyKind::Normal,
            "cfg(unix)",
        );
        let record = ProductSourceRecord::dependency(
            project,
            package,
            "wire",
            "serde",
            "^1",
            ProductDependencyKind::Normal,
            "cfg(unix)",
            Some(source),
            true,
            false,
            vec!["derive".to_owned(), "derive".to_owned()],
        )
        .expect("dependency");
        let mut encoded = Vec::new();
        ProductSourceRelation::encode_value(&record, &mut encoded);
        assert_eq!(
            ProductSourceRelation::decode_value(&encoded).expect("decode"),
            record
        );
        assert_eq!(
            key,
            product_dependency_key(
                project,
                package,
                "wire",
                ProductDependencyKind::Normal,
                "cfg(unix)"
            )
        );
        let changed = ProductSourceRecord::dependency(
            project,
            package,
            "wire",
            "serde",
            "^2",
            ProductDependencyKind::Normal,
            "cfg(unix)",
            Some(source),
            true,
            false,
            vec!["derive".to_owned()],
        )
        .expect("changed dependency");
        assert_ne!(changed, record);
    }

    #[test]
    fn project_frontiers_reject_duplicate_fact_keys() {
        assert!(
            ProductSourceRecord::project_with_facts(
                "project",
                [0; 32],
                Vec::new(),
                vec![[1; 32], [1; 32]],
            )
            .is_err()
        );
        let project = ProductSourceRecord::project_with_graph_facts(
            "project",
            [2; 32],
            [3; 32],
            vec![[4; 32]],
            vec![[5; 32]],
        )
        .expect("project");
        let mut encoded = Vec::new();
        ProductSourceRelation::encode_value(&project, &mut encoded);
        let decoded = ProductSourceRelation::decode_value(&encoded).expect("decode project");
        let fields = decoded.project_fields().expect("project fields");
        assert_eq!(fields.source_version, [2; 32]);
        assert_eq!(fields.graph_version, [3; 32]);
        assert_eq!(fields.files, [[4; 32]]);
        assert_eq!(fields.facts, [[5; 32]]);
    }
}
