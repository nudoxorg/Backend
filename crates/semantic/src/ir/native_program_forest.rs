//! Durable ownership for a forest of independently admitted TypeScript programs.
//! Reference edges are configuration relationships, never source-membership edges.

use super::SemanticImageIdentity;
use super::native_program_sources::{
    MAX_NATIVE_PROGRAM_MANIFEST_BYTES, MAX_NATIVE_PROGRAM_SOURCES, NativeProgramSourceManifest,
    NativeProgramSourceManifestError, NativeProgramSourceRef,
};
use backend_version::{ContentId, Schema, SourceFactDomain, ToolchainDomain};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const MAGIC: &[u8; 8] = b"BKNPSF01";
const PROJECT_SET_MAGIC: &[u8; 8] = b"BKNPSU01";
/// Same bounded number of configuration nodes as project-host admission.
pub const MAX_NATIVE_PROGRAM_NODES: usize = 256;

/// Opaque forest/project-set CAS payload; magic distinguishes the two envelopes.
/// Legacy single-program objects retain their original schema.
pub struct NativeProgramForestSchema;
impl Schema for NativeProgramForestSchema {
    const DOMAIN: u8 = 0x7a;
    const TYPE: u16 = 0xc009;
    const VERSION: u8 = 2;
    type Value = [u8];
    fn encode(value: &Self::Value, out: &mut Vec<u8>) {
        out.extend_from_slice(value);
    }
}

/// An admitted config with its own effective options/SDK/source environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProgramNode {
    /// Canonical workspace-relative config path; nodes are ordered by this path.
    pub config_path: String,
    /// Exact raw config bytes, including any BOM.
    pub config: ContentId<SourceFactDomain>,
    /// Effective per-program options and complete compiler-read closure identity.
    pub environment: [u8; 32],
    /// Ordered actual config reference edges into the canonical node array.
    pub references: Vec<u32>,
    /// None only for a proved empty root list with actual references (a solution container).
    pub program: Option<NativeProgramSourceManifest>,
}

/// A member remains associated with its actual leaf, never a union program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeProgramOwnedSourceRef<'a> {
    /// Identity of the owning leaf program manifest, not the whole forest.
    pub program: [u8; 32],
    /// Identity of the owning config node in a forest; absent for a legacy single program.
    pub node: Option<[u8; 32]>,
    /// Exact source row borrowed from the owning leaf manifest.
    pub row: NativeProgramSourceRef<'a>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Programs {
    Single(NativeProgramSourceManifest),
    Forest {
        project_set: bool,
        root: u32,
        nodes: Vec<NativeProgramNode>,
        node_ids: Vec<[u8; 32]>,
        toolchain: ContentId<ToolchainDomain>,
        image_bindings: BTreeMap<String, ([u8; 32], SemanticImageIdentity)>,
    },
}

/// Versioned single/forest/project-set envelope with no synthetic `program()` API.
/// `membership_identity` authenticates a container; positive coordinates use leaf IDs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeProgramSources {
    programs: Programs,
    bytes: Arc<[u8]>,
    identity: [u8; 32],
}

impl<'a> std::ops::Deref for NativeProgramOwnedSourceRef<'a> {
    type Target = NativeProgramSourceRef<'a>;
    fn deref(&self) -> &Self::Target {
        &self.row
    }
}

impl From<NativeProgramSourceManifest> for NativeProgramSources {
    fn from(program: NativeProgramSourceManifest) -> Self {
        Self {
            bytes: Arc::from(program.as_bytes()),
            identity: program.program(),
            programs: Programs::Single(program),
        }
    }
}

impl NativeProgramSources {
    /// Builds a canonical, bounded config-reference graph for one toolchain.
    ///
    /// `root` indexes `nodes`; every node must be reachable from it, node paths
    /// must be canonical and sorted, and reference edges must be in range and
    /// acyclic. Node count, source count, and encoded bytes are bounded.
    ///
    /// # Errors
    ///
    /// Returns the manifest encoding or limit error when the graph violates
    /// those identity, topology, or size constraints.
    pub fn from_forest(
        toolchain: ContentId<ToolchainDomain>,
        root: u32,
        nodes: Vec<NativeProgramNode>,
    ) -> Result<Self, NativeProgramSourceManifestError> {
        Self::from_graph(toolchain, root, nodes, false)
    }

