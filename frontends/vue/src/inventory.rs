//! Untrusted, partial Vue host inventory rows for the reserved auxiliary relation.
//!
//! A decoded row is a claim. The owner must authenticate its selected project,
//! relative path and analysis basis, then verify the exact host bytes before use.
use crate::{Block, Container, DeclarationKind, parse};
use backend_compile::VerifiedSourceExtent;
use backend_version::{CanonicalRelation, Relation, RelationDecodeError, Schema};
use serde::{Deserialize, Serialize};
use std::ops::Range;

// Keep rows below the current canonical workspace node capacity; oversize
// inventories are refused and must remain explicitly unavailable.
const MAX_VALUE: usize = 30 * 1024;
const MAX_PATH: usize = 4096;
/// Reserved Vue inventory relation, not a compiler language profile.
#[derive(Debug)]
pub struct VueInventoryRelation;
/// Reserved selected-root pointer schema; registration and atomic selection
/// must be supplied by the workspace owner.
pub struct VueInventoryRootSchema;
impl Schema for VueInventoryRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 19;
    const VERSION: u8 = 1;
    type Value = [u8; 32];
    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Declaration {
    block: usize,
    kind: DeclarationKind,
    owner: Range<usize>,
    name: Range<usize>,
}
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CallObservation {
    block: usize,
    site: Range<usize>,
    target: Range<usize>,
}
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScriptObservation {
    block: usize,
    status: String,
    diagnostics: Vec<String>,
    unmapped_names: usize,
    unresolved_calls: usize,
}
/// Coverage of one Vue source capability, independent of page completion.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum VueCapabilityCoverage {
    /// Some native syntax facts are available; the requested capability is incomplete.
    Partial,
    /// This producer did not analyze the capability.
    Unavailable,
}
/// Separate script, template and style coverage for one host.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VueCoverage {
    /// Script syntax observation, not binding or module resolution.
    pub script: VueCapabilityCoverage,
    /// Vue template semantics.
    pub template: VueCapabilityCoverage,
    /// CSS or preprocessor semantics.
    pub style: VueCapabilityCoverage,
}
/// Persisted parser observations. These never represent a complete Vue graph.
/// Native diagnostics are preserved as opaque debug records; callers must
/// reparse the verified host for structured diagnostics.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VueHostInventory {
    project: [u8; 32],
    path: String,
    analysis_basis: [u8; 32],
    producer: [u8; 32],
    input_digest: [u8; 32],
    source_digest: [u8; 32],
    host_length: usize,
    coverage: VueCoverage,
    blocks: Vec<Block>,
    declarations: Vec<Declaration>,
    calls: Vec<CallObservation>,
    container_diagnostics: Vec<String>,
    scripts: Vec<ScriptObservation>,
}
/// A replayed inventory bound to actual verified host bytes. Workspace owner,
/// path and analysis-basis selection are still authenticated by the caller.
pub struct VerifiedVueInventory<'row, 'host> {
    row: &'row VueHostInventory,
    container: Container<'host>,
}
/// One verified syntactic declaration occurrence, not a resolved symbol.
pub struct VerifiedVueDeclaration<'a> {
    /// Block index in the original host inventory.
    pub block: usize,
    /// Native parser-observed declaration kind.
    pub kind: DeclarationKind,
    /// Verified declaration owner extent.
    pub owner: VerifiedSourceExtent<'a>,
    /// Verified authored name extent.
    pub name: VerifiedSourceExtent<'a>,
}
/// A direct call and local declaration proven by the TSZ binder.
pub struct VerifiedVueCall<'a> {
    /// Source script block index.
    pub block: usize,
    /// Exact authored call identifier.
    pub site: VerifiedSourceExtent<'a>,
    /// Exact local declaration name; no external target is implied.
    pub target: VerifiedSourceExtent<'a>,
}
/// A bounded host-local search page; page completion is not semantic coverage.
pub struct VueDeclarationPage<'a> {
    /// Matching authored declaration occurrences.
    pub declarations: Vec<VerifiedVueDeclaration<'a>>,
    /// More matching occurrences exist in this host.
    pub has_more: bool,
    /// Independent Vue capability outcomes.
    pub coverage: VueCoverage,
}
impl<'row, 'host> VerifiedVueInventory<'row, 'host> {
    /// Separate verified capability outcomes; no page completion upgrades them.
    pub fn coverage(&self) -> &VueCoverage {
        &self.row.coverage
    }
    /// Number of syntax-only declaration observations.
    pub fn declaration_count(&self) -> usize {
        self.row.declarations.len()
    }
    /// Reconstruct a declaration from authenticated host bytes, never stored
    /// name text. This does not establish name binding or a reference edge.
    pub fn declaration(&self, index: usize) -> Option<VerifiedVueDeclaration<'host>> {
        let record = self.row.declarations.get(index)?;
        let owner = self.container.host().subregion(record.owner.clone()).ok()?;
        let name = self.container.host().subregion(record.name.clone()).ok()?;
        Some(VerifiedVueDeclaration {
            block: record.block,
            kind: record.kind,
            owner,
            name,
        })
    }
    /// Search exact authored declaration spelling inside this host. This
    /// bounded local result is always syntax-only and does not establish
    /// reference completeness or workspace-wide negative evidence.
    pub fn search_named(
        &self,
        name: &str,
        limit: usize,
    ) -> Result<VueDeclarationPage<'host>, &'static str> {
        if name.len() > 4096 || limit == 0 || limit > 4096 {
            return Err("invalid Vue declaration page limit");
        }
        let mut declarations = Vec::new();
        let mut has_more = false;
        for index in 0..self.declaration_count() {
            let declaration = self
                .declaration(index)
                .ok_or("verified Vue declaration range became invalid")?;
            if declaration.name.text() == name {
                if declarations.len() == limit {
                    has_more = true;
                    break;
                }
                declarations.push(declaration);
            }
        }
        Ok(VueDeclarationPage {
            declarations,
            has_more,
            coverage: self.row.coverage.clone(),
        })
    }
    /// Number of locally bound direct calls captured in this host.
    pub fn call_count(&self) -> usize {
        self.row.calls.len()
    }
    /// Read one verified local direct-call relation.
    pub fn call(&self, index: usize) -> Option<VerifiedVueCall<'host>> {
        let record = self.row.calls.get(index)?;
        let site = self.container.host().subregion(record.site.clone()).ok()?;
        let target = self
            .container
            .host()
            .subregion(record.target.clone())
            .ok()?;
        Some(VerifiedVueCall {
            block: record.block,
            site,
            target,
        })
    }
    /// Parsed host, including original structured native diagnostics and gaps.
    pub fn container(&self) -> &Container<'host> {
        &self.container
    }
}
impl VueHostInventory {
    /// Produces a bounded row from one actual verified host and an externally
    /// authenticated project/path and native tool/configuration basis.
    /// The resulting inventory is always partial.
    pub fn capture(
        project: [u8; 32],
        path: &str,
        analysis_basis: [u8; 32],
        container: &Container<'_>,
    ) -> Result<Self, &'static str> {
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let budget = tsz_common::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(10),
            &cancelled,
            5_000_000,
        );
        Self::capture_with_checkpoint(project, path, analysis_basis, container, &budget)
    }
    /// Capture one host with cooperative cancellation and a finite native
    /// script work/deadline budget. A stopped parse aborts this host capture.
    pub fn capture_cancellable(
        project: [u8; 32],
        path: &str,
        analysis_basis: [u8; 32],
        container: &Container<'_>,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<Self, &'static str> {
        let budget = tsz_common::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(10),
            cancelled,
            5_000_000,
        );
        Self::capture_with_checkpoint(project, path, analysis_basis, container, &budget)
    }
    /// Capture with one caller-supplied cooperative native script work budget.
    pub fn capture_with_checkpoint(
        project: [u8; 32],
        path: &str,
        analysis_basis: [u8; 32],
        container: &Container<'_>,
        checkpoint: &dyn tsz_common::ExecutionCheckpoint,
    ) -> Result<Self, &'static str> {
        if project == [0; 32] || analysis_basis == [0; 32] || !valid_path(path) {
            return Err("invalid Vue inventory authority");
        }
        let mut scripts = Vec::new();
        let mut declarations = Vec::new();
        let mut calls = Vec::new();
        for (index, _) in container.blocks().iter().enumerate() {
            if !matches!(container.kind(index), Some(crate::BlockKind::Script { .. })) {
                continue;
            }
            match container.parse_script_with_checkpoint(index, checkpoint) {
                Ok(parsed) => {
                    for declaration in parsed.declarations {
                        declarations.push(Declaration {
                            block: index,
                            kind: declaration.kind,
                            owner: declaration.owner.byte_range(),
                            name: declaration.name.byte_range(),
                        });
                    }
                    for call in parsed.calls {
                        calls.push(CallObservation {
                            block: index,
                            site: call.site.byte_range(),
                            target: call.target.byte_range(),
                        });
                    }
                    scripts.push(ScriptObservation {
                        block: index,
                        status: "partial".into(),
                        diagnostics: parsed
                            .diagnostics
                            .iter()
                            .map(|d| format!("{:?}", d.diagnostic))
                            .collect(),
                        unmapped_names: parsed.unmapped_names,
                        unresolved_calls: parsed.unresolved_calls,
                    });
                }
                Err(crate::ScriptParseError::Stopped) => return Err("Vue native script stopped"),
                Err(error) => scripts.push(ScriptObservation {
                    block: index,
                    status: format!("{error:?}"),
                    diagnostics: Vec::new(),
                    unmapped_names: 0,
                    unresolved_calls: 0,
                }),
            }
        }
        let host = container.host();
        let row = Self {
            project,
            path: path.into(),
            analysis_basis,
            producer: crate::support_fingerprint(),
            input_digest: host.content().to_bytes(),
            source_digest: *host.source().as_ref(),
            host_length: host.text().len(),
            coverage: VueCoverage {
                script: if scripts.iter().any(|s| s.status == "partial") {
                    VueCapabilityCoverage::Partial
                } else {
                    VueCapabilityCoverage::Unavailable
                },
                template: VueCapabilityCoverage::Unavailable,
                style: VueCapabilityCoverage::Unavailable,
            },
            blocks: container.blocks().to_vec(),
            declarations,
            calls,
            container_diagnostics: container
                .diagnostics()
                .iter()
                .map(|d| format!("{d:?}"))
                .collect(),
            scripts,
        };
        row.validate()?;
        Ok(row)
    }
    /// Reparse exact bytes and compare every observation. This establishes byte
    /// correspondence and current producer identity, not workspace selection.
    pub fn verify_host<'row, 'host>(
        &'row self,
        bytes: &'host [u8],
        selected_key: [u8; 32],
        selected_project: [u8; 32],
        selected_path: &str,
        expected_basis: [u8; 32],
    ) -> Result<VerifiedVueInventory<'row, 'host>, &'static str> {
        let cancelled = std::sync::atomic::AtomicBool::new(false);
        let budget = tsz_common::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(10),
            &cancelled,
            5_000_000,
        );
        self.verify_host_with_checkpoint(
            bytes,
            selected_key,
            selected_project,
            selected_path,
            expected_basis,
            &budget,
        )
    }
    /// Verify against one caller-owned native replay budget, shared across hosts.
    pub fn verify_host_with_checkpoint<'row, 'host>(
        &'row self,
        bytes: &'host [u8],
        selected_key: [u8; 32],
        selected_project: [u8; 32],
        selected_path: &str,
        expected_basis: [u8; 32],
        checkpoint: &dyn tsz_common::ExecutionCheckpoint,
    ) -> Result<VerifiedVueInventory<'row, 'host>, &'static str> {
        checkpoint.checkpoint(1).map_err(|_| "Vue replay stopped")?;
        self.validate()?;
        if self.key() != selected_key
            || self.project != selected_project
            || self.path != selected_path
        {
            return Err("Vue selected owner or key differs");
        }
        if self.producer != crate::support_fingerprint() {
            return Err("Vue producer differs");
        }
        if self.analysis_basis != expected_basis {
            return Err("Vue analysis basis differs");
        }
        let container = parse(bytes).map_err(|_| "Vue host could not be parsed")?;
        checkpoint.checkpoint(1).map_err(|_| "Vue replay stopped")?;
        let actual = Self::capture_with_checkpoint(
            self.project,
            &self.path,
            expected_basis,
            &container,
            checkpoint,
        )?;
        checkpoint.checkpoint(1).map_err(|_| "Vue replay stopped")?;
        if *self != actual {
            return Err("Vue host or inventory differs");
        }
        Ok(VerifiedVueInventory {
            row: self,
            container,
        })
    }
    /// Compare actual host bytes to both stored identities without granting source authority.
    pub fn matches_host_bytes(&self, bytes: &[u8]) -> bool {
        if bytes.len() != self.host_length {
            return false;
        }
        let Ok(source) = backend_compile::VerifiedSourceText::capture(bytes) else {
            return false;
        };
        let Ok(extent) = source.extent(0..bytes.len()) else {
            return false;
        };
        extent.content().to_bytes() == self.input_digest
            && *extent.source().as_ref() == self.source_digest
    }
    /// Captured host byte length, still an untrusted row claim before replay.
    pub const fn byte_length(&self) -> usize {
        self.host_length
    }
    /// Claimed native tool and source-policy basis; authenticate against selection.
    pub const fn analysis_basis(&self) -> [u8; 32] {
        self.analysis_basis
    }
    /// Native producer identity captured with this host.
    pub const fn producer(&self) -> [u8; 32] {
        self.producer
    }
    /// Path and owner must still be joined to the selected workspace.
    pub fn owner(&self) -> ([u8; 32], &str) {
        (self.project, &self.path)
    }
    /// This relation never claims complete declarations or resolved references.
    pub const fn is_partial(&self) -> bool {
        true
    }
    /// Stable key derived from owner and path, not a content identity.
    pub fn key(&self) -> [u8; 32] {
        key(self.project, &self.path)
    }
    fn validate(&self) -> Result<(), &'static str> {
        if self.project == [0; 32]
            || self.analysis_basis == [0; 32]
            || self.producer == [0; 32]
            || !valid_path(&self.path)
            || self.host_length > 1024 * 1024
            || self.blocks.len() > 4096
        {
            return Err("invalid Vue inventory row");
        }
        let mut bounded = BoundedWriter { count: 0 };
        serde_json::to_writer(&mut bounded, self)
            .map_err(|_| "Vue inventory row exceeds budget")?;
        Ok(())
    }
}
struct BoundedWriter {
    count: usize,
}
impl std::io::Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.count = self
            .count
            .checked_add(bytes.len())
            .ok_or_else(|| std::io::Error::other("Vue row size overflow"))?;
        if self.count > MAX_VALUE {
            return Err(std::io::Error::other("Vue row budget"));
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn valid_path(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.bytes().any(|b| b.is_ascii_control())
        && !(path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic())
        && path
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != "..")
        && path
            .rsplit('.')
            .next()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("vue"))
}
fn key(project: [u8; 32], path: &str) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.vue-host-inventory.v1\0");
    hash.update(&project);
    hash.update(&(path.len() as u32).to_be_bytes());
    hash.update(path.as_bytes());
    *hash.finalize().as_bytes()
}
/// Digest a sorted, duplicate-free bounded set of project-relative Vue paths.
/// This witnesses observed paths only; it does not prove discovery completeness.
pub fn vue_path_inventory_digest(paths: &[String]) -> Result<[u8; 32], &'static str> {
    if paths.len() > 100_000 {
        return Err("Vue path count limit");
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.vue-path-inventory.v1\0");
    let mut bytes = 0usize;
    let mut previous: Option<&str> = None;
    for path in paths {
        if !valid_path(path) || previous.is_some_and(|prior| prior >= path.as_str()) {
            return Err("invalid Vue inventory path ordering");
        }
        previous = Some(path);
        bytes = bytes
            .checked_add(path.len())
            .ok_or("Vue path byte overflow")?;
        if bytes > 16 * 1024 * 1024 {
            return Err("Vue path byte limit");
        }
        hash.update(&(path.len() as u64).to_be_bytes());
        hash.update(path.as_bytes());
    }
    hash.update(&(paths.len() as u64).to_be_bytes());
    Ok(*hash.finalize().as_bytes())
}
/// A source-walk selection or per-host outcome. Every form is partial;
/// absence of a selection is unknown, not an empty inventory.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "record", deny_unknown_fields)]
pub enum VueInventoryRecord {
    /// Selected project walk; `discovered` is observed, not a complete negative.
    Selection {
        /// Selected workspace project.
        project: [u8; 32],
        /// Canonical admitted filesystem root; usable only after workspace selection.
        source_root: String,
        /// Authenticated native analysis and source policy basis.
        basis: [u8; 32],
        /// Native frontend producer identity.
        producer: [u8; 32],
        /// Vue candidates witnessed by this bounded walk.
        discovered: u32,
        /// Digest of sorted exact project-relative Vue host paths.
        inventory_digest: [u8; 32],
        /// Candidates whose source or parser result was unavailable.
        unavailable: u32,
        /// The same-walk candidate list was truncated or could not represent a path.
        walk_truncated: bool,
    },
    /// Captured native source observations, requiring host replay.
    Host(VueHostInventory),
    /// A selected host whose bytes or analysis were unavailable.
    Unavailable {
        /// Selected workspace project.
        project: [u8; 32],
        /// Project-relative Vue source path.
        path: String,
        /// The captured unavailable reason.
        reason: VueUnavailable,
    },
}
/// Bounded failure classes; an unavailable host never represents empty facts.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub enum VueUnavailable {
    /// Source could not be read or was deleted during capture.
    Read,
    /// Source or analysis exceeded a finite budget.
    Limit,
    /// Host source was not UTF-8.
    InvalidUtf8,
    /// Native parser could not return an admissible inventory.
    Parser,
}
/// Stable selection row key for one project.
pub fn vue_selection_key(project: [u8; 32]) -> [u8; 32] {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.vue-selection.v1\0");
    hash.update(&project);
    *hash.finalize().as_bytes()
}
impl VueInventoryRecord {
    /// Claimed project owner, to be joined to selected workspace authority.
    pub fn project(&self) -> [u8; 32] {
        match self {
            Self::Selection { project, .. } | Self::Unavailable { project, .. } => *project,
            Self::Host(host) => host.owner().0,
        }
    }
    /// Canonical key, checked again by relation admission and host replay.
    pub fn key(&self) -> [u8; 32] {
        match self {
            Self::Selection { project, .. } => vue_selection_key(*project),
            Self::Host(row) => row.key(),
            Self::Unavailable { project, path, .. } => key(*project, path),
        }
    }
    /// Check scalar and encoded row limits; this does not prove selection.
    pub fn validate(&self) -> Result<(), &'static str> {
        match self {
            Self::Selection {
                project,
                basis,
                producer,
                source_root,
                discovered,
                inventory_digest,
                unavailable,
                walk_truncated: _,
            } => {
                if *project == [0; 32]
                    || source_root.is_empty()
                    || source_root.len() > MAX_PATH
                    || source_root.bytes().any(|b| b.is_ascii_control())
                    || !std::path::Path::new(source_root).is_absolute()
                    || *basis == [0; 32]
                    || *producer == [0; 32]
                    || *inventory_digest == [0; 32]
                    || *unavailable > *discovered
                    || *discovered > 100_000
                {
                    return Err("invalid Vue selection");
                }
            }
            Self::Host(row) => row.validate()?,
            Self::Unavailable { project, path, .. } => {
                if *project == [0; 32] || !valid_path(path) {
                    return Err("invalid Vue unavailable host");
                }
            }
        }
        let mut bounded = BoundedWriter { count: 0 };
        serde_json::to_writer(&mut bounded, self).map_err(|_| "Vue relation row exceeds budget")?;
        Ok(())
    }
}
impl Relation for VueInventoryRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 18;
    const VERSION: u8 = 1;
    type Key = [u8; 32];
    type Value = VueInventoryRecord;
    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        // The closed data model has no fallible JSON values; callers validate
        // size before admission, and decoding enforces canonical bytes.
        serde_json::to_writer(output, value).expect("Vue inventory JSON is infallible");
    }
}
impl CanonicalRelation for VueInventoryRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes.try_into().map_err(|_| RelationDecodeError::Malformed)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        if bytes.len() > MAX_VALUE {
            return Err(RelationDecodeError::Malformed);
        }
        let row: VueInventoryRecord =
            serde_json::from_slice(bytes).map_err(|_| RelationDecodeError::Malformed)?;
        row.validate().map_err(|_| RelationDecodeError::Malformed)?;
        if serde_json::to_vec(&row).map_err(|_| RelationDecodeError::Malformed)? != bytes {
            return Err(RelationDecodeError::Malformed);
        }
        Ok(row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_version::{CanonicalRelation, Relation};

    #[test]
    fn canonical_row_reparse_fences_bytes_basis_and_forged_spans() {
        let source = "\u{feff}<!-- 🚀 -->\r\n<script setup lang='ts'>const café = 1;</script><style scoped>.x{color:red}</style>";
        let container = parse(source.as_bytes()).unwrap();
        let row = VueHostInventory::capture([1; 32], "src/Card.vue", [2; 32], &container).unwrap();
        assert!(row.is_partial());
        assert_eq!(row.blocks.len(), 2);
        assert_eq!(row.declarations.len(), 1);
        let mut encoded = Vec::new();
        VueInventoryRelation::encode_value(&VueInventoryRecord::Host(row.clone()), &mut encoded);
        let VueInventoryRecord::Host(decoded) =
            VueInventoryRelation::decode_value(&encoded).unwrap()
        else {
            panic!("host row expected")
        };
        assert_eq!(decoded, row);
        let verified = decoded
            .verify_host(
                source.as_bytes(),
                decoded.key(),
                [1; 32],
                "src/Card.vue",
                [2; 32],
            )
            .unwrap();
        assert_eq!(verified.coverage().script, VueCapabilityCoverage::Partial);
        assert_eq!(
            verified.coverage().template,
            VueCapabilityCoverage::Unavailable
        );
        assert_eq!(
            verified.coverage().style,
            VueCapabilityCoverage::Unavailable
        );
        assert_eq!(verified.declaration_count(), 1);
        let declaration = verified.declaration(0).unwrap();
        assert_eq!(declaration.name.text(), "café");
        assert_eq!(&source[declaration.name.byte_range()], "café");
        let page = verified.search_named("café", 1).unwrap();
        assert_eq!(page.declarations.len(), 1);
        assert!(!page.has_more);
        assert_eq!(page.coverage.script, VueCapabilityCoverage::Partial);
        let empty = verified.search_named("absent", 1).unwrap();
        assert!(empty.declarations.is_empty());
        assert_eq!(empty.coverage.template, VueCapabilityCoverage::Unavailable);
        assert!(
            decoded
                .verify_host(
                    source.replace('1', "2").as_bytes(),
                    decoded.key(),
                    [1; 32],
                    "src/Card.vue",
                    [2; 32]
                )
                .is_err()
        );
        assert!(
            decoded
                .verify_host(
                    source.as_bytes(),
                    decoded.key(),
                    [1; 32],
                    "src/Card.vue",
                    [3; 32]
                )
                .is_err()
        );
        assert!(
            decoded
                .verify_host(source.as_bytes(), [9; 32], [1; 32], "src/Card.vue", [2; 32])
                .is_err()
        );
        assert!(
            decoded
                .verify_host(
                    source.as_bytes(),
                    decoded.key(),
                    [1; 32],
                    "src/Other.vue",
                    [2; 32]
                )
                .is_err()
        );
        let cancelled = std::sync::atomic::AtomicBool::new(true);
        let stopped = tsz_common::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(10),
            &cancelled,
            5_000_000,
        );
        assert!(
            decoded
                .verify_host_with_checkpoint(
                    source.as_bytes(),
                    decoded.key(),
                    [1; 32],
                    "src/Card.vue",
                    [2; 32],
                    &stopped
                )
                .is_err()
        );
        assert!(decoded.matches_host_bytes(source.as_bytes()));
        assert!(!decoded.matches_host_bytes(source.replace('1', "2").as_bytes()));
        let mut wrong_producer = decoded.clone();
        wrong_producer.producer = [8; 32];
        assert!(
            wrong_producer
                .verify_host(
                    source.as_bytes(),
                    wrong_producer.key(),
                    [1; 32],
                    "src/Card.vue",
                    [2; 32]
                )
                .is_err()
        );
        let mut forged = decoded;
        forged.declarations[0].name.start += 1;
        assert!(
            forged
                .verify_host(
                    source.as_bytes(),
                    forged.key(),
                    [1; 32],
                    "src/Card.vue",
                    [2; 32]
                )
                .is_err()
        );
        encoded.push(b' ');
        assert!(VueInventoryRelation::decode_value(&encoded).is_err());
    }
    #[test]
    fn missing_and_external_inputs_never_become_complete_inventory() {
        let source = b"<script src='./x.js'></script><script lang='coffee'>x</script><style lang='scss'>.x {}</style>";
        let parsed = parse(source).unwrap();
        let row = VueHostInventory::capture([1; 32], "a.vue", [2; 32], &parsed).unwrap();
        assert_eq!(row.scripts[0].status, "NoInlineContent");
        assert_eq!(row.scripts[1].status, "UnsupportedLanguage");
        assert!(row.declarations.is_empty());
        assert!(row.is_partial());
        assert!(VueHostInventory::capture([1; 32], "../bad.vue", [2; 32], &parsed).is_err());
    }
    #[test]
    fn finite_host_search_does_not_upgrade_partial_coverage() {
        let source = b"<script>const same=1;</script><script setup>const same=2;</script>";
        let parsed = parse(source).unwrap();
        let row = VueHostInventory::capture([1; 32], "src/App.vue", [2; 32], &parsed).unwrap();
        let verified = row
            .verify_host(source, row.key(), [1; 32], "src/App.vue", [2; 32])
            .unwrap();
        let page = verified.search_named("same", 1).unwrap();
        assert_eq!(page.declarations.len(), 1);
        assert!(page.has_more);
        assert_eq!(page.coverage.script, VueCapabilityCoverage::Partial);
        assert!(verified.search_named("same", 0).is_err());
    }
}