    /// Builds an explicit set of independently admitted projects for one toolchain.
    ///
    /// `primary` identifies the producer-selected config. Each node preserves its
    /// actual references, effective options, read closure, and source membership.
    /// Disconnected components are allowed, but all must be acyclic. A shared
    /// source may be a dependency in one project and uniquely owned by another
    /// only when its content identity agrees in every project.
    ///
    /// # Errors
    ///
    /// Returns an encoding or limit error for invalid topology, contradictory
    /// source identities, duplicate package owners, or exceeded size bounds.
    pub fn from_project_set(
        toolchain: ContentId<ToolchainDomain>,
        primary: u32,
        nodes: Vec<NativeProgramNode>,
    ) -> Result<Self, NativeProgramSourceManifestError> {
        Self::from_graph(toolchain, primary, nodes, true)
    }

    fn from_graph(
        toolchain: ContentId<ToolchainDomain>,
        root: u32,
        nodes: Vec<NativeProgramNode>,
        project_set: bool,
    ) -> Result<Self, NativeProgramSourceManifestError> {
        validate_graph(toolchain, root, &nodes, project_set)?;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(if project_set {
            PROJECT_SET_MAGIC
        } else {
            MAGIC
        });
        bytes.extend_from_slice(toolchain.as_ref());
        bytes.extend_from_slice(&root.to_le_bytes());
        bytes.extend_from_slice(&(nodes.len() as u32).to_le_bytes());
        let mut node_ids = Vec::with_capacity(nodes.len());
        for node in &nodes {
            put(&mut bytes, node.config_path.as_bytes())?;
            bytes.extend_from_slice(node.config.as_ref());
            bytes.extend_from_slice(&node.environment);
            bytes.extend_from_slice(&(node.references.len() as u32).to_le_bytes());
            for reference in &node.references {
                bytes.extend_from_slice(&reference.to_le_bytes());
            }
            let id = node_identity(toolchain, node);
            bytes.extend_from_slice(&id);
            node_ids.push(id);
            put(
                &mut bytes,
                node.program
                    .as_ref()
                    .map_or(&[][..], |program| program.as_bytes()),
            )?;
            if bytes.len() > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
                return Err(NativeProgramSourceManifestError::Limit);
            }
        }
        bytes.extend_from_slice(&0u32.to_le_bytes());
        if bytes.len() > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
            return Err(NativeProgramSourceManifestError::Limit);
        }
        let identity = *blake3::hash(&bytes).as_bytes();
        Ok(Self {
            programs: Programs::Forest {
                project_set,
                root,
                nodes,
                node_ids,
                toolchain,
                image_bindings: BTreeMap::new(),
            },
            bytes: bytes.into(),
            identity,
        })
    }

    /// Decodes a legacy manifest, strict forest, or independent project set.
    ///
    /// Forest decoding revalidates graph topology, member identities, image
    /// bindings, and canonical byte encoding before returning an owner.
    ///
    /// # Errors
    ///
    /// Returns an encoding, identity, or size error for malformed or
    /// noncanonical bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, NativeProgramSourceManifestError> {
        let project_set = bytes.starts_with(PROJECT_SET_MAGIC);
        if !project_set && !bytes.starts_with(MAGIC) {
            return NativeProgramSourceManifest::decode(bytes).map(Into::into);
        }
        if bytes.len() > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
            return Err(NativeProgramSourceManifestError::Limit);
        }
        let mut input = Input(&bytes[8..]);
        let toolchain = ContentId::try_from(input.take(32)?)
            .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
        let root = input.u32()?;
        let count = input.u32()? as usize;
        if count == 0 || count > MAX_NATIVE_PROGRAM_NODES {
            return Err(NativeProgramSourceManifestError::Limit);
        }
        let mut nodes = Vec::with_capacity(count);
        for _ in 0..count {
            let config_path = std::str::from_utf8(input.cell()?)
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?
                .to_owned();
            let config = ContentId::try_from(input.take(32)?)
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
            let environment = input
                .take(32)?
                .try_into()
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
            let reference_count = input.u32()? as usize;
            if reference_count > count {
                return Err(NativeProgramSourceManifestError::Limit);
            }
            let mut references = Vec::with_capacity(reference_count);
            for _ in 0..reference_count {
                references.push(input.u32()?);
            }
            let expected: [u8; 32] = input
                .take(32)?
                .try_into()
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
            let member = input.cell()?;
            let program = (!member.is_empty())
                .then(|| NativeProgramSourceManifest::decode(member))
                .transpose()?;
            let node = NativeProgramNode {
                config_path,
                config,
                environment,
                references,
                program,
            };
            if node_identity(toolchain, &node) != expected {
                return Err(NativeProgramSourceManifestError::ProgramIdentity);
            }
            nodes.push(node);
        }
        let image_count = input.u32()? as usize;
        if image_count > MAX_NATIVE_PROGRAM_SOURCES {
            return Err(NativeProgramSourceManifestError::Limit);
        }
        let mut images = Vec::with_capacity(image_count);
        for _ in 0..image_count {
            let path = std::str::from_utf8(input.cell()?)
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?
                .to_owned();
            let _recorded_node: [u8; 32] = input
                .take(32)?
                .try_into()
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
            let image: [u8; 32] = input
                .take(32)?
                .try_into()
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
            images.push((path, image));
        }
        if !input.0.is_empty() {
            return Err(NativeProgramSourceManifestError::Encoding);
        }
        let result =
            Self::from_graph(toolchain, root, nodes, project_set)?.with_image_bindings(images)?;
        if result.as_bytes() != bytes {
            return Err(NativeProgramSourceManifestError::Encoding);
        }
        Ok(result)
    }

    /// Seals the exact produced image bytes to the unique derived program node.
    /// An unbound source member may retain source provenance but cannot certify an image.
    pub fn with_image_bindings(
        self,
        images: impl IntoIterator<Item = (String, [u8; 32])>,
    ) -> Result<Self, NativeProgramSourceManifestError> {
        let owners = self
            .sources()
            .filter_map(|row| {
                row.package_path
                    .zip(row.node)
                    .map(|(path, node)| (path.to_owned(), node))
            })
            .collect::<BTreeMap<_, _>>();
        let Programs::Forest {
            project_set,
            root,
            nodes,
            toolchain,
            ..
        } = self.programs
        else {
            return Err(NativeProgramSourceManifestError::Encoding);
        };
        let mut result = Self::from_graph(toolchain, root, nodes, project_set)?;
        let mut bindings = BTreeMap::new();
        for (path, image) in images {
            let image = SemanticImageIdentity::try_from(image)
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?;
            let node = *owners
                .get(&path)
                .ok_or(NativeProgramSourceManifestError::Encoding)?;
            if bindings.len() >= MAX_NATIVE_PROGRAM_SOURCES {
                return Err(NativeProgramSourceManifestError::Limit);
            }
            if bindings.insert(path, (node, image)).is_some() {
                return Err(NativeProgramSourceManifestError::Encoding);
            }
        }
        let mut bytes = result.as_bytes().to_vec();
        bytes.truncate(bytes.len() - 4);
        bytes.extend_from_slice(&(bindings.len() as u32).to_le_bytes());
        for (path, (node, image)) in &bindings {
            put(&mut bytes, path.as_bytes())?;
            if bytes.len().saturating_add(64) > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
                return Err(NativeProgramSourceManifestError::Limit);
            }
            bytes.extend_from_slice(node);
            bytes.extend_from_slice(image.as_ref());
        }
        result.identity = *blake3::hash(&bytes).as_bytes();
        result.bytes = bytes.into();
        if let Programs::Forest { image_bindings, .. } = &mut result.programs {
            *image_bindings = bindings;
        }
        Ok(result)
    }

    /// Legacy single objects retain their source/toolchain admission contract.
    /// Forest images additionally require their exact producer-sealed digest.
    pub fn image_matches(&self, package_path: &str, image: &[u8; 32]) -> bool {
        match &self.programs {
            Programs::Single(_) => true,
            Programs::Forest { image_bindings, .. } => image_bindings
                .get(package_path)
                .is_some_and(|(_, expected)| expected.as_ref() == image),
        }
    }

    /// Returns whether this uses a config-node envelope (forest or project set).
    pub fn is_forest(&self) -> bool {
        matches!(self.programs, Programs::Forest { .. })
    }

    /// Returns the original manifest only for a legacy single-program owner.
    ///
    /// A forest has no synthetic union manifest, so this returns `None` there.
    pub fn single_program(&self) -> Option<&NativeProgramSourceManifest> {
        match &self.programs {
            Programs::Single(program) => Some(program),
            Programs::Forest { .. } => None,
        }
    }

    /// Returns the stable identity of this membership owner.
    ///
    /// Legacy owners retain their program identity; forests identify the full
    /// canonical envelope. Use each source row's `program` for leaf identity.
    pub fn membership_identity(&self) -> [u8; 32] {
        self.identity
    }

    /// Returns this owner's canonical encoded bytes, borrowed for its lifetime.
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Returns the toolchain content identity shared by this owner and its leaves.
    pub fn toolchain(&self) -> ContentId<ToolchainDomain> {
        match &self.programs {
            Programs::Single(program) => program.toolchain(),
            Programs::Forest { toolchain, .. } => *toolchain,
        }
    }

    /// Returns the root (or project-set primary) index and borrowed config nodes.
    /// Legacy owners have none; a primary does not imply reference reachability.
    pub fn nodes(&self) -> Option<(u32, &[NativeProgramNode])> {
        match &self.programs {
            Programs::Single(_) => None,
            Programs::Forest { root, nodes, .. } => Some((*root, nodes)),
        }
    }

    /// Iterates actual leaf source rows while preserving their owning identities.
    ///
    /// For forest rows, `node` identifies the owning config and `program`
    /// identifies that leaf's manifest. Legacy rows have no config-node ID.
    /// Each row borrows from this owner and cannot outlive it.
    pub fn sources(&self) -> impl Iterator<Item = NativeProgramOwnedSourceRef<'_>> {
        let (single, forest): (Option<&NativeProgramSourceManifest>, &[_]) = match &self.programs {
            Programs::Single(program) => (Some(program), &[]),
            Programs::Forest { nodes, .. } => (None, nodes),
        };
        single
            .into_iter()
            .flat_map(|program| {
                program.sources().map(|row| NativeProgramOwnedSourceRef {
                    program: program.program(),
                    node: None,
                    row,
                })
            })
            .chain(forest.iter().enumerate().flat_map(move |(index, node)| {
                let id = match &self.programs {
                    Programs::Forest { node_ids, .. } => node_ids[index],
                    _ => unreachable!(),
                };
                node.program.iter().flat_map(move |program| {
                    program
                        .sources()
                        .map(move |row| NativeProgramOwnedSourceRef {
                            program: program.program(),
                            node: Some(id),
                            row,
                        })
                })
            }))
    }
}

fn node_identity(toolchain: ContentId<ToolchainDomain>, node: &NativeProgramNode) -> [u8; 32] {
    let mut digest = blake3::Hasher::new();
    digest.update(b"typescript.program-node.v1\0");
    digest.update(toolchain.as_ref());
    digest.update(&(node.config_path.len() as u64).to_le_bytes());
    digest.update(node.config_path.as_bytes());
    digest.update(node.config.as_ref());
    digest.update(&node.environment);
    digest.update(&(node.references.len() as u64).to_le_bytes());
    for reference in &node.references {
        digest.update(&reference.to_le_bytes());
    }
    match &node.program {
        Some(program) => {
            digest.update(&[1]);
            digest.update(&program.program());
        }
        None => {
            digest.update(&[0]);
        }
    }
    *digest.finalize().as_bytes()
}

fn validate_graph(
    toolchain: ContentId<ToolchainDomain>,
    root: u32,
    nodes: &[NativeProgramNode],
    project_set: bool,
) -> Result<(), NativeProgramSourceManifestError> {
    use NativeProgramSourceManifestError::{Encoding, Limit};
    if nodes.is_empty() || nodes.len() > MAX_NATIVE_PROGRAM_NODES || root as usize >= nodes.len() {
        return Err(Limit);
    }
    let mut package_owners = BTreeSet::new();
    let mut members: BTreeMap<&str, (super::SourceIdentity, Option<&str>)> = BTreeMap::new();
    let mut sources = 0usize;
    let mut encoded = 52usize;
    for (index, node) in nodes.iter().enumerate() {
        if !valid_path(&node.config_path)
            || (index > 0 && nodes[index - 1].config_path >= node.config_path)
        {
            return Err(Encoding);
        }
        if node.program.is_none() && node.references.is_empty() {
            return Err(Encoding);
        }
        let mut references = BTreeSet::new();
        for reference in &node.references {
            if *reference as usize >= nodes.len() || !references.insert(*reference) {
                return Err(Encoding);
            }
        }
        encoded = encoded
            .saturating_add(108)
            .saturating_add(node.config_path.len())
            .saturating_add(node.references.len().saturating_mul(4));
        if let Some(program) = &node.program {
            if program.toolchain() != toolchain {
                return Err(Encoding);
            }
            encoded = encoded.saturating_add(program.as_bytes().len());
            for row in program.sources() {
                sources = sources.saturating_add(1);
                if let Some((source, package)) = members.get_mut(row.program_path) {
                    if *source != row.source
                        || (!project_set && *package != row.package_path)
                        || (package.is_some()
                            && row.package_path.is_some()
                            && *package != row.package_path)
                    {
                        return Err(Encoding);
                    }
                    // Retain the actual owner across intervening dependency rows.
                    if package.is_none() {
                        *package = row.package_path;
                    }
                } else {
                    members.insert(row.program_path, (row.source, row.package_path));
                }
                if row
                    .package_path
                    .is_some_and(|path| !package_owners.insert(path))
                {
                    return Err(Encoding);
                }
            }
        }
        if sources > MAX_NATIVE_PROGRAM_SOURCES || encoded > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
            return Err(Limit);
        }
    }
    fn visit(
        index: usize,
        nodes: &[NativeProgramNode],
        state: &mut [u8],
    ) -> Result<(), NativeProgramSourceManifestError> {
        match state[index] {
            1 => return Err(Encoding),
            2 => return Ok(()),
            _ => {}
        }
        state[index] = 1;
        for reference in &nodes[index].references {
            visit(*reference as usize, nodes, state)?;
        }
        state[index] = 2;
        Ok(())
    }
    let mut state = vec![0; nodes.len()];
    visit(root as usize, nodes, &mut state)?;
    if project_set {
        for index in 0..nodes.len() {
            visit(index, nodes, &mut state)?;
        }
    }
    if state.iter().any(|state| *state != 2) {
        return Err(Encoding);
    }
    Ok(())
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains(['\\', '\0'])
        && !path.split('/').any(|part| matches!(part, "" | "." | ".."))
}
fn put(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), NativeProgramSourceManifestError> {
    if out.len().saturating_add(4).saturating_add(bytes.len()) > MAX_NATIVE_PROGRAM_MANIFEST_BYTES {
        return Err(NativeProgramSourceManifestError::Limit);
    }
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
    Ok(())
}
struct Input<'a>(&'a [u8]);
impl<'a> Input<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], NativeProgramSourceManifestError> {
        let (head, tail) = self
            .0
            .split_at_checked(count)
            .ok_or(NativeProgramSourceManifestError::Encoding)?;
        self.0 = tail;
        Ok(head)
    }
    fn u32(&mut self) -> Result<u32, NativeProgramSourceManifestError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| NativeProgramSourceManifestError::Encoding)?,
        ))
    }
    fn cell(&mut self) -> Result<&'a [u8], NativeProgramSourceManifestError> {
        let count = self.u32()? as usize;
        self.take(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{NativeProgramSource, SourceIdentity};
    fn leaf(path: &str) -> NativeProgramSourceManifest {
        NativeProgramSourceManifest::from_sources(
            ContentId::from_canonical_bytes(b"compiler"),
            vec![NativeProgramSource {
                program_path: path.into(),
                package_path: Some(path.into()),
                source: SourceIdentity::from_bytes(b"export {};")
                    .expect("fixture source bytes are valid"),
            }],
        )
        .expect("fixture source manifest is valid")
    }
    fn nodes() -> Vec<NativeProgramNode> {
        vec![
            NativeProgramNode {
                config_path: "tsconfig.app.json".into(),
                config: ContentId::from_canonical_bytes(b"config1"),
                environment: [2; 32],
                references: vec![],
                program: Some(leaf("src/app.ts")),
            },
            NativeProgramNode {
                config_path: "tsconfig.json".into(),
                config: ContentId::from_canonical_bytes(b"config3"),
                environment: [4; 32],
                references: vec![0, 2],
                program: None,
            },
            NativeProgramNode {
                config_path: "tsconfig.node.json".into(),
                config: ContentId::from_canonical_bytes(b"config5"),
                environment: [6; 32],
                references: vec![],
                program: Some(leaf("vite.config.ts")),
            },
        ]
    }

    fn independent_nodes() -> Vec<NativeProgramNode> {
        let mut projects = nodes();
        projects.remove(1);
        projects
    }

    fn shared_leaf(package_path: Option<&str>, bytes: &[u8]) -> NativeProgramSourceManifest {
        NativeProgramSourceManifest::from_sources(
            ContentId::from_canonical_bytes(b"compiler"),
            vec![NativeProgramSource {
                program_path: "src/app.ts".into(),
                package_path: package_path.map(str::to_owned),
                source: SourceIdentity::from_bytes(bytes).expect("valid fixture source"),
            }],
        )
        .expect("valid shared fixture manifest")
    }

    #[test]
    fn project_set_retains_independent_primary_leaf_owners_and_image_bindings() {
        let toolchain = ContentId::from_canonical_bytes(b"compiler");
        let projects = independent_nodes();
        assert!(NativeProgramSources::from_forest(toolchain, 0, projects.clone()).is_err());
        let set = NativeProgramSources::from_project_set(toolchain, 0, projects.clone())
            .expect("independent projects are explicitly admitted");
        assert!(set.as_bytes().starts_with(PROJECT_SET_MAGIC));
        let (primary, actual) = set.nodes().expect("project set config nodes");
        assert_eq!(primary, 0);
        assert_eq!(actual, projects);
        assert!(actual.iter().all(|node| node.references.is_empty()));
        let rows = set.sources().collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[0].program,
            projects[0].program.as_ref().unwrap().program()
        );
        assert_eq!(
            rows[1].program,
            projects[1].program.as_ref().unwrap().program()
        );
        assert_ne!(rows[0].node, rows[1].node);
        assert_ne!(rows[0].program, set.membership_identity());
        let primary_identity = set.membership_identity();
        let image = *SemanticImageIdentity::from_encoded_bytes(b"independent image").as_ref();
        let bound = set
            .with_image_bindings([("vite.config.ts".into(), image)])
            .expect("independent owner can seal its image");
        assert!(bound.as_bytes().starts_with(PROJECT_SET_MAGIC));
        let cold = NativeProgramSources::decode(bound.as_bytes()).expect("project set cold decode");
        assert_eq!(cold, bound);
        assert!(cold.image_matches("vite.config.ts", &image));
        assert!(!cold.image_matches("src/app.ts", &image));
        let mut disguised = bound.as_bytes().to_vec();
        disguised[..8].copy_from_slice(MAGIC);
        assert!(NativeProgramSources::decode(&disguised).is_err());

        let forest = NativeProgramSources::from_forest(toolchain, 1, nodes()).unwrap();
        let connected_set = NativeProgramSources::from_project_set(toolchain, 1, nodes()).unwrap();
        assert_ne!(
            forest.membership_identity(),
            connected_set.membership_identity()
        );
        assert_eq!(&forest.as_bytes()[8..], &connected_set.as_bytes()[8..]);
        let changed_primary =
            NativeProgramSources::from_project_set(toolchain, 1, projects).unwrap();
        assert_ne!(changed_primary.membership_identity(), primary_identity);
    }

    #[test]
    fn project_set_only_relaxes_equal_content_dependency_representation() {
        let toolchain = ContentId::from_canonical_bytes(b"compiler");
        let mut projects = independent_nodes();
        projects[1].program = Some(shared_leaf(None, b"export {};"));
        let set = NativeProgramSources::from_project_set(toolchain, 0, projects.clone())
            .expect("same bytes can be owned in one project and read in another");
        let rows = set.sources().collect::<Vec<_>>();
        assert_eq!(rows[0].package_path, Some("src/app.ts"));
        assert_eq!(rows[1].package_path, None);
        assert_eq!(rows[0].source, rows[1].source);
        assert_eq!(NativeProgramSources::decode(set.as_bytes()).unwrap(), set);
        let mut dependency_first = projects.clone();
        dependency_first[0].program = Some(shared_leaf(None, b"export {};"));
        dependency_first[1].program = Some(shared_leaf(Some("src/app.ts"), b"export {};"));
        assert!(NativeProgramSources::from_project_set(toolchain, 0, dependency_first).is_ok());
        projects[0].references.push(1);
        assert!(NativeProgramSources::from_forest(toolchain, 0, projects.clone()).is_err());
        projects[1].program = Some(shared_leaf(None, b"export const changed = true;"));
        assert!(NativeProgramSources::from_project_set(toolchain, 0, projects.clone()).is_err());
        projects[1].program = Some(shared_leaf(Some("src/app.ts"), b"export {};"));
        assert!(NativeProgramSources::from_project_set(toolchain, 0, projects.clone()).is_err());
        projects[1].program = Some(shared_leaf(Some("different.ts"), b"export {};"));
        assert!(NativeProgramSources::from_project_set(toolchain, 0, projects).is_err());
    }

    #[test]
    fn project_set_validates_every_component_and_preserves_bounds() {
        let toolchain = ContentId::from_canonical_bytes(b"compiler");
        let mut projects = independent_nodes();
        projects[1].references.push(1);
        assert!(NativeProgramSources::from_project_set(toolchain, 0, projects.clone()).is_err());
        projects[1].references = vec![2];
        assert!(NativeProgramSources::from_project_set(toolchain, 0, projects.clone()).is_err());
        projects[1].references = vec![0, 0];
        assert!(NativeProgramSources::from_project_set(toolchain, 0, projects.clone()).is_err());
        projects[1].references.clear();
        projects[1].program = None;
        assert!(NativeProgramSources::from_project_set(toolchain, 0, projects).is_err());
        assert!(NativeProgramSources::from_project_set(toolchain, 2, independent_nodes()).is_err());
        assert!(NativeProgramSources::from_project_set(toolchain, 0, vec![]).is_err());
        let mut projects = independent_nodes();
        projects.reverse();
        assert!(NativeProgramSources::from_project_set(toolchain, 0, projects).is_err());
        assert!(
            NativeProgramSources::from_project_set(
                ContentId::from_canonical_bytes(b"other compiler"),
                0,
                independent_nodes()
            )
            .is_err()
        );
    }
    #[test]
    fn forest_round_trip_retains_leaf_ownership_and_legacy_bytes() {
        let legacy = leaf("src/app.ts");
        let single = NativeProgramSources::from(legacy.clone());
        assert_eq!(single.as_bytes(), legacy.as_bytes());
        assert_eq!(single.membership_identity(), legacy.program());
        let forest = NativeProgramSources::from_forest(
            ContentId::from_canonical_bytes(b"compiler"),
            1,
            nodes(),
        )
        .expect("forest fixture is structurally valid");
        assert_eq!(
            NativeProgramSources::decode(forest.as_bytes())
                .expect("encoded forest fixture decodes"),
            forest
        );
        let rows = forest.sources().collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].program, legacy.program());
        assert_ne!(rows[0].node, rows[1].node);
        let mut changed = nodes();
        changed[0].environment[0] ^= 1;
        assert_ne!(
            NativeProgramSources::from_forest(
                ContentId::from_canonical_bytes(b"compiler"),
                1,
                changed
            )
            .expect("changed forest fixture remains valid")
            .membership_identity(),
            forest.membership_identity()
        );
    }
    #[test]
    fn forest_refuses_cycles_dangling_edges_and_ambiguous_sources() {
        let toolchain = ContentId::from_canonical_bytes(b"compiler");
        let mut cycle = nodes();
        cycle[0].references.push(1);
        assert!(NativeProgramSources::from_forest(toolchain, 1, cycle).is_err());
        let mut dangling = nodes();
        dangling[1].references.push(3);
        assert!(NativeProgramSources::from_forest(toolchain, 1, dangling).is_err());
        let mut duplicate = nodes();
        duplicate[2].program = Some(leaf("src/app.ts"));
        assert!(NativeProgramSources::from_forest(toolchain, 1, duplicate).is_err());
    }

    #[test]
    fn forest_image_bindings_require_unique_actual_owners_and_exact_encoded_identity() {
        let forest = NativeProgramSources::from_forest(
            ContentId::from_canonical_bytes(b"compiler"),
            1,
            nodes(),
        )
        .expect("forest fixture is structurally valid");
        let image = *SemanticImageIdentity::from_encoded_bytes(b"actual encoded fixture").as_ref();
        let wrong =
            *SemanticImageIdentity::from_encoded_bytes(b"different encoded fixture").as_ref();
        assert!(!forest.image_matches("src/app.ts", &image));
        let bound = forest
            .clone()
            .with_image_bindings([("src/app.ts".into(), image)])
            .expect("image binding fixture is valid");
        let cold = NativeProgramSources::decode(bound.as_bytes())
            .expect("encoded image binding fixture decodes");
        assert_eq!(cold, bound);
        assert!(cold.image_matches("src/app.ts", &image));
        assert!(!cold.image_matches("src/app.ts", &wrong));
        assert!(
            !cold.image_matches("vite.config.ts", &image),
            "partial outputs never invent another leaf image"
        );
        assert!(
            forest
                .clone()
                .with_image_bindings([("foreign.ts".into(), image)])
                .is_err()
        );
        assert!(
            forest
                .clone()
                .with_image_bindings([("src/app.ts".into(), image), ("src/app.ts".into(), image)])
                .is_err()
        );
        assert!(
            forest
                .clone()
                .with_image_bindings([("src/app.ts".into(), [0; 32])])
                .is_err(),
            "wrong identity authority bytes refuse"
        );
        let mut corrupt = bound.as_bytes().to_vec();
        let node_position = corrupt.len() - 64;
        corrupt[node_position] ^= 1;
        assert!(
            NativeProgramSources::decode(&corrupt).is_err(),
            "image owner node cannot be rebound in durable bytes"
        );
    }

    #[test]
    fn forest_rejects_contradictory_mapping_and_authenticates_options_and_edges() {
        let toolchain = ContentId::from_canonical_bytes(b"compiler");
        let base = nodes();
        let mut contradictory = base.clone();
        contradictory[2].program = Some(
            NativeProgramSourceManifest::from_sources(
                toolchain,
                vec![NativeProgramSource {
                    program_path: "src/app.ts".into(),
                    package_path: None,
                    source: SourceIdentity::from_bytes(b"export {};")
                        .expect("fixture source bytes are valid"),
                }],
            )
            .expect("contradictory fixture manifest is valid"),
        );
        assert!(NativeProgramSources::from_forest(toolchain, 1, contradictory).is_err());
        let first = NativeProgramSources::from_forest(toolchain, 1, base.clone())
            .expect("base forest fixture is valid");
        let mut options = base.clone();
        options[0].environment[0] ^= 1;
        assert_eq!(
            options[0].program, base[0].program,
            "physical leaf membership remains exact"
        );
        assert_ne!(
            node_identity(toolchain, &options[0]),
            node_identity(toolchain, &base[0])
        );
        let mut edges = base;
        edges[1].references.reverse();
        assert_ne!(
            NativeProgramSources::from_forest(toolchain, 1, edges)
                .expect("changed edge fixture remains a valid forest")
                .membership_identity(),
            first.membership_identity()
        );
        let mut corrupt = first.as_bytes().to_vec();
        // Tamper the config identity without changing the independently sealed node ID.
        corrupt[48 + 4 + "tsconfig.app.json".len()] ^= 1;
        assert!(NativeProgramSources::decode(&corrupt).is_err());
        assert!(first.single_program().is_none());
    }
}
