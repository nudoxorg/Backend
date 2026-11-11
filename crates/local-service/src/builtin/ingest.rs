//! Deterministic, parallel, content-versioned filesystem ingestion.

#[cfg(test)]
use super::source_budget::SourceAdmissionLimits;
use super::source_budget::{SourceAdmissionLedger, SourceAdmissionPolicy};
use super::source_frontier;
#[path = "ingest/go_project.rs"]
mod go_project;
use self::go_project::GoProjectCapture;
use backend_compile::{
    DeclarationKind, InputContentSchema, SourceExcerpt, SourceLanguage, SyntaxError,
    SyntaxFrontend, typed_of,
};
use backend_engine::builtin::{
    NATIVE_SOURCE_CHUNK_BYTES, NativePathDisposition, NativePathRefusal,
    ProductNativeFrontierRecord, ProductNativeFrontierRelation,
    build_native_frontier_records_with_content,
};
use backend_engine::{
    ProductSourceFileFactsUpdate, ProductSourceRecord, ProductSourceRelation, Relation,
    SourceUnavailableReason, build_product_source_file_facts, product_source_file_key,
};
use backend_frontend_python::legacy::checker::{
    PythonEnvironmentCapture, PythonEnvironmentInput, PythonProjectControl,
};
use backend_library::{
    DiscoveryPolicy, EntryKind, discover_source_entries, source_selection_policy,
};
use backend_semantic::ir::SemanticCoreReader;
use backend_semantic::vocabulary::{
    CSharpVersion, CStandard, CxxStandard, GoVersion, JavaRelease, Language, LanguageProfile,
    PythonVersion, RustEdition, TypeScriptSource,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, mpsc};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;

// One relation row can never exceed the canonical node capacity, so the
// per-record ceiling is that bound rather than an independent number that
// would admit records the tree must later reject.
const MAX_ENCODED_RECORD_BYTES: usize = ProductSourceRecord::ROW_VALUE_CAPACITY;
const MAX_DIRECTORY_ENTRIES: usize = 100_000;
const MAX_DISCOVERY_ENTRIES: usize = 500_000;
const MAX_COMPILER_WORKSPACE_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_COMPILER_WORKSPACE_STREAM_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_COMPILER_WORKSPACE_CHUNK_BYTES: usize = 1024 * 1024;
#[cfg(windows)]
const COMPILER_WORKSPACE_POLICY_IDENTITY: &str = "nudox.compiler-workspace.v1/gitignore+generated-defaults.v3;python-initialized-subpackages.v1;declared-captured-python-source-roots.v1;csharp-bin-source-filter.v1;ignore-case=insensitive;portable-case-collision=reject;symlink=reject";
#[cfg(not(windows))]
const COMPILER_WORKSPACE_POLICY_IDENTITY: &str = "nudox.compiler-workspace.v1/gitignore+generated-defaults.v3;python-initialized-subpackages.v1;declared-captured-python-source-roots.v1;csharp-bin-source-filter.v1;ignore-case=sensitive;portable-case-collision=reject;symlink=reject";
const MAX_COMPILER_CONFIGURATION_FILES_PER_LANGUAGE: usize = 4_096;
pub(super) const MAX_COMPILER_CONFIGURATION_FILE_BYTES: usize = 8 * 1024 * 1024;
const MAX_COMPILER_CONFIGURATION_BYTES_PER_LANGUAGE: usize = 32 * 1024 * 1024;
const RESULT_QUEUE_PER_WORKER: usize = 1;
pub(super) const INDEX_SCAN_CANCELLED: &str = "index scan cancelled";

fn check_scan_cancellation(cancellation: Option<&AtomicBool>) -> Result<(), String> {
    if cancellation.is_some_and(|cancellation| cancellation.load(Ordering::Acquire)) {
        Err(INDEX_SCAN_CANCELLED.to_owned())
    } else {
        Ok(())
    }
}

fn absolute_path_spelling(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|current| current.join(path))
            .map_err(|error| format!("resolve project path spelling: {error}"))
    }
}

/// One complete, deterministic project scan before comparison with the
/// selected versioned relation.
pub(super) struct IndexSnapshot {
    vue_scan_identity: std::sync::Arc<()>,
    pub(super) python_environment:
        Option<backend_frontend_python::legacy::checker::PythonEnvironmentInput>,
    pub(super) source_version: [u8; 32],
    /// Separate discovery evidence, not a compiler or supported-source member.
    pub(super) source_selection: backend_library::SourceSelectionObservation,
    /// Native CSS candidates witnessed by this same discovery walk.
    pub(super) stylo_walk: StyloWalkEvidence,
    /// SQL candidates witnessed by the same bounded, nonignored source walk.
    pub(super) sql_walk: SqlWalkEvidence,
    /// Vue candidates from the same confined discovery walk; not compiler members.
    pub(super) vue_walk: VueWalkEvidence,
    /// Exact source-ledger baseline before optional native Stylo work.
    stylo_budget_baseline: StyloBudgetBaseline,
    pub(super) files: Vec<([u8; 32], ProductSourceRecord)>,
    /// Complete file-facts updates produced before compact source-row retention.
    pub(super) source_facts: Vec<ProductSourceFileFactsUpdate>,
    /// Full persisted fact-page charges, including unchanged frontier files.
    pub(super) encoded_fact_bytes: Vec<([u8; 32], usize)>,
    pub(super) compiler_sources: Vec<CompilerSourceHandle>,
    pub(super) native_typescript_capture: Option<NativeTsCapture>,
    /// Exact per-path dispositions for complete Clang and C# profile frontiers.
    pub(super) native_frontier: Vec<ProductNativeFrontierRecord>,
    /// Parser-malformed, UTF-8 native sources eligible for the separate Clang route.
    pub(super) native_clang_candidates: Vec<NativeClangCandidateHandle>,
    /// Parser-malformed, UTF-8 C# sources eligible for the separate C# route.
    pub(super) native_csharp_candidates: Vec<NativeCSharpCandidateHandle>,
    pub(super) reused_compiler_files: Vec<ReusedCompilerFile>,
    pub(super) compiler_configuration: CompilerConfigurationSnapshot,
    pub(super) revision_fence: CompilerRevisionFence,
    /// Source bytes actually read by this scan. This is distinct from the
    /// byte-length budget, which is also charged for files admitted from a
    /// verified frontier.
    pub(super) source_bytes_read: usize,
    /// Limits pinned for source admission, compiler rereads, and warm-cache
    /// identity during this scan's downstream compilation.
    pub(super) source_admission_policy: SourceAdmissionPolicy,
}

#[derive(Clone, Copy)]
struct StyloBudgetBaseline {
    source_bytes: usize,
    record_bytes: usize,
    records: usize,
}

/// Private evidence produced by the same source discovery walk. A missing
/// inventory witness cannot authorize native CSS publication or negatives.
pub(super) struct StyloWalkEvidence {
    project: [u8; 32],
    root: PathBuf,
    files: Vec<CompilerFileRevision>,
    inventory: Option<(u64, [u8; 32])>,
    inventory_complete: bool,
    discovery_options: Vec<u8>,
    source_policy: [u8; 32],
    local_policy_paths: Vec<PathBuf>,
}

/// Raw same-walk SQL inventory claims; this is not selected source authority.
#[derive(Clone)]
pub(super) struct SqlWalkEvidence {
    project: [u8; 32],
    root: PathBuf,
    files: Vec<CompilerFileRevision>,
    inventory_complete: bool,
}
impl SqlWalkEvidence {
    pub(super) fn root(&self) -> &Path { &self.root }
    /// Number of candidate `.sql` files in the (possibly truncated) walk.
    pub(super) fn file_count(&self) -> usize { self.files.len() }
    /// Whether the bounded walk saw every candidate `.sql` file.
    pub(super) fn inventory_complete(&self) -> bool { self.inventory_complete }
    pub(super) fn checked_retained_bytes(&self) -> Option<usize> {
        let mut total = std::mem::size_of::<Self>();
        sql_charge_allocation(&mut total, self.root.capacity())?;
        sql_charge_allocation(&mut total, self.files.capacity().checked_mul(std::mem::size_of::<CompilerFileRevision>())?)?;
        for file in &self.files { sql_charge_allocation(&mut total, file.relative_path.capacity())?; }
        Some(total)
    }
}
impl StyloWalkEvidence {
    pub(super) fn project(&self) -> [u8; 32] {
        self.project
    }
    pub(super) fn root(&self) -> &Path {
        &self.root
    }
    pub(super) fn files(&self) -> &[CompilerFileRevision] {
        &self.files
    }
    pub(super) fn inventory(&self) -> Option<(u64, [u8; 32])> {
        self.inventory
    }
    pub(super) fn inventory_complete(&self) -> bool {
        self.inventory_complete
    }
    pub(super) fn discovery_options(&self) -> &[u8] {
        &self.discovery_options
    }
    pub(super) fn source_policy(&self) -> [u8; 32] {
        self.source_policy
    }
    pub(super) fn local_policy_paths(&self) -> &[PathBuf] {
        &self.local_policy_paths
    }
    /// Extends the existing directory/root fence with the CSS paths observed
    /// in the same walk. Callers must check this fence before and after reads.
    pub(super) fn bind_fence(
        &self,
        fence: &CompilerRevisionFence,
    ) -> Result<CompilerRevisionFence, String> {
        if self.root != fence.root {
            return Err("Stylo walk root differs from source fence".into());
        }
        let mut bound = fence.clone();
        bound.files.extend(self.files.iter().cloned());
        bound
            .files
            .sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
        bound.files.dedup_by(|left, right| {
            left.relative_path == right.relative_path && left.metadata == right.metadata
        });
        Ok(bound)
    }
}

/// Vue candidates witnessed by the same source walk. This alone grants no
/// declaration or workspace-publication authority.
#[derive(Clone)]
pub(super) struct VueWalkEvidence {
    project: [u8; 32],
    root: PathBuf,
    files: Vec<CompilerFileRevision>,
    complete: bool,
    discovery_options: Vec<u8>,
    source_policy: [u8; 32],
    local_policy_paths: Vec<PathBuf>,
}
impl VueWalkEvidence {
    pub(super) fn project(&self) -> [u8; 32] { self.project }
    pub(super) fn root(&self) -> &Path { &self.root }
    pub(super) fn files(&self) -> &[CompilerFileRevision] { &self.files }
    pub(super) fn complete(&self) -> bool { self.complete }
    fn bind_fence(&self, fence: &CompilerRevisionFence) -> Result<CompilerRevisionFence, String> {
        if self.root != fence.root { return Err("Vue walk root differs from source fence".into()); }
        let mut bound = fence.clone();
        bound.files.extend(self.files.iter().cloned());
        bound.files.sort_by(|a,b| a.relative_path.cmp(&b.relative_path));
        bound.files.dedup_by(|a,b| a.relative_path == b.relative_path && a.metadata == b.metadata);
        Ok(bound)
    }
}

/// Captured tree and file metadata used to reject a compiler result if known
/// source/configuration paths change after scanning. Source contents are
/// separately re-admitted at the compiler boundary before native execution.
#[derive(Clone)]
pub(super) struct CompilerRevisionFence {
    /// Absolute spelling supplied when this scan began, before symlink
    /// resolution. Retaining it lets later fences notice a retargeted alias.
    requested_root: PathBuf,
    /// Canonical root pinned when the scan began.
    root: PathBuf,
    directories: Vec<(PathBuf, Option<FileSystemRevision>)>,
    pub(super) files: Vec<CompilerFileRevision>,
    /// Native producer read closure retained through the final product commit.
    pub(super) typescript_reuse: Vec<backend_engine::application::TypeScriptSnapshotReuse>,
}

impl CompilerRevisionFence {
    /// Conservative heap charge for a retained native-only fence.
    pub(super) fn checked_retained_bytes(&self) -> Option<usize> {
        if !self.typescript_reuse.is_empty() { return None; }
        fn add(total: &mut usize, capacity: usize) -> Option<()> {
            if capacity != 0 { *total = total.checked_add(capacity)?.checked_add(2 * std::mem::size_of::<usize>())?; }
            Some(())
        }
        let mut total = std::mem::size_of::<Self>();
        add(&mut total, self.requested_root.capacity())?;
        add(&mut total, self.root.capacity())?;
        add(&mut total, self.directories.capacity().checked_mul(std::mem::size_of::<(PathBuf, Option<FileSystemRevision>)>())?)?;
        for (path, _) in &self.directories { add(&mut total, path.capacity())?; }
        add(&mut total, self.files.capacity().checked_mul(std::mem::size_of::<CompilerFileRevision>())?)?;
        for file in &self.files { add(&mut total, file.relative_path.capacity())?; }
        add(&mut total, self.typescript_reuse.capacity().checked_mul(std::mem::size_of::<backend_engine::application::TypeScriptSnapshotReuse>())?)?;
        Some(total)
    }
    /// Conservative retained-byte charge for the finite C# custody profile.
    /// Opaque TypeScript reuse proof allocation is not accounted here.
    pub(super) fn csharp_retained_bytes(&self) -> Option<usize> {
        if !self.typescript_reuse.is_empty() { return None; }
        let mut total = std::mem::size_of::<Self>()
            .checked_add(self.requested_root.capacity().checked_mul(4)?)?
            .checked_add(self.root.capacity().checked_mul(4)?)?
            .checked_add(self.directories.capacity().checked_mul(std::mem::size_of::<(PathBuf, Option<FileSystemRevision>)>())?)?
            .checked_add(self.files.capacity().checked_mul(std::mem::size_of::<CompilerFileRevision>())?)?
            .checked_add(self.typescript_reuse.capacity().checked_mul(std::mem::size_of::<backend_engine::application::TypeScriptSnapshotReuse>())?)?;
        for (path, _) in &self.directories { total = total.checked_add(path.capacity().checked_mul(4)?)?.checked_add(64)?; }
        for file in &self.files { total = total.checked_add(file.relative_path.capacity().checked_mul(4)?)?.checked_add(64)?; }
        total.checked_add(256)
    }

    /// Exact capacity-based charge for SQL's retained scan fence. Reuse
    /// witnesses are refused until their nested ownership is accounted for.
    pub(super) fn checked_sql_retained_bytes(&self) -> Option<usize> {
        if !self.typescript_reuse.is_empty() { return None; }
        let mut total = std::mem::size_of::<Self>();
        sql_charge_allocation(&mut total, self.requested_root.capacity())?;
        sql_charge_allocation(&mut total, self.root.capacity())?;
        sql_charge_allocation(&mut total, self.directories.capacity().checked_mul(std::mem::size_of::<(PathBuf, Option<FileSystemRevision>)>())?)?;
        for (path, _) in &self.directories { sql_charge_allocation(&mut total, path.capacity())?; }
        sql_charge_allocation(&mut total, self.files.capacity().checked_mul(std::mem::size_of::<CompilerFileRevision>())?)?;
        for file in &self.files { sql_charge_allocation(&mut total, file.relative_path.capacity())?; }
        sql_charge_allocation(&mut total, self.typescript_reuse.capacity().checked_mul(std::mem::size_of::<backend_engine::application::TypeScriptSnapshotReuse>())?)?;
        Some(total)
    }
    /// Canonical root pinned by this source scan. Filesystem reads that happen
    /// after admission must use this path rather than resolving the caller's
    /// original spelling again.
    pub(super) fn canonical_root(&self) -> &Path {
        &self.root
    }

    /// Original absolute root spelling bound by this revision fence.
    pub(super) fn requested_root(&self) -> &Path { &self.requested_root }

    /// Opens the exact root admitted by this scan after checking both the
    /// original path spelling and the root directory's captured identity.
    fn open_bound_root(&self, requested_root: &Path) -> Result<ProjectRoot, String> {
        let requested_spelling = absolute_path_spelling(requested_root)?;
        if requested_spelling != self.requested_root {
            return Err("project root spelling changed after source admission".to_owned());
        }
        let resolved = self
            .requested_root
            .canonicalize()
            .map_err(|error| format!("resolve admitted project root: {error}"))?;
        if resolved != self.root {
            return Err("project root alias now resolves to a different directory".to_owned());
        }
        let expected_revision = self
            .directories
            .iter()
            .find(|(relative, _)| relative.as_os_str().is_empty())
            .and_then(|(_, revision)| *revision)
            .ok_or_else(|| "admitted project root has no captured revision".to_owned())?;
        let capability = ProjectRoot::open(&self.root)?;
        if capability.revision()? != expected_revision {
            return Err("project root directory changed after source admission".to_owned());
        }
        Ok(capability)
    }
}

#[derive(Clone)]
pub(super) struct CompilerFileRevision {
    pub(super) relative_path: PathBuf,
    pub(super) metadata: Option<FileSystemRevision>,
}

/// Entry kind admitted into a complete compiler workspace inventory.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CompilerWorkspaceEntryKind {
    /// The workspace root or one admitted directory.
    Directory,
    /// An admitted regular file, including a zero-byte file.
    File,
}

/// One path in a complete, normalized compiler workspace inventory.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CompilerWorkspaceEntry {
    /// Portable root-relative path using `/`; the root itself is `""`.
    pub(super) path: String,
    /// Directory or regular-file kind. Links and special nodes fail admission.
    pub(super) kind: CompilerWorkspaceEntryKind,
    /// Exact file size; directories carry `None`.
    pub(super) byte_length: Option<u64>,
    revision: FileSystemRevision,
}

/// A complete admissible workspace view with a confined read capability and
/// an exact full-entry revalidation fence.
pub(super) struct CompilerWorkspaceSnapshot {
    requested_root: PathBuf,
    root: PathBuf,
    root_capability: ProjectRoot,
    policy: DiscoveryPolicy,
    entries: Vec<CompilerWorkspaceEntry>,
    fence_digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct FileSystemRevision {
    pub(super) length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    unix: (u64, u64, i64, i64),
    #[cfg(windows)]
    windows: backend_platform::win32::project_fs::FileRevision,
}

/// Bounded configuration and lockfile read set admitted by project discovery.
#[derive(Default)]
pub(super) struct CompilerConfigurationSnapshot {
    pub(super) files: Vec<CompilerConfigurationFile>,
    pub(super) complete_languages: BTreeSet<Language>,
    go_capture: GoProjectCapture,
}

/// One bounded, confined read of a compiler configuration input.
#[derive(Clone)]
pub(super) struct CompilerConfigurationFile {
    pub(super) language: Language,
    pub(super) relative_path: PathBuf,
    pub(super) content: [u8; 32],
}

/// Compact source capability retained between baseline parsing and compiler
/// admission. The compiler rereads this path through `ProjectRoot` and checks
/// the exact identity before constructing its bounded byte owner.
#[derive(Clone)]
pub(super) struct CompilerSourceHandle {
    pub(super) relative_path: String,
    pub(super) profile: LanguageProfile,
    pub(super) content: [u8; 32],
    /// Exact source-fact identity bound to the captured bytes, including unavailable rows.
    pub(super) source_fact_identity: SourceFactIdentity,
    /// Exact byte count for native-only inputs; legacy handles predate it.
    pub(super) expected_byte_length: Option<u32>,
}

/// Private same-walk native TS input. The compiler independently rereads the
/// source under the revision fence and checks both identities and its length;
/// the accompanying row is only a claim until owner admission.
struct NativeTsCapturedSource {
    evidence: backend_engine::builtin::NativeSourceEvidence,
    source_fact_identity: SourceFactIdentity,
}

/// One bounded native source observation under the already-open project root.
/// It deliberately makes no source-inventory completeness claim.
pub(super) struct NativeTsCapture {
    canonical_root: PathBuf,
    project: [u8; 32],
    sources: Vec<NativeTsCapturedSource>,
    revision_fence: CompilerRevisionFence,
    encoded_bytes: usize,
    source_version: [u8; 32],
    // Only a complete scan supplies this; helper-only observations cannot be
    // promoted to a publication witness by reconstructing policy later.
    fixed_discovery_policy: bool,
    source_policy: SourceAdmissionPolicy,
    project_source_version: Option<[u8; 32]>,
    cancellation: Option<Arc<AtomicBool>>,
}

/// A fresh positive-source rewalk of the original private capture. It carries
/// no workspace selection or inventory-completeness authority.
pub(super) struct RevalidatedNativeTsCapture {
    capture: NativeTsCapture,
    bytes_read: usize,
    cancellation: Arc<AtomicBool>,
}

impl RevalidatedNativeTsCapture {
    pub(super) fn matches_cancellation(&self, cancellation: &Arc<AtomicBool>) -> bool {
        Arc::ptr_eq(&self.cancellation, cancellation)
    }
    /// Conservative retained allocation charge; source rewalk I/O and the
    /// enclosing Arc allocation header are charged by the owning plan.
    pub(super) fn checked_retained_bytes(&self) -> Option<usize> {
        let mut total = std::mem::size_of::<Self>();
        fn allocation(total: &mut usize, capacity: usize) -> Option<()> {
            if capacity != 0 {
                *total = total.checked_add(capacity)?.checked_add(2 * std::mem::size_of::<usize>())?;
            }
            Some(())
        }
        let capture = &self.capture;
        let fence = &capture.revision_fence;
        // Native-only scans do not retain compiler-reuse witnesses. Their
        // nested heap ownership requires a separate accounting contract.
        if !fence.typescript_reuse.is_empty() { return None; }
        allocation(&mut total, capture.canonical_root.capacity())?;
        allocation(&mut total, fence.requested_root.capacity())?;
        allocation(&mut total, fence.root.capacity())?;
        allocation(&mut total, capture.sources.capacity().checked_mul(std::mem::size_of::<NativeTsCapturedSource>())?)?;
        for source in &capture.sources {
            allocation(&mut total, source.evidence.path.capacity())?;
            if let Some(lines) = &source.evidence.line_starts {
                allocation(&mut total, lines.capacity().checked_mul(std::mem::size_of::<u32>())?)?;
            }
            if let Some(headers) = &source.evidence.headers {
                allocation(&mut total, headers.entries.capacity().checked_mul(std::mem::size_of::<backend_engine::builtin::NativeHeaderEntry>())?)?;
            }
        }
        allocation(&mut total, fence.directories.capacity().checked_mul(std::mem::size_of::<(PathBuf, Option<FileSystemRevision>)>())?)?;
        for (path, _) in &fence.directories { allocation(&mut total, path.capacity())?; }
        allocation(&mut total, fence.files.capacity().checked_mul(std::mem::size_of::<CompilerFileRevision>())?)?;
        for file in &fence.files { allocation(&mut total, file.relative_path.capacity())?; }
        allocation(&mut total, fence.typescript_reuse.capacity().checked_mul(std::mem::size_of::<backend_engine::application::TypeScriptSnapshotReuse>())?)?;
        // Conservatively charge both Arc owners and their shared allocation.
        total.checked_add(3 * std::mem::size_of::<usize>())
    }
    pub(super) fn project(&self) -> [u8; 32] { self.capture.project }
    pub(super) fn canonical_root(&self) -> &Path { &self.capture.canonical_root }
    pub(super) fn project_source_version(&self) -> [u8; 32] {
        self.capture.project_source_version.expect("validated project source basis")
    }
    pub(super) fn bytes_read(&self) -> usize { self.bytes_read }
    /// Repeats the original bounded positive-source observation on a worker.
    pub(super) fn revalidate_again(&self, bytes_read: &mut usize) -> Result<(), String> {
        self.capture.revalidate(Some(self.cancellation.as_ref()), bytes_read)
    }
    pub(super) fn fence_is_current(&self) -> Result<bool, String> {
        compiler_revision_is_current_with_cancellation(&self.capture.revision_fence, Some(self.cancellation.as_ref()))
    }
    pub(super) fn cancelled(&self) -> bool {
        self.cancellation.load(Ordering::Acquire)
    }
    pub(super) fn program_image_evidence(
        &self,
        activated: &super::ActivatedProductSemantics,
        headers: Option<&super::commands::index::NativeHeaderCustody>,
    ) -> Result<Vec<backend_engine::builtin::NativeSourceEvidence>, String> {
        self.capture.program_image_evidence(activated, headers)
    }
}

impl IndexSnapshot {
    /// Native-only discovery has unwitnessed global/ancestor ignore inputs.
    /// Its source digest cannot independently establish a current program.
    pub(super) fn native_typescript_discovery_incomplete(&self) -> bool {
        self.native_typescript_capture.is_some()
    }
}

impl NativeTsCapture {
    pub(super) fn into_revalidated(
        self,
        bytes_read: &mut usize,
    ) -> Result<RevalidatedNativeTsCapture, String> {
        let cancellation = self.cancellation.as_ref().ok_or("native TypeScript capture has no original cancellation")?.clone();
        let before = *bytes_read;
        self.revalidate(Some(cancellation.as_ref()), bytes_read)?;
        let charged = bytes_read.checked_sub(before).ok_or("native rewalk accounting overflow")?;
        if self.project_source_version.is_none() {
            return Err("native TypeScript capture has no project source basis".into());
        }
        Ok(RevalidatedNativeTsCapture { capture: self, bytes_read: charged, cancellation })
    }
    /// Joins a compiler-admitted activation to the private scan witness. The
    /// activation must already have been admitted under its exact product key
    /// and claim; this method does not select a workspace or prove freshness.
    /// Forest programs seal image digests; legacy Single programs instead rely
    /// on the caller's exact admitted compiler closure for image membership.
    pub(super) fn program_image_evidence(
        &self,
        activated: &super::ActivatedProductSemantics,
        headers: Option<&super::commands::index::NativeHeaderCustody>,
    ) -> Result<Vec<backend_engine::builtin::NativeSourceEvidence>, String> {
        let mut header_claims = BTreeMap::new();
        if let Some(headers) = headers {
            for header in headers.entries() {
                if header_claims.insert(header.path.as_str(), header).is_some() {
                    return Err("duplicate native header source".into());
                }
            }
        }
        let program = activated.native_program_sources.as_ref()
            .ok_or("native TypeScript program is unavailable")?;
        let captured: BTreeMap<_, _> = self.sources.iter()
            .map(|source| (source.evidence.path.as_str(), source)).collect();
        if captured.len() != self.sources.len() {
            return Err("duplicate native TypeScript captured path".into());
        }
        let mut members = BTreeMap::new();
        for row in program.sources() {
            let Some(path) = row.package_path else { continue };
            let source = captured.get(path).ok_or("native program package source was not captured")?;
            if row.source.identity != source.source_fact_identity
                || row.source.byte_len != source.evidence.byte_length
                || members.insert(path, row.source).is_some()
            {
                return Err("native program source differs from the captured input".into());
            }
        }
        let mut witnessed = BTreeSet::new();
        let mut admitted_headers = BTreeMap::new();
        for image in activated.images() {
            let view = image.reopen().map_err(|error| format!("reopen native image: {error}"))?;
            let backend_semantic::ir::ImageProvenance::Captured { source, recipe, scope, .. } =
                view.image_facts().provenance else { return Err("native image provenance unavailable".into()) };
            let path = view.atom(scope.path).and_then(|bytes| std::str::from_utf8(bytes).ok())
                .ok_or("native image path unavailable")?;
            let captured_source = captured.get(path).ok_or("native image source was not captured")?;
            if recipe.tool != backend_semantic::vocabulary::NativeTool::TypeScriptCompiler
                || recipe.toolchain != program.toolchain()
                || recipe.profile != captured_source.evidence.profile
                || members.get(path) != Some(&source)
                || !program.image_matches(path, backend_semantic::ir::SemanticImageIdentity::from_encoded_bytes(image.as_ref()).as_ref())
                || !witnessed.insert(path.to_owned())
            {
                return Err("native image is not an exact program member".into());
            }
            if let Some(header) = header_claims.remove(path) {
                if header.source != source
                    || header.headers.producer_recipe != super::native_ts_headers::producer_recipe()
                { return Err("native header producer or source differs".into()); }
                let mut row = captured_source.evidence.clone();
                row.headers = Some(header.headers.clone());
                row.validate().map_err(str::to_owned)?;
                super::native_ts_headers::admit_image(&row, &view, image.as_ref())?;
                admitted_headers.insert(path.to_owned(), header.headers.clone());
            }
        }
        if !header_claims.is_empty() { return Err("native headers lack their selected image".into()); }
        // A mapped program input is source evidence even when this profile
        // did not emit an image for it. Keep that weaker fact distinct from
        // an admitted image; consumers must not infer declarations from it.
        let mut evidence = members.keys().map(|path| {
            let mut row = captured.get(path)
                .ok_or("native program member lost its captured source")?.evidence.clone();
            row.kind = if witnessed.contains(*path) {
                backend_engine::builtin::NativeSourceEvidenceKind::Image
            } else {
                backend_engine::builtin::NativeSourceEvidenceKind::ProgramMember
            };
            row.headers = admitted_headers.remove(*path);
            Ok(row)
        }).collect::<Result<Vec<_>, &str>>()?;
        evidence.sort_unstable_by(|left, right| left.path.cmp(&right.path));
        Ok(evidence)
    }

    /// Rewalks the original discovery policy and rereads exact TS inputs under
    /// the original root/configuration fence. This proves only the observed
    /// positives at this check: ambient ignore inputs remain unwitnessed and
    /// inventory completeness remains unknown. The caller must charge the
    /// returned read work and recheck the fence before final publication.
    pub(super) fn revalidate(
        &self,
        cancellation: Option<&AtomicBool>,
        bytes_read: &mut usize,
    ) -> Result<(), String> {
        if !self.fixed_discovery_policy {
            return Err("native TypeScript capture has no fixed scan policy".into());
        }
        if !compiler_revision_is_current_with_cancellation(&self.revision_fence, cancellation)? {
            return Err("native TypeScript capture fence changed".into());
        }
        let root = self.revision_fence.open_bound_root(&self.revision_fence.requested_root)?;
        let walk = project_paths_with_policy(
            &self.canonical_root, &root, None, true, source_selection_policy(), cancellation,
        )?;
        if !walk.sources.is_empty() {
            return Err("native TypeScript rewalk found mixed legacy source lanes".into());
        }
        let mut ledger = SourceAdmissionLedger::default();
        let current = capture_native_typescript_sources(
            &self.canonical_root, &walk.native_typescript_paths, self.project,
            self.source_policy, &mut ledger, &self.revision_fence, bytes_read, cancellation,
        )?;
        if current.source_version != self.source_version
            || current.sources.len() != self.sources.len()
            || current.sources.iter().zip(&self.sources).any(|(left, right)| {
                left.evidence != right.evidence || left.source_fact_identity != right.source_fact_identity
            })
        {
            return Err("native TypeScript source observation changed during rewalk".into());
        }
        Ok(())
    }
}

fn capture_native_typescript_sources(
    root: &Path,
    paths: &[PathBuf],
    project: [u8; 32],
    policy: SourceAdmissionPolicy,
    ledger: &mut SourceAdmissionLedger,
    revision_fence: &CompilerRevisionFence,
    bytes_read: &mut usize,
    cancellation: Option<&AtomicBool>,
) -> Result<NativeTsCapture, String> {
    use backend_engine::Relation as _;
    if !root.is_absolute()
        || root.canonicalize().map_err(|error| error.to_string())? != root
        || revision_fence.canonical_root() != root
        || !compiler_revision_is_current_with_cancellation(revision_fence, cancellation)?
    {
        return Err("native TypeScript project root or revision fence is not current".into());
    }
    let root_capability = revision_fence.open_bound_root(&revision_fence.requested_root)?;
    let root_text = root
        .to_str()
        .ok_or("native TypeScript project root is not exact UTF-8")?;
    if backend_engine::PackageKey::from_value(root_text).to_bytes() != project {
        return Err("native TypeScript project key does not bind its canonical root".into());
    }
    if paths.len() > backend_engine::builtin::MAX_NATIVE_EVIDENCE_PROJECT_ROWS {
        return Err("native TypeScript project source count exceeds bound".into());
    }
    let mut ordered = Vec::with_capacity(paths.len());
    let mut path_work = 0usize;
    for path in paths {
        check_scan_cancellation(cancellation)?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| "native TypeScript path escaped project")?;
        let normalized = normalized_workspace_path(relative)?;
        path_work = path_work
            .checked_add(normalized.len())
            .and_then(|work| work.checked_add(64))
            .ok_or("native TypeScript path work overflow")?;
        if path_work > backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES {
            return Err("native TypeScript path work exceeds bound".into());
        }
        ordered.push((normalized, relative));
    }
    ordered.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if ordered.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        return Err("native TypeScript source paths are duplicate".into());
    }
    let mut captured = Vec::with_capacity(paths.len());
    let mut encoded_total = 0usize;
    let mut source_version = blake3::Hasher::new();
    source_version.update(b"nudox.native-typescript-source-set.v1\0");
    source_version.update(&(paths.len() as u64).to_be_bytes());
    for (normalized, relative) in ordered {
        check_scan_cancellation(cancellation)?;
        let profile = admitted_file_profile(SourceLanguage::TypeScript, &normalized)
            .ok_or("native TypeScript path has no supported profile")?;
        let limit = ledger.native_retained_read_limit(policy);
        let bytes = match root_capability.read(relative, limit) {
            Ok(bytes) => bytes,
            Err(fault) => {
                let observed = match fault {
                    SourceFault::UnavailableRead(_, count) => count,
                    _ => 0,
                };
                *bytes_read = bytes_read
                    .checked_add(observed)
                    .ok_or("native TypeScript read count overflow")?;
                return Err(format!(
                    "native TypeScript source unavailable: {normalized}"
                ));
            }
        };
        *bytes_read = bytes_read
            .checked_add(bytes.len())
            .ok_or("native TypeScript read count overflow")?;
        std::str::from_utf8(&bytes)
            .map_err(|_| format!("native TypeScript source is not UTF-8: {normalized}"))?;
        let byte_length =
            u32::try_from(bytes.len()).map_err(|_| "native TypeScript file length overflow")?;
        let content = typed_of::<InputContentSchema>(&bytes).to_bytes();
        let source_fact_identity = SourceFactIdentity::from_canonical_bytes(&bytes);
        let evidence = backend_engine::builtin::NativeSourceEvidence {
            project,
            profile,
            path: normalized.clone(),
            input_digest: content,
            source_digest: *source_fact_identity.as_ref(),
            byte_length,
            kind: backend_engine::builtin::NativeSourceEvidenceKind::Image,
            line_starts: backend_engine::builtin::NativeSourceEvidence::capture_line_starts(&bytes),
            headers: None,
        };
        evidence.validate().map_err(str::to_owned)?;
        let mut encoded = Vec::new();
        backend_engine::builtin::NativeSourceEvidenceRelation::encode_value(
            &evidence,
            &mut encoded,
        );
        let row_charge = encoded
            .len()
            .checked_add(40)
            .ok_or("native TypeScript row charge overflow")?;
        encoded_total = encoded_total
            .checked_add(row_charge)
            .ok_or("native TypeScript aggregate overflow")?;
        if encoded_total > backend_engine::builtin::MAX_NATIVE_EVIDENCE_BYTES {
            return Err("native TypeScript evidence byte budget exceeded".into());
        }
        // Reserve the eventual compiler materialization budget conservatively.
        // The scan's Vec is dropped after its identities have been derived.
        ledger
            .admit_with_fact_pages(
                policy,
                policy
                    .admit_actual_file(bytes.len())
                    .map_err(|error| error.to_string())?,
                row_charge,
                0,
                true,
            )
            .map_err(|error| error.to_string())?;
        source_version.update(&<[u8; 2]>::from(profile));
        source_version.update(&(normalized.len() as u32).to_be_bytes());
        source_version.update(normalized.as_bytes());
        source_version.update(&content);
        source_version.update(source_fact_identity.as_ref());
        source_version.update(&byte_length.to_be_bytes());
        captured.push(NativeTsCapturedSource {
            evidence,
            source_fact_identity,
        });
    }
    check_scan_cancellation(cancellation)?;
    if !compiler_revision_is_current_with_cancellation(revision_fence, cancellation)? {
        return Err("native TypeScript root or configuration changed during capture".into());
    }
    Ok(NativeTsCapture {
        canonical_root: root.to_path_buf(),
        project,
        sources: captured,
        revision_fence: revision_fence.clone(),
        encoded_bytes: encoded_total,
        source_version: *source_version.finalize().as_bytes(),
        fixed_discovery_policy: false,
        source_policy: policy,
        project_source_version: None,
        cancellation: None,
    })
}

/// Locally observed discovery-policy inputs for native TS source-version
/// composition. The discovery walker can also consult ancestor and global Git
/// excludes, which this digest does not witness. It must not be used as proof
/// of a complete inventory or as a substitute for a fresh publication walk.
/// Missing or unreadable local policy files refuse the native scan.
fn native_typescript_discovery_basis(
    walk: &StyloWalkEvidence,
    fence: &CompilerRevisionFence,
    cancellation: Option<&AtomicBool>,
    bytes_read: &mut usize,
) -> Result<[u8; 32], String> {
    if walk.root != fence.root || !compiler_revision_is_current_with_cancellation(fence, cancellation)? {
        return Err("native TypeScript discovery fence is not current".into());
    }
    let root = fence.open_bound_root(&fence.requested_root)?;
    let mut paths = Vec::with_capacity(walk.local_policy_paths.len());
    for path in &walk.local_policy_paths {
        let relative = path.strip_prefix(&walk.root).map_err(|_| "native discovery policy escaped project")?;
        paths.push((normalized_workspace_path(relative)?, relative));
    }
    paths.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    if paths.windows(2).any(|pair| pair[0].0 == pair[1].0) { return Err("duplicate native discovery policy path".into()); }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.native-typescript-fixed-source-selection-local-incomplete.v1\0");
    hash.update(&(walk.discovery_options.len() as u64).to_be_bytes());
    hash.update(&walk.discovery_options);
    hash.update(&walk.source_policy);
    hash.update(&(paths.len() as u64).to_be_bytes());
    let mut remaining = 16usize * 1024 * 1024;
    for (name, relative) in paths {
        check_scan_cancellation(cancellation)?;
        let bytes = match root.read(relative, remaining.min(1024 * 1024)) {
            Ok(bytes) => bytes,
            Err(fault) => {
                if let SourceFault::UnavailableRead(_, count) = fault {
                    *bytes_read = bytes_read.checked_add(count).ok_or("native policy read count overflow")?;
                }
                return Err(format!("native discovery policy unavailable: {name}"));
            }
        };
        *bytes_read = bytes_read.checked_add(bytes.len()).ok_or("native policy read count overflow")?;
        remaining = remaining.checked_sub(bytes.len()).ok_or("native policy byte budget exceeded")?;
        hash.update(&(name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
        hash.update(&(bytes.len() as u64).to_be_bytes());
        hash.update(&bytes);
    }
    if !compiler_revision_is_current_with_cancellation(fence, cancellation)? {
        return Err("native TypeScript discovery changed during capture".into());
    }
    Ok(*hash.finalize().as_bytes())
}

/// Exact parser-malformed source admitted only to the separate local native
/// Clang candidate route. Callers must use the dedicated materializer.
#[derive(Clone)]
pub(super) struct NativeClangCandidateHandle {
    pub(super) relative_path: String,
    pub(super) profile: LanguageProfile,
    pub(super) content: [u8; 32],
    pub(super) source_fact_identity: SourceFactIdentity,
    pub(super) source_bytes: Option<Box<[u8]>>,
}

/// Exact parser-malformed source admitted only to the separate local Roslyn
/// candidate route. Callers must use the dedicated materializer.
#[derive(Clone)]
pub(super) struct NativeCSharpCandidateHandle {
    pub(super) relative_path: String,
    pub(super) profile: LanguageProfile,
    pub(super) content: [u8; 32],
    pub(super) source_fact_identity: SourceFactIdentity,
    pub(super) source_bytes: Option<Box<[u8]>>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct NativeCandidateProfiles {
    pub(super) clang: BTreeSet<LanguageProfile>,
    pub(super) csharp: BTreeSet<LanguageProfile>,
}

impl NativeCandidateProfiles {
    pub(super) fn contains(&self, profile: &LanguageProfile) -> bool {
        self.clang.contains(profile) || self.csharp.contains(profile)
    }
}

/// Exact source bytes admitted for one semantic authority slot; Python may remain undecoded.
#[derive(Clone)]
pub(super) struct CompilerSource {
    pub(super) relative_path: String,
    pub(super) profile: LanguageProfile,
    pub(super) source: Vec<u8>,
    pub(super) content: [u8; 32],
    /// Exact source-fact identity revalidated during compiler admission.
    pub(super) source_fact_identity: SourceFactIdentity,
}

/// A file whose prior analysis is still exact, so the scan keeps its content
/// hash and drops the compiler text until a sibling change forces a compile.
#[derive(Clone)]
pub(super) struct ReusedCompilerFile {
    pub(super) relative_path: String,
    pub(super) profile: LanguageProfile,
    pub(super) content: [u8; 32],
    /// Exact source-fact identity copied from the validated source row.
    pub(super) source_fact_identity: SourceFactIdentity,
}

/// Domain-typed identity of the canonical bytes consumed by semantic compilers.
pub(super) type SourceFactIdentity = backend_version::ContentId<backend_version::SourceFactDomain>;

struct ScannedFile {
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    /// Source size charged against the project's bounded ingest budget.
    source_bytes: usize,
    /// Bytes actually returned by a source read in this scan.
    source_bytes_read: usize,
    encoded_record_bytes: usize,
    encoded_fact_bytes: usize,
    /// `None` when the claiming frontend has no semantic profile for this
    /// extension: the file is still a project row, it simply carries nothing
    /// a compiler could be asked to analyse.
    compiler_source: Option<CompilerSourceHandle>,
    reused_compiler: Option<ReusedCompilerFile>,
    source_facts: Option<ProductSourceFileFactsUpdate>,
    native_frontier: Option<(LanguageProfile, NativePathDisposition)>,
    native_clang_candidate: Option<NativeClangCandidateHandle>,
    native_csharp_candidate: Option<NativeCSharpCandidateHandle>,
}

/// An opened project directory capability. Source reads resolve every path
/// component relative to this descriptor with symlink following disabled, so
/// a concurrently modified checkout cannot redirect ingestion outside the
/// admitted project after discovery.
struct ProjectRoot {
    #[cfg(unix)]
    directory: fs::File,
    #[cfg(windows)]
    directory: backend_platform::win32::project_fs::ProjectRoot,
    #[cfg(not(any(unix, windows)))]
    canonical: PathBuf,
}

/// Result of reopening a selected source through the same confined reader used
/// by discovery. A successful read still requires the caller to verify the
/// selected host's content identities before exposing any source extent.
#[derive(Debug, Eq, PartialEq)]
pub(super) enum SelectedSourceReadError {
    Vanished,
    TooLarge,
    Unavailable,
    Invalid,
}

pub(super) fn read_stylo_selected_source(
    root: &Path,
    relative: &str,
    maximum_bytes: usize,
) -> Result<Vec<u8>, SelectedSourceReadError> {
    let root = ProjectRoot::open(root).map_err(|_| SelectedSourceReadError::Unavailable)?;
    root.read(Path::new(relative), maximum_bytes).map_err(|fault| match fault {
        SourceFault::Vanished => SelectedSourceReadError::Vanished,
        SourceFault::Unavailable(SourceUnavailableReason::TooLarge)
        | SourceFault::UnavailableRead(SourceUnavailableReason::TooLarge, _) => {
            SelectedSourceReadError::TooLarge
        }
        SourceFault::Unavailable(_) | SourceFault::UnavailableRead(_, _) => {
            SelectedSourceReadError::Unavailable
        }
        SourceFault::Fatal(_) => SelectedSourceReadError::Invalid,
    })
}

impl ProjectRoot {
    fn open(path: &Path) -> Result<Self, String> {
        #[cfg(unix)]
        {
            use rustix::fs::{Mode, OFlags, open};
            let directory = open(
                path,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(fs::File::from)
            .map_err(|error| format!("open project directory {}: {error}", path.display()))?;
            Ok(Self { directory })
        }
        #[cfg(windows)]
        {
            let directory = backend_platform::win32::project_fs::ProjectRoot::open(path)
                .map_err(|error| format!("open project directory {}: {error}", path.display()))?;
            Ok(Self { directory })
        }
        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                canonical: path.to_path_buf(),
            })
        }
    }

    fn revision(&self) -> Result<FileSystemRevision, String> {
        #[cfg(unix)]
        let metadata = self
            .directory
            .metadata()
            .map_err(|error| format!("stat opened workspace root: {error}"))?;
        #[cfg(windows)]
        {
            return self
                .directory
                .revision()
                .map(file_system_revision_from_windows)
                .map_err(|error| format!("stat opened workspace root: {error}"));
        }
        #[cfg(not(any(unix, windows)))]
        let metadata = fs::symlink_metadata(&self.canonical)
            .map_err(|error| format!("stat workspace root: {error}"))?;
        #[cfg(not(windows))]
        Ok(file_system_revision_from_metadata(&metadata))
    }

    /// Opens one project-relative path, following no symlink at any step.
    ///
    /// Each component is resolved against the previously opened directory
    /// descriptor, so a checkout mutated during ingestion cannot redirect the
    /// read outside the admitted project.
    #[cfg(unix)]
    fn open_confined(&self, relative: &Path) -> Result<fs::File, SourceFault> {
        use rustix::fs::{Mode, OFlags, openat};
        let escaped = || SourceFault::Fatal("source path escaped its project root".to_owned());
        let components = relative.components().collect::<Vec<_>>();
        let (last, parents) = components.split_last().ok_or_else(escaped)?;
        let mut directory = self
            .directory
            .try_clone()
            .map_err(|error| SourceFault::Fatal(error.to_string()))?;
        for component in parents {
            let std::path::Component::Normal(name) = component else {
                return Err(escaped());
            };
            directory = openat(
                &directory,
                *name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(fs::File::from)
            .map_err(|error| SourceFault::from_open(error.kind()))?;
        }
        let std::path::Component::Normal(name) = last else {
            return Err(escaped());
        };
        openat(
            &directory,
            *name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(fs::File::from)
        .map_err(|error| SourceFault::from_open(error.kind()))
    }

    fn read(&self, relative: &Path, maximum_bytes: usize) -> Result<Vec<u8>, SourceFault> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(SourceFault::Fatal(
                "source path is not a confined relative path".to_owned(),
            ));
        }
        #[cfg(unix)]
        {
            read_bounded(self.open_confined(relative)?, maximum_bytes)
        }
        #[cfg(windows)]
        {
            let segments = windows_relative_segments(relative).map_err(SourceFault::Fatal)?;
            let file = self
                .directory
                .open_file_read(&segments)
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            read_bounded(file, maximum_bytes)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let path = self.canonical.join(relative);
            let canonical = path
                .canonicalize()
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            if !canonical.starts_with(&self.canonical) {
                return Err(SourceFault::Fatal(
                    "source path escaped its project root".to_owned(),
                ));
            }
            let file = std::fs::File::open(canonical)
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            read_bounded(file, maximum_bytes)
        }
    }

    fn read_compiler_configuration(
        &self,
        relative: &Path,
        maximum_bytes: usize,
    ) -> Result<Vec<u8>, SourceFault> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err(SourceFault::Fatal(
                "compiler configuration path is not confined".to_owned(),
            ));
        }
        #[cfg(unix)]
        {
            read_bounded(self.open_confined(relative)?, maximum_bytes)
        }
        #[cfg(windows)]
        {
            let segments = windows_relative_segments(relative).map_err(SourceFault::Fatal)?;
            let file = self
                .directory
                .open_file_read(&segments)
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            read_bounded(file, maximum_bytes)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let path = self.canonical.join(relative);
            let canonical = path
                .canonicalize()
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            if !canonical.starts_with(&self.canonical) {
                return Err(SourceFault::Fatal(
                    "compiler configuration escaped its project root".to_owned(),
                ));
            }
            let file = std::fs::File::open(canonical)
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            read_bounded(file, maximum_bytes)
        }
    }

    fn compiler_configuration_revision_or_absent(
        &self,
        relative: &Path,
    ) -> Result<Option<FileSystemRevision>, String> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|part| !matches!(part, std::path::Component::Normal(_)))
        {
            return Err("Go module candidate is not confined".to_owned());
        }
        #[cfg(unix)]
        let opened = self.open_confined(relative);
        #[cfg(windows)]
        let opened = self
            .directory
            .open_file_read(&windows_relative_segments(relative)?)
            .map_err(|error| SourceFault::from_open(error.kind()));
        #[cfg(not(any(unix, windows)))]
        let opened = (|| {
            let path = self.canonical.join(relative);
            let canonical = path
                .canonicalize()
                .map_err(|error| SourceFault::from_open(error.kind()))?;
            if canonical != path || !canonical.starts_with(&self.canonical) {
                return Err(SourceFault::Fatal(
                    "Go module candidate escaped its root".to_owned(),
                ));
            }
            fs::File::open(canonical).map_err(|error| SourceFault::from_open(error.kind()))
        })();
        match opened {
            Ok(file) => {
                if !file
                    .metadata()
                    .map_err(|error| error.to_string())?
                    .is_file()
                {
                    return Err(format!(
                        "Go module candidate {} is not a regular file",
                        relative.display()
                    ));
                }
                file_system_revision_from_open_file(&file).map(Some)
            }
            Err(SourceFault::Vanished) => Ok(None),
            Err(_) => Err(format!(
                "Go module candidate {} is not safely readable",
                relative.display()
            )),
        }
    }

    fn open_workspace_file(&self, relative: &Path) -> Result<fs::File, String> {
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, std::path::Component::Normal(_)))
        {
            return Err("workspace path is not a confined relative path".to_owned());
        }
        #[cfg(unix)]
        {
            self.open_confined(relative).map_err(|error| match error {
                SourceFault::Fatal(detail) => detail,
                SourceFault::Unavailable(_)
                | SourceFault::UnavailableRead(_, _)
                | SourceFault::Vanished => "workspace file changed or became unreadable".to_owned(),
            })
        }
        #[cfg(windows)]
        {
            let segments = windows_relative_segments(relative)?;
            self.directory
                .open_file_read(&segments)
                .map_err(|error| format!("open workspace file: {error}"))
        }
        #[cfg(not(any(unix, windows)))]
        {
            let path = self.canonical.join(relative);
            let canonical = path
                .canonicalize()
                .map_err(|error| format!("open workspace file: {error}"))?;
            if !canonical.starts_with(&self.canonical) || canonical != path {
                return Err("workspace path escaped or aliased its project root".to_owned());
            }
            fs::File::open(canonical).map_err(|error| format!("open workspace file: {error}"))
        }
    }

    #[cfg(windows)]
    fn revision_relative(&self, relative: &Path) -> Result<FileSystemRevision, String> {
        if relative.as_os_str().is_empty() {
            return self.revision();
        }
        let segments = windows_relative_segments(relative)?;
        self.directory
            .revision_relative(&segments)
            .map(file_system_revision_from_windows)
            .map_err(|error| format!("stat opened workspace entry: {error}"))
    }
}

#[cfg(windows)]
fn windows_relative_segments(relative: &Path) -> Result<Vec<&str>, String> {
    let mut segments = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err("project path contains a non-normal component".to_owned());
        };
        let name = name
            .to_str()
            .ok_or_else(|| "project path component is not valid UTF-8".to_owned())?;
        if name.is_empty() || name.contains(['/', '\\']) {
            return Err("project path component is invalid".to_owned());
        }
        segments.push(name);
    }
    if segments.is_empty() {
        return Err("project path is empty".to_owned());
    }
    Ok(segments)
}

#[cfg(windows)]
fn open_canonical_project_root(root: &Path) -> Result<(PathBuf, ProjectRoot), String> {
    let requested = ProjectRoot::open(root)?;
    let requested_revision = requested.revision()?;
    let canonical = root
        .canonicalize()
        .map_err(|error| format!("resolve project root {}: {error}", root.display()))?;
    let opened = ProjectRoot::open(&canonical)?;
    if opened.revision()? != requested_revision {
        return Err("project root changed while it was being opened".to_owned());
    }
    Ok((canonical, opened))
}

#[derive(Clone, Debug)]
struct ConfinedRootIdentity {
    requested_root: PathBuf,
    root: PathBuf,
    revision: FileSystemRevision,
}

impl ConfinedRootIdentity {
    fn from_snapshot(snapshot: &CompilerWorkspaceSnapshot) -> Result<Self, String> {
        let revision = snapshot
            .entries
            .first()
            .filter(|entry| entry.path.is_empty())
            .map(|entry| entry.revision)
            .ok_or_else(|| "captured workspace root revision is missing".to_owned())?;
        Ok(Self {
            requested_root: snapshot.requested_root.clone(),
            root: snapshot.root.clone(),
            revision,
        })
    }
    fn matches_snapshot(&self, snapshot: &CompilerWorkspaceSnapshot) -> bool {
        snapshot.requested_root == self.requested_root
            && snapshot.root == self.root
            && snapshot
                .entries
                .first()
                .filter(|entry| entry.path.is_empty())
                .map(|entry| entry.revision)
                == Some(self.revision)
    }
    fn matches_fence(&self, fence: &CompilerRevisionFence) -> bool {
        self.requested_root == fence.requested_root
            && self.root == fence.root
            && fence
                .directories
                .iter()
                .find(|(path, _)| path.as_os_str().is_empty())
                .and_then(|(_, revision)| *revision)
                == Some(self.revision)
    }
    fn retained_bytes(&self) -> Option<usize> {
        self.requested_root
            .capacity()
            .checked_add(self.root.capacity())?
            .checked_mul(4)
    }
}

/// Confined configuration observations, including paths hidden by discovery policy.
/// This is a capture primitive, not authority to compile or publish a unit.
#[derive(Clone, Debug)]
pub(super) struct ConfinedConfigurationCapture {
    root: ConfinedRootIdentity,
    probes: Vec<(String, Option<Vec<u8>>, Option<FileSystemRevision>)>,
}

impl ConfinedConfigurationCapture {
    pub(super) fn probes(&self) -> impl Iterator<Item = (&str, Option<&[u8]>)> {
        self.probes
            .iter()
            .map(|(path, bytes, _)| (path.as_str(), bytes.as_deref()))
    }

    /// Conservative retained heap charge for caller budget admission.
    pub(super) fn retained_bytes(&self) -> Option<usize> {
        let mut total = std::mem::size_of::<Self>()
            .checked_add(self.probes.capacity().checked_mul(std::mem::size_of::<(
                String,
                Option<Vec<u8>>,
                Option<FileSystemRevision>,
            )>())?)?
            .checked_add(self.root.retained_bytes()?)?;
        for (path, bytes, _) in &self.probes {
            total = total
                .checked_add(path.capacity())?
                .checked_add(64)?
                .checked_add(bytes.as_ref().map_or(0, Vec::capacity))?
                .checked_add(if bytes.is_some() { 64 } else { 0 })?;
        }
        total.checked_add(64)
    }

    fn revalidate_on_root(&self, root: &ProjectRoot) -> Result<bool, String> {
        for (path, bytes, revision) in &self.probes {
            let current = root.compiler_configuration_revision_or_absent(Path::new(path))?;
            if &current != revision {
                return Ok(false);
            }
            if let Some(expected) = bytes {
                let actual = root
                    .read_compiler_configuration(Path::new(path), expected.len())
                    .map_err(|_| format!("configuration became unreadable: {path}"))?;
                if &actual != expected
                    || root.compiler_configuration_revision_or_absent(Path::new(path))? != current
                {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Re-probes positive and negative paths against the original root fence.
    /// This remains an observation; it is not a unit permit or owner grant.
    pub(super) fn revalidate_revision_fence(
        &self,
        fence: &CompilerRevisionFence,
        cancellation: Option<&AtomicBool>,
    ) -> Result<bool, String> {
        if !self.root.matches_fence(fence)
            || !compiler_revision_is_current_with_cancellation(fence, cancellation)?
        {
            return Ok(false);
        }
        let root = fence.open_bound_root(&fence.requested_root)?;
        if !self.revalidate_on_root(&root)? {
            return Ok(false);
        }
        compiler_revision_is_current_with_cancellation(fence, cancellation)
    }

    /// A negative observation is re-probed independently of the ignore-filtered
    /// inventory; inaccessible paths are errors, never evidence of absence.
    pub(super) fn revalidate(&self, snapshot: &CompilerWorkspaceSnapshot) -> Result<bool, String> {
        if !self.root.matches_snapshot(snapshot)
            || !snapshot.revalidate()?
            || !self.revalidate_on_root(&snapshot.root_capability)?
        {
            return Ok(false);
        }
        snapshot.revalidate()
    }
}

/// One exact file read from the original confined workspace inventory.
/// This observation does not itself authorize unit selection.
#[derive(Clone, Debug)]
pub(super) struct ConfinedFileCapture {
    root: ConfinedRootIdentity,
    path: String,
    bytes: Vec<u8>,
    revision: FileSystemRevision,
}

impl ConfinedFileCapture {
    pub(super) fn path(&self) -> &str {
        &self.path
    }
    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(super) fn retained_bytes(&self) -> Option<usize> {
        std::mem::size_of::<Self>()
            .checked_add(self.root.retained_bytes()?)?
            .checked_add(self.path.capacity())?
            .checked_add(self.bytes.capacity())?.checked_add(128)
    }
    pub(super) fn revalidate_revision_fence(
        &self,
        fence: &CompilerRevisionFence,
        cancellation: Option<&AtomicBool>,
    ) -> Result<bool, String> {
        if !self.root.matches_fence(fence)
            || !compiler_revision_is_current_with_cancellation(fence, cancellation)?
        {
            return Ok(false);
        }
        let root = fence.open_bound_root(&fence.requested_root)?;
        if root.compiler_configuration_revision_or_absent(Path::new(&self.path))?
            != Some(self.revision)
        {
            return Ok(false);
        }
        let bytes = root
            .read_compiler_configuration(Path::new(&self.path), self.bytes.len())
            .map_err(|_| format!("captured file became unreadable: {}", self.path))?;
        if bytes != self.bytes
            || root.compiler_configuration_revision_or_absent(Path::new(&self.path))?
                != Some(self.revision)
        {
            return Ok(false);
        }
        compiler_revision_is_current_with_cancellation(fence, cancellation)
    }
}

impl CompilerWorkspaceSnapshot {
    fn root_binding_is_current(&self) -> bool {
        let requested_resolves_to_root =
            self.requested_root.canonicalize().ok().as_deref() == Some(self.root.as_path());
        let expected_revision = self
            .entries
            .iter()
            .find(|entry| entry.path.is_empty())
            .map(|entry| entry.revision);
        let opened_revision = ProjectRoot::open(&self.root)
            .and_then(|root| root.revision())
            .ok();
        requested_resolves_to_root
            && expected_revision.is_some()
            && opened_revision == expected_revision
    }

    /// Opens and inventories every path admitted by the versioned workspace
    /// policy. The root itself is represented by an empty relative path.
    pub(super) fn open(root: &Path) -> Result<Self, String> {
        Self::open_with_cancellation(root, None)
    }

    pub(super) fn open_with_cancellation(
        root: &Path,
        cancellation: Option<&AtomicBool>,
    ) -> Result<Self, String> {
        check_scan_cancellation(cancellation)?;
        let requested_root = absolute_path_spelling(root)?;
        #[cfg(not(windows))]
        {
            let requested_metadata = fs::symlink_metadata(&requested_root).map_err(|error| {
                format!(
                    "stat compiler workspace {}: {error}",
                    requested_root.display()
                )
            })?;
            if requested_metadata.file_type().is_symlink() {
                return Err("compiler workspace root cannot be a symlink".to_owned());
            }
        }
        #[cfg(windows)]
        let (root, root_capability) = open_canonical_project_root(&requested_root)?;
        #[cfg(not(windows))]
        let root = requested_root.canonicalize().map_err(|error| {
            format!(
                "open compiler workspace {}: {error}",
                requested_root.display()
            )
        })?;
        #[cfg(not(windows))]
        if !root.is_dir() {
            return Err(format!(
                "compiler workspace {} is not a directory",
                root.display()
            ));
        }
        let policy = source_selection_policy();
        #[cfg(not(windows))]
        let root_capability = ProjectRoot::open(&root)?;
        let entries =
            capture_compiler_workspace_entries(&root, &policy, &root_capability, cancellation)?;
        check_scan_cancellation(cancellation)?;
        if entries.first().map(|entry| entry.revision) != Some(root_capability.revision()?) {
            return Err("compiler workspace root changed during inventory capture".to_owned());
        }
        #[cfg(windows)]
        if open_canonical_project_root(&root)?.1.revision()? != root_capability.revision()? {
            return Err("compiler workspace root changed during inventory capture".to_owned());
        }
        let fence_digest =
            compiler_workspace_fence_digest(&entries, COMPILER_WORKSPACE_POLICY_IDENTITY);
        Ok(Self {
            requested_root,
            root,
            root_capability,
            policy,
            entries,
            fence_digest,
        })
    }

    /// Capture explicit confined paths independently of ignore rules. The caller
    /// supplies the canonical ordered path set and must bind it to a unit permit.
    pub(super) fn capture_configuration_paths(
        &self,
        paths: &[String],
        maximum_total: usize,
    ) -> Result<ConfinedConfigurationCapture, String> {
        if paths.is_empty() || paths.len() > 17 || maximum_total > 2 * 1024 * 1024 {
            return Err("configuration capture exceeds its bound".to_owned());
        }
        // The ordered set is part of the selected project identity. Do not let
        // a caller omit an ancestor or substitute another project’s probes.
        let project = &paths[0];
        if project.len() > 4096
            || project.contains(['\\', ':'])
            || project.bytes().any(|byte| byte.is_ascii_control())
            || project.starts_with('/')
            || !project.to_ascii_lowercase().ends_with(".csproj")
            || project
                .split('/')
                .any(|part| part.is_empty() || part == "." || part == "..")
        {
            return Err("invalid C# project path".to_owned());
        }
        let mut expected = vec![project.clone()];
        let mut directory = project.rsplit_once('/').map_or("", |(parent, _)| parent);
        loop {
            let prefix = if directory.is_empty() {
                String::new()
            } else {
                format!("{directory}/")
            };
            expected.push(format!("{prefix}Directory.Build.props"));
            expected.push(format!("{prefix}Directory.Build.targets"));
            if directory.is_empty() {
                break;
            }
            if expected.len() >= 17 {
                return Err("C# project path is too deep".to_owned());
            }
            directory = directory.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
        if paths != expected
            || paths
                .iter()
                .try_fold(0usize, |sum, path| sum.checked_add(path.len()))
                .is_none_or(|sum| sum > 17 * 4096)
        {
            return Err("configuration probes are not the ordered project set".to_owned());
        }
        self.capture_bounded_configuration_probes(paths, maximum_total, true)
    }

    /// Captures SDK global.json probes from the selected project directory to
    /// the confined package root. Selection outside that root remains unknown.
    pub(super) fn capture_csharp_global_json_paths(
        &self, project_path: &str,
    ) -> Result<ConfinedConfigurationCapture, String> {
        if project_path.is_empty() || project_path.len() > 4096
            || project_path.contains(['\\', ':']) || project_path.chars().any(char::is_control)
            || project_path.nfc().collect::<String>() != project_path
            || !project_path.to_ascii_lowercase().ends_with(".csproj")
            || project_path.split('/').any(|part| part.is_empty() || part == "." || part == "..")
        { return Err("invalid C# project path".to_owned()); }
        let mut directory = project_path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let mut paths = Vec::new();
        loop {
            paths.push(if directory.is_empty() { "global.json".to_owned() } else { format!("{directory}/global.json") });
            if directory.is_empty() { break; }
            if paths.len() >= 8 { return Err("C# project path is too deep".to_owned()); }
            directory = directory.rsplit_once('/').map_or("", |(parent, _)| parent);
        }
        let path_bytes = paths.iter().try_fold(0usize, |total, path| total.checked_add(path.len())).ok_or("SDK probe bound exceeded")?;
        let payload_limit = (1024usize * 1024).checked_sub(path_bytes).ok_or("SDK probe bound exceeded")?;
        self.capture_bounded_configuration_probes(&paths, payload_limit, false)
    }

    fn capture_bounded_configuration_probes(
        &self, paths: &[String], maximum_total: usize, first_required: bool,
    ) -> Result<ConfinedConfigurationCapture, String> {
        if !self.root_binding_is_current() {
            return Err("workspace root changed before configuration capture".to_owned());
        }
        let mut seen = BTreeSet::new();
        let mut total = 0usize;
        let mut probes = Vec::with_capacity(paths.len());
        for path in paths {
            if !seen.insert(path) {
                return Err("duplicate configuration probe".to_owned());
            }
            let relative = Path::new(path);
            let revision = self
                .root_capability
                .compiler_configuration_revision_or_absent(relative)?;
            let bytes = match revision {
                Some(_) => {
                    let remaining = maximum_total
                        .checked_sub(total)
                        .ok_or("configuration bound exceeded")?;
                    let bytes = self
                        .root_capability
                        .read_compiler_configuration(relative, remaining)
                        .map_err(|_| format!("configuration is not safely readable: {path}"))?;
                    total = total
                        .checked_add(bytes.len())
                        .ok_or("configuration bound exceeded")?;
                    if self
                        .root_capability
                        .compiler_configuration_revision_or_absent(relative)?
                        != revision
                    {
                        return Err(format!("configuration changed during capture: {path}"));
                    }
                    Some(bytes)
                }
                None => None,
            };
            probes.push((path.clone(), bytes, revision));
        }
        if first_required && probes.first().is_none_or(|(_, bytes, _)| bytes.is_none()) {
            return Err("selected C# project is absent".to_owned());
        }
        let capture = ConfinedConfigurationCapture {
            root: ConfinedRootIdentity::from_snapshot(self)?,
            probes,
        };
        if !capture.revalidate_on_root(&self.root_capability)? || !self.root_binding_is_current() {
            return Err("configuration changed during capture".to_owned());
        }
        Ok(capture)
    }

    /// Capture one observed file and its same-inventory revision, bounded by caller policy.
    pub(super) fn capture_confined_file(
        &self,
        path: &str,
        maximum_bytes: usize,
    ) -> Result<ConfinedFileCapture, String> {
        let position = self
            .entries
            .binary_search_by(|entry| entry.path.as_str().cmp(path))
            .map_err(|_| "file is not in the captured workspace".to_owned())?;
        let entry = &self.entries[position];
        if entry.kind != CompilerWorkspaceEntryKind::File {
            return Err("captured path is not a file".to_owned());
        }
        let bytes = self.read_file(path, maximum_bytes)?;
        let capture = ConfinedFileCapture {
            root: ConfinedRootIdentity::from_snapshot(self)?,
            path: path.to_owned(),
            bytes,
            revision: entry.revision,
        };
        if !self.root_binding_is_current() {
            return Err("workspace root changed during file capture".to_owned());
        }
        Ok(capture)
    }

    /// Stable versioned identity for the exact ignore and generated-folder
    /// rules used to construct this snapshot.
    #[must_use]
    pub(super) const fn policy_identity(&self) -> &'static str {
        COMPILER_WORKSPACE_POLICY_IDENTITY
    }

    /// Returns the complete sorted inventory, including the root entry.
    #[must_use]
    pub(super) fn entries(&self) -> &[CompilerWorkspaceEntry] {
        &self.entries
    }

    /// Absolute spelling supplied for this workspace. Durable compiler work
    /// retains it so later recovery can detect if an alias resolves elsewhere;
    /// filesystem reads still use the canonical root capability.
    #[must_use]
    pub(super) fn root_path(&self) -> &Path {
        &self.requested_root
    }

    /// Digest pairing compiler authority evidence with this captured complete
    /// filesystem fence. Callers must still use [`Self::revalidate`] before
    /// selecting outputs.
    #[must_use]
    pub(super) const fn fence_digest(&self) -> [u8; 32] {
        self.fence_digest
    }

    /// Reads one captured regular file through the root capability, bounded by
    /// both the caller's limit and the captured length.
    pub(super) fn read_file(&self, relative: &str, max_bytes: usize) -> Result<Vec<u8>, String> {
        if max_bytes > MAX_COMPILER_WORKSPACE_FILE_BYTES {
            return Err("workspace read bound exceeds the per-file limit".to_owned());
        }
        let expected_length = self.captured_file_length(relative)?;
        let expected_length_usize = usize::try_from(expected_length)
            .map_err(|_| "workspace file length exceeds this target".to_owned())?;
        if expected_length_usize > max_bytes {
            return Err("workspace file exceeds the requested read bound".to_owned());
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(expected_length_usize)
            .map_err(|_| "workspace read allocation exceeds memory".to_owned())?;
        let _ = self.stream_file(relative, max_bytes as u64, 64 * 1024, |chunk| {
            bytes.extend_from_slice(chunk);
            Ok(())
        })?;
        Ok(bytes)
    }

    /// Streams a captured regular file using bounded memory while pinning the
    /// opened file identity through EOF. Consumers should stage side effects
    /// until this method returns successfully because a mutation can be found
    /// after earlier chunks have already been delivered.
    pub(super) fn stream_file(
        &self,
        relative: &str,
        max_bytes: u64,
        chunk_bytes: usize,
        mut consume: impl FnMut(&[u8]) -> Result<(), String>,
    ) -> Result<u64, String> {
        if max_bytes > MAX_COMPILER_WORKSPACE_STREAM_BYTES {
            return Err("workspace stream bound exceeds the per-file limit".to_owned());
        }
        if chunk_bytes == 0 || chunk_bytes > MAX_COMPILER_WORKSPACE_CHUNK_BYTES {
            return Err("workspace stream chunk size is outside the allowed bound".to_owned());
        }
        let expected_length = self.captured_file_length(relative)?;
        if expected_length > max_bytes {
            return Err("workspace file exceeds the requested stream bound".to_owned());
        }
        let entry = &self.entries[self
            .entries
            .binary_search_by(|entry| entry.path.as_str().cmp(relative))
            .map_err(|_| "workspace path is not in the captured inventory".to_owned())?];
        let mut file = self
            .root_capability
            .open_workspace_file(Path::new(relative))?;
        if file_system_revision_from_open_file(&file)? != entry.revision {
            return Err("workspace file changed after inventory capture".to_owned());
        }
        let mut buffer = Vec::new();
        buffer
            .try_reserve_exact(chunk_bytes)
            .map_err(|_| "workspace stream buffer allocation failed".to_owned())?;
        buffer.resize(chunk_bytes, 0);
        let mut total = 0_u64;
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| format!("read workspace file: {error}"))?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(read).map_err(|_| "workspace read overflow".to_owned())?)
                .ok_or_else(|| "workspace read length overflow".to_owned())?;
            if total > expected_length || total > max_bytes {
                return Err("workspace file grew beyond its captured length".to_owned());
            }
            consume(&buffer[..read])?;
        }
        if total != expected_length || file_system_revision_from_open_file(&file)? != entry.revision
        {
            return Err("workspace file changed while it was streamed".to_owned());
        }
        Ok(total)
    }

    fn captured_file_length(&self, relative: &str) -> Result<u64, String> {
        let position = self
            .entries
            .binary_search_by(|entry| entry.path.as_str().cmp(relative))
            .map_err(|_| "workspace path is not in the captured inventory".to_owned())?;
        let entry = &self.entries[position];
        if entry.kind != CompilerWorkspaceEntryKind::File {
            return Err("workspace path does not name a regular file".to_owned());
        }
        entry
            .byte_length
            .ok_or_else(|| "workspace file has no captured length".to_owned())
    }

    /// Checks the exact complete path, kind, and filesystem-revision fence.
    /// `Ok(false)` means the admitted workspace changed; traversal failures
    /// remain errors so callers cannot mistake an incomplete scan for fresh.
    pub(super) fn revalidate(&self) -> Result<bool, String> {
        self.revalidate_with_cancellation(None)
    }

    pub(super) fn revalidate_with_cancellation(
        &self,
        cancellation: Option<&AtomicBool>,
    ) -> Result<bool, String> {
        check_scan_cancellation(cancellation)?;
        if !self.root_binding_is_current() {
            return Ok(false);
        }
        #[cfg(windows)]
        if open_canonical_project_root(&self.root)?.1.revision()?
            != self.root_capability.revision()?
        {
            return Ok(false);
        }
        let current = capture_compiler_workspace_entries(
            &self.root,
            &self.policy,
            &self.root_capability,
            cancellation,
        )?;
        #[cfg(windows)]
        if open_canonical_project_root(&self.root)?.1.revision()?
            != self.root_capability.revision()?
        {
            return Ok(false);
        }
        Ok(current == self.entries
            && compiler_workspace_fence_digest(&current, self.policy_identity())
                == self.fence_digest
            && current.first().map(|entry| entry.revision)
                == Some(self.root_capability.revision()?)
            && self.root_binding_is_current())
    }
}

fn compiler_workspace_fence_digest(
    entries: &[CompilerWorkspaceEntry],
    policy_identity: &str,
) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    #[cfg(windows)]
    hasher.update(b"backend.compiler-workspace-fence.v2\0");
    #[cfg(not(windows))]
    hasher.update(b"backend.compiler-workspace-fence.v1\0");
    hasher.update(policy_identity.as_bytes());
    hasher.update(&[0]);
    for entry in entries {
        hasher.update(
            &u64::try_from(entry.path.len())
                .unwrap_or(u64::MAX)
                .to_be_bytes(),
        );
        hasher.update(entry.path.as_bytes());
        hasher.update(&[match entry.kind {
            CompilerWorkspaceEntryKind::Directory => 0,
            CompilerWorkspaceEntryKind::File => 1,
        }]);
        match entry.byte_length {
            Some(length) => {
                hasher.update(&[1]);
                hasher.update(&length.to_be_bytes());
            }
            None => {
                hasher.update(&[0]);
            }
        };
        hasher.update(&entry.revision.length.to_be_bytes());
        match entry.revision.modified {
            Some(modified) => {
                hasher.update(&[1]);
                match modified.duration_since(UNIX_EPOCH) {
                    Ok(duration) => {
                        hasher.update(&[0]);
                        hasher.update(&duration.as_nanos().to_be_bytes());
                    }
                    Err(error) => {
                        hasher.update(&[1]);
                        hasher.update(&error.duration().as_nanos().to_be_bytes());
                    }
                }
            }
            None => {
                hasher.update(&[0]);
            }
        };
        #[cfg(unix)]
        {
            for value in [
                entry.revision.unix.0,
                entry.revision.unix.1,
                entry.revision.unix.2 as u64,
                entry.revision.unix.3 as u64,
            ] {
                hasher.update(&value.to_be_bytes());
            }
        }
        #[cfg(windows)]
        {
            hasher.update(&entry.revision.windows.volume_serial_number.to_be_bytes());
            hasher.update(&entry.revision.windows.file_id);
            hasher.update(&entry.revision.windows.change_time.to_be_bytes());
            hasher.update(&entry.revision.windows.last_write_time.to_be_bytes());
            hasher.update(&[u8::from(entry.revision.windows.is_directory)]);
            hasher.update(&entry.revision.windows.number_of_links.to_be_bytes());
        }
    }
    *hasher.finalize().as_bytes()
}

fn capture_compiler_workspace_entries(
    root: &Path,
    policy: &DiscoveryPolicy,
    _root_capability: &ProjectRoot,
    cancellation: Option<&AtomicBool>,
) -> Result<Vec<CompilerWorkspaceEntry>, String> {
    let mut entries = Vec::new();
    #[cfg(windows)]
    let root_revision = _root_capability.revision()?;
    #[cfg(not(windows))]
    let root_revision = file_system_revision(root)
        .ok_or_else(|| "compiler workspace root metadata is unavailable".to_owned())?;
    entries.push(CompilerWorkspaceEntry {
        path: String::new(),
        kind: CompilerWorkspaceEntryKind::Directory,
        byte_length: None,
        revision: root_revision,
    });
    let mut budget = DiscoveryBudget::default();
    let mut directory_widths = BTreeMap::<String, usize>::new();
    let mut case_keys = BTreeSet::new();
    let mut canonical_paths = BTreeSet::new();
    for result in policy.clone().walk_workspace_entries(root) {
        check_scan_cancellation(cancellation)?;
        let discovered = result.map_err(|error| error.to_string())?;
        if discovered.path() == root {
            continue;
        }
        let relative = discovered
            .path()
            .strip_prefix(root)
            .map_err(|_| "workspace entry escaped its root".to_owned())?;
        let relative = normalized_workspace_path(relative)?;
        let parent = relative.rsplit_once('/').map_or("", |(parent, _)| parent);
        let width = directory_widths.entry(parent.to_owned()).or_default();
        budget.admit_entry(*width)?;
        *width = width.saturating_add(1);
        if !case_keys.insert(relative.to_lowercase()) {
            return Err(format!(
                "workspace contains a case-colliding path: {relative}"
            ));
        }
        let path = root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
        let canonical = path
            .canonicalize()
            .map_err(|error| format!("resolve workspace entry {}: {error}", path.display()))?;
        if canonical != path || !canonical.starts_with(root) || !canonical_paths.insert(canonical) {
            return Err(format!("workspace contains a path alias: {relative}"));
        }
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| format!("stat workspace entry {}: {error}", path.display()))?;
        let kind = match discovered.kind() {
            EntryKind::Directory if metadata.is_dir() => CompilerWorkspaceEntryKind::Directory,
            EntryKind::File if metadata.is_file() => CompilerWorkspaceEntryKind::File,
            EntryKind::Symlink => {
                return Err(format!("workspace contains a symlink: {relative}"));
            }
            EntryKind::Other => {
                return Err(format!(
                    "workspace contains a special filesystem entry: {relative}"
                ));
            }
            _ => {
                return Err(format!(
                    "workspace entry kind changed during discovery: {relative}"
                ));
            }
        };
        #[cfg(windows)]
        let revision = _root_capability.revision_relative(Path::new(&relative))?;
        #[cfg(not(windows))]
        let revision = file_system_revision(&path)
            .ok_or_else(|| format!("workspace metadata is unavailable: {relative}"))?;
        #[cfg(windows)]
        if revision.windows.is_directory != (kind == CompilerWorkspaceEntryKind::Directory) {
            return Err(format!(
                "workspace entry kind changed during capture: {relative}"
            ));
        }
        let byte_length = match kind {
            CompilerWorkspaceEntryKind::Directory => None,
            CompilerWorkspaceEntryKind::File => Some(revision.length),
        };
        entries.push(CompilerWorkspaceEntry {
            path: relative,
            kind,
            byte_length,
            revision,
        });
    }
    check_scan_cancellation(cancellation)?;
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(entries)
}

fn normalized_workspace_path(relative: &Path) -> Result<String, String> {
    let mut components = Vec::new();
    for component in relative.components() {
        let std::path::Component::Normal(component) = component else {
            return Err("workspace path contains a non-normal component".to_owned());
        };
        let component = component
            .to_str()
            .ok_or_else(|| "workspace path is not valid UTF-8".to_owned())?;
        if component.is_empty()
            || component.contains(['/', '\\'])
            || component.chars().any(char::is_control)
            || component.nfc().collect::<String>() != component
        {
            return Err(format!("workspace path is not normalized: {component:?}"));
        }
        components.push(component);
    }
    if components.is_empty() {
        return Err("workspace root path must use the empty relative path".to_owned());
    }
    Ok(components.join("/"))
}

/// Reads one already-opened regular file within the per-file byte bound.
///
/// Every failure here is a fact about the file rather than the project, so it
/// is classified rather than propagated as a scan error.
fn read_bounded(mut file: fs::File, maximum_bytes: usize) -> Result<Vec<u8>, SourceFault> {
    let metadata = file
        .metadata()
        .map_err(|_| SourceFault::Unavailable(SourceUnavailableReason::Unreadable))?;
    if !metadata.is_file() {
        return Err(SourceFault::Vanished);
    }
    let capacity = usize::try_from(metadata.len())
        .ok()
        .filter(|length| *length <= maximum_bytes)
        .ok_or(SourceFault::Unavailable(SourceUnavailableReason::TooLarge))?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(capacity)
        .map_err(|_| SourceFault::Fatal("source allocation exceeds memory".to_owned()))?;
    let bound = u64::try_from(maximum_bytes)
        .map_err(|_| SourceFault::Fatal("input bound exceeds this target".to_owned()))?;
    if file
        .by_ref()
        .take(bound.saturating_add(1))
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Err(SourceFault::UnavailableRead(
            SourceUnavailableReason::Unreadable,
            bytes.len(),
        ));
    }
    if bytes.len() > maximum_bytes {
        return Err(SourceFault::UnavailableRead(
            SourceUnavailableReason::TooLarge,
            bytes.len(),
        ));
    }
    Ok(bytes)
}

/// What one file's failure means for the project scan.
///
/// Only a failure that invalidates the whole scan may stop it.  A file that
/// cannot be read, decoded, or parsed is a fact about that file: it keeps its
/// place in the project frontier with a typed reason, and every other file
/// still indexes.  Before this split, one unreadable byte anywhere made a
/// large checkout permanently unindexable.
#[derive(Clone, Debug, Eq, PartialEq)]
enum SourceFault {
    /// The file disappeared between discovery and read; drop it silently
    /// because the project no longer contains it.
    Vanished,
    /// The file is present but nothing could be extracted from it.
    Unavailable(SourceUnavailableReason),
    /// The file was read, but its bytes could not be extracted. Retain the
    /// number of bytes actually returned by the bounded read for measurement.
    UnavailableRead(SourceUnavailableReason, usize),
    /// The scan itself cannot continue.
    Fatal(String),
}

impl SourceFault {
    fn from_open(kind: std::io::ErrorKind) -> Self {
        match kind {
            std::io::ErrorKind::NotFound => Self::Vanished,
            _ => Self::Unavailable(SourceUnavailableReason::Unreadable),
        }
    }
}

#[derive(Default)]
struct DiscoveryBudget {
    entries: usize,
}

impl DiscoveryBudget {
    fn admit_entry(&mut self, directory_entries: usize) -> Result<(), String> {
        if directory_entries >= MAX_DIRECTORY_ENTRIES {
            return Err("directory exceeds the bounded discovery width".to_owned());
        }
        self.entries = self
            .entries
            .checked_add(1)
            .ok_or_else(|| "project discovery entry count overflow".to_owned())?;
        if self.entries > MAX_DISCOVERY_ENTRIES {
            return Err("project contains too many filesystem entries".to_owned());
        }
        Ok(())
    }
}

struct ProductFrontend {
    baseline: SyntaxFrontend,
    producer_version: [u8; 32],
}

struct FrontendSet {
    values: [ProductFrontend; 7],
}

// Readiness must never construct a syntax parser. Legacy indexing may fill
// this registry; capability reporting only observes what was already admitted.
static FRONTENDS: OnceLock<FrontendSet> = OnceLock::new();

impl FrontendSet {
    fn build() -> Result<Self, String> {
        #[cfg(test)]
        if TEST_REJECT_STRUCTURAL_FRONTENDS.load(Ordering::Acquire) {
            return Err("test refused structural frontend construction".to_owned());
        }
        let baselines = [
            backend_frontend_rust::syntax_frontend(),
            backend_frontend_python::syntax_frontend(),
            backend_frontend_typescript::syntax_frontend(),
            backend_frontend_go::syntax_frontend(),
            backend_frontend_java::syntax_frontend(),
            backend_frontend_csharp::syntax_frontend(),
            backend_frontend_clang::syntax_frontend(),
        ]
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
        let values = baselines
            .into_iter()
            .map(|baseline| {
                let producer_version = producer_version(&baseline);
                ProductFrontend {
                    baseline,
                    producer_version,
                }
            })
            .collect::<Vec<_>>()
            .try_into()
            .map_err(|_| "frontend registry has the wrong cardinality".to_owned())?;
        Ok(Self { values })
    }

    fn for_path(&self, path: &Path) -> Option<&ProductFrontend> {
        self.values
            .iter()
            .find(|frontend| frontend.baseline.supports_path(path))
    }

    fn for_language(&self, language: SourceLanguage) -> Option<&ProductFrontend> {
        self.values
            .iter()
            .find(|frontend| frontend.language() == language)
    }
}

impl ProductFrontend {
    fn language(&self) -> SourceLanguage {
        self.baseline.language()
    }

    const fn analysis_version(&self) -> [u8; 32] {
        self.producer_version
    }
}

pub(super) fn semantic_capabilities(
    baseline: &backend_engine::CapabilityInventory,
    compiler: &backend_engine::application::LocalCompilerClient,
) -> Result<backend_engine::CapabilityInventory, String> {
    let frontends = FRONTENDS.get();
    let target = backend_engine::CapabilityTarget::Native {
        os: native_os(),
        architecture: native_architecture(),
    };
    let mut rows = baseline
        .as_slice()
        .iter()
        .map(|status| match status.family() {
            backend_engine::CapabilityFamily::LanguageOracle { profile, task } => {
                let compiler_capability = compiler.capabilities().for_profile(profile);
                return match compiler_capability.state() {
                    backend_engine::application::LocalCompilerCapabilityState::Deferred => {
                        backend_engine::CapabilityStatus::unavailable(
                            status.id(),
                            status.family(),
                            backend_engine::CapabilityUnavailable::NoManifest,
                        )
                    }
                    backend_engine::application::LocalCompilerCapabilityState::Probing => {
                        backend_engine::CapabilityStatus::probing(status.id(), status.family())
                    }
                    backend_engine::application::LocalCompilerCapabilityState::ProbeFailed => {
                        backend_engine::CapabilityStatus::unavailable(
                            status.id(),
                            status.family(),
                            backend_engine::CapabilityUnavailable::ProbeFailed,
                        )
                    }
                    backend_engine::application::LocalCompilerCapabilityState::Unavailable => {
                        *status
                    }
                    backend_engine::application::LocalCompilerCapabilityState::Ready => {
                        let Some(manifest) = compiler_capability.manifest() else {
                            return *status;
                        };
                        let (Some(toolchain_identity), Some(package_authority)) = (
                            compiler_capability.toolchain_identity(),
                            compiler_capability.local_authority_fingerprint(),
                        ) else {
                            return *status;
                        };
                        let package_authority =
                            backend_engine::PackageAuthorityIdentity::LocalConfiguration(
                                package_authority,
                            );
                        let toolchain = compiler_capability.toolchain();
                        let protocol_abi = backend_compile::NATIVE_PAYLOAD_VERSION;
                        backend_engine::CapabilityStatus::observed(
                            status.id(),
                            status.family(),
                            manifest,
                            target,
                            protocol_abi,
                            backend_engine::CapabilityAuthority::Compiler {
                                recipe: backend_engine::compiler_authority_recipe(
                                    manifest,
                                    profile,
                                    task,
                                    protocol_abi,
                                    toolchain,
                                    toolchain_identity,
                                    package_authority,
                                ),
                                toolchain,
                                toolchain_identity,
                                package_authority,
                            },
                            backend_engine::CapabilityLifecycle::Active,
                        )
                    }
                };
            }
            backend_engine::CapabilityFamily::StructuralFrontend { profile } => {
                let manifest = frontends
                    .and_then(|frontends| frontends.for_language(profile.language()))
                    .map(ProductFrontend::analysis_version);
                structural_capability_from_observation(*status, manifest, target)
            }
            backend_engine::CapabilityFamily::Embedding { .. } => *status,
        })
        .collect::<Vec<_>>();
    rows.sort_unstable_by_key(backend_engine::CapabilityStatus::id);
    backend_engine::CapabilityInventory::try_new(rows).map_err(|error| error.to_string())
}

/// Preserve the admitted baseline when no structural registry was observed.
fn structural_capability_from_observation(
    baseline: backend_engine::CapabilityStatus,
    manifest: Option<[u8; 32]>,
    target: backend_engine::CapabilityTarget,
) -> backend_engine::CapabilityStatus {
    let Some(manifest) = manifest else { return baseline };
    backend_engine::CapabilityStatus::observed(
        baseline.id(), baseline.family(), manifest, target,
        backend_compile::NATIVE_PAYLOAD_VERSION,
        backend_engine::CapabilityAuthority::Structural { producer: manifest },
        backend_engine::CapabilityLifecycle::Ready,
    )
}

const fn native_os() -> u8 {
    if cfg!(target_os = "macos") {
        1
    } else if cfg!(target_os = "linux") {
        2
    } else if cfg!(target_os = "windows") {
        3
    } else {
        0
    }
}

const fn native_architecture() -> u8 {
    if cfg!(target_arch = "aarch64") {
        1
    } else if cfg!(target_arch = "x86_64") {
        2
    } else {
        0
    }
}

fn producer_version(frontend: &SyntaxFrontend) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend.product-structural-baseline.v1\0");
    hasher.update(&frontend.producer().to_bytes());
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
#[derive(Clone, Debug)]
pub(super) struct TestNativeProgramObservation {
    pub binding: [u8; 32],
    pub membership: [u8; 32],
    pub sources: Vec<(String, [u8; 32], u32)>,
}
#[cfg(test)]
pub(super) static TEST_CAPTURE_NATIVE_PROGRAMS: AtomicBool = AtomicBool::new(false);
#[cfg(test)]
pub(super) static TEST_NATIVE_PROGRAMS: std::sync::Mutex<Vec<TestNativeProgramObservation>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(super) fn test_observe_native_program(binding: [u8; 32], program: &backend_semantic::ir::NativeProgramSources) -> Result<(), String> {
    if TEST_CAPTURE_NATIVE_PROGRAMS.load(Ordering::Acquire) {
        let observation = TestNativeProgramObservation {
            binding,
            membership: program.membership_identity(),
            sources: program.sources().map(|row| (
                row.program_path.to_owned(), *row.source.identity.as_ref(), row.source.byte_len,
            )).collect(),
        };
        TEST_NATIVE_PROGRAMS.lock().map_err(|_| "test native Program observation poisoned")?.push(observation);
    }
    Ok(())
}

#[cfg(test)]
pub(super) static TEST_REJECT_STRUCTURAL_FRONTENDS: AtomicBool = AtomicBool::new(false);

fn frontends() -> Result<&'static FrontendSet, String> {
    #[cfg(test)]
    if TEST_REJECT_STRUCTURAL_FRONTENDS.load(Ordering::Acquire) {
        return Err("test refused structural frontend construction".to_owned());
    }
    if let Some(frontends) = FRONTENDS.get() {
        return Ok(frontends);
    }
    let candidate = FrontendSet::build()?;
    let _ = FRONTENDS.set(candidate);
    FRONTENDS
        .get()
        .ok_or_else(|| "frontend registry failed to initialize".to_owned())
}

fn scan_source_paths(
    root: &Path,
    root_capability: &ProjectRoot,
    paths: &[PathBuf],
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
    python_version: PythonVersion,
    go_capture: &GoProjectCapture,
    delta: Option<&source_frontier::SourceDelta>,
    source_policy: SourceAdmissionPolicy,
    budget: &mut SourceAdmissionLedger,
    cancellation: Option<&AtomicBool>,
    observation: Option<&crate::builtin::index_work_observation::IndexWorkObservation>,
) -> Result<Vec<ScannedFile>, String> {
    let workers = source_policy.worker_count(
        thread::available_parallelism().map_or(1, usize::from),
        paths.len(),
    );
    // The stop flag is owned by the caller of the scoped workers, so its
    // borrow outlives every join without shared ownership or a mutex.
    let stop = AtomicBool::new(false);
    thread::scope(|scope| {
        let queue = workers
            .checked_mul(RESULT_QUEUE_PER_WORKER)
            .ok_or_else(|| "source result queue width overflow".to_owned())?;
        let (sender, receiver) = mpsc::sync_channel(queue);
        let mut handles = Vec::with_capacity(workers);
        let mut acknowledgements = Vec::with_capacity(workers);
        // Stride adjacent path positions across workers. Contiguous chunks
        // plus ordered acknowledgements would leave all but the first chunk
        // idle; this bounded window permits each worker to progress while
        // retaining at most one unacknowledged result per worker.
        for worker_index in 0..workers {
            let root = root;
            let root_capability = root_capability;
            let sender = sender.clone();
            let cancellation = cancellation;
            let stop = &stop;
            let (acknowledge, acknowledged) = mpsc::sync_channel::<bool>(1);
            acknowledgements.push(acknowledge);
            handles.push(scope.spawn(move || {
                for (path_index, path) in
                    paths.iter().enumerate().skip(worker_index).step_by(workers)
                {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    if let Some(observed) = observation {
                        observed.source_started(
                            worker_index,
                            path_index,
                            path.strip_prefix(root).unwrap_or(path),
                        );
                    }
                    let result = catch_unwind(AssertUnwindSafe(|| {
                        scan_source_path(
                            root,
                            root_capability,
                            path,
                            project,
                            reusable,
                            frontends,
                            python_version,
                            go_capture,
                            delta,
                            source_policy,
                            cancellation,
                        )
                    }))
                    .unwrap_or_else(|_| Err("source analysis worker panicked".to_owned()));
                    if let Some(observed) = observation {
                        observed.source_finished(worker_index);
                    }
                    let failed = result.is_err();
                    if sender.send((path_index, worker_index, result)).is_err() {
                        break;
                    }
                    let proceed = acknowledged.recv().unwrap_or(false);
                    if let Some(observed) = observation {
                        observed.source_released(worker_index);
                    }
                    if !proceed || failed {
                        break;
                    }
                }
            }));
        }
        drop(sender);
        let mut output =
            Vec::with_capacity(paths.len().min(source_policy.limits().max_project_records));
        let mut failure = None;
        let mut pending = BTreeMap::<usize, (usize, Result<Option<ScannedFile>, String>)>::new();
        let mut next_path_index = 0_usize;
        loop {
            let (path_index, worker_index, result) = match if observation.is_some() {
                receiver.recv_timeout(std::time::Duration::from_millis(250))
            } else {
                receiver
                    .recv()
                    .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
            } {
                Ok(result) => result,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    if let Some(observed) = observation {
                        observed.flush(false);
                    }
                    continue;
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            if let Some(observed) = observation {
                observed.flush(false);
            }
            if failure.is_some() {
                let _ = acknowledgements[worker_index].send(false);
                continue;
            }
            if let Err(error) = check_scan_cancellation(cancellation) {
                failure = Some(error);
                stop.store(true, Ordering::Release);
                let _ = acknowledgements[worker_index].send(false);
                for (_, (pending_worker, _)) in std::mem::take(&mut pending) {
                    let _ = acknowledgements[pending_worker].send(false);
                }
                continue;
            }
            pending.insert(path_index, (worker_index, result));
            loop {
                let Some((pending_worker, result)) = pending.remove(&next_path_index) else {
                    break;
                };
                // Cancellation may arrive while an earlier path is admitted.
                // Poll each result already held in the ordered window too.
                match check_scan_cancellation(cancellation).and_then(|()| result) {
                    Ok(Some(file)) => {
                        let charge = source_policy.admit_actual_file(file.source_bytes).and_then(
                            |admitted| {
                                budget.admit_with_fact_pages(
                                    source_policy,
                                    admitted,
                                    file.encoded_record_bytes,
                                    file.encoded_fact_bytes,
                                    false,
                                )
                            },
                        );
                        match charge {
                            Ok(()) => output.push(file),
                            Err(refusal) => failure = Some(refusal.to_string()),
                        }
                    }
                    Ok(None) => {}
                    Err(error) => failure = Some(error),
                }
                if failure.is_some() {
                    stop.store(true, Ordering::Release);
                    let _ = acknowledgements[pending_worker].send(false);
                    for (_, (waiting_worker, _)) in std::mem::take(&mut pending) {
                        let _ = acknowledgements[waiting_worker].send(false);
                    }
                    break;
                }
                if acknowledgements[pending_worker].send(true).is_err() {
                    failure =
                        Some("source analysis worker stopped before acknowledgement".to_owned());
                    stop.store(true, Ordering::Release);
                    for (_, (waiting_worker, _)) in std::mem::take(&mut pending) {
                        let _ = acknowledgements[waiting_worker].send(false);
                    }
                    break;
                }
                if let Some(observed) = observation {
                    observed.source_acknowledged();
                }
                next_path_index = next_path_index
                    .checked_add(1)
                    .ok_or_else(|| "source path index overflow".to_owned())?;
            }
        }
        for handle in handles {
            if handle.join().is_err() && failure.is_none() {
                failure = Some("source analysis worker panicked".to_owned());
            }
        }
        if let Some(observed) = observation {
            observed.flush(true);
        }
        if failure.is_none() && next_path_index != paths.len() {
            failure = Some("source analysis workers did not account for every path".to_owned());
        }
        if let Some(error) = failure {
            return Err(error);
        }
        output.sort_by(|left, right| left.relative.cmp(&right.relative));
        Ok(output)
    })
}

fn scan_source_path(
    root: &Path,
    root_capability: &ProjectRoot,
    path: &Path,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
    python_version: PythonVersion,
    go_capture: &GoProjectCapture,
    delta: Option<&source_frontier::SourceDelta>,
    source_policy: SourceAdmissionPolicy,
    cancellation: Option<&AtomicBool>,
) -> Result<Option<ScannedFile>, String> {
    check_scan_cancellation(cancellation)?;
    if let Some(cached) = delta
        .filter(|delta| delta.is_current())
        .and_then(|delta| delta.unchanged.get(path))
    {
        let cached_length_admitted = usize::try_from(cached.revision.length)
            .ok()
            .is_some_and(|length| source_policy.admit_actual_file(length).is_ok());
        if cached_length_admitted {
            let record = reusable
                .get(&cached.key)
                .ok_or_else(|| "source frontier lost its exact CAS row".to_owned())?;
            let source_fact_identity = record
                .file_fields()
                .and_then(|fields| fields.source_identity)
                .ok_or_else(|| {
                    "source frontier row lost its exact source-fact identity".to_owned()
                })?;
            let profile = project_profile_fault(
                frontends,
                path,
                python_version,
                go_capture.literal_version(Path::new(&cached.relative_path)),
            )
            .map_err(|fault| match fault {
                SourceFault::Unavailable(_)
                | SourceFault::UnavailableRead(_, _)
                | SourceFault::Vanished => "source frontier profile changed".to_owned(),
                SourceFault::Fatal(error) => error,
            })?;
            let source_bytes = usize::try_from(cached.revision.length)
                .map_err(|_| "source byte length exceeds this target".to_owned())?;
            let mut file = reused_scanned_file(
                cached.relative_path.clone(),
                cached.key,
                record.clone(),
                source_bytes,
                0,
                cached.content,
                profile,
                source_fact_identity,
                Some(cached.encoded_record_bytes),
            )?;
            file.encoded_fact_bytes = cached.encoded_fact_bytes;
            return Ok(Some(file));
        }
    }
    scan_file(
        root,
        root_capability,
        path,
        project,
        reusable,
        frontends,
        python_version,
        go_capture,
        source_policy,
    )
}

/// Compares a confined host file with its indexed source identity.
/// `Err` means no comparison was possible; it never implies freshness.
pub(super) fn indexed_source_matches(
    root: &Path,
    path: &str,
    expected: backend_version::ContentId<backend_version::SourceFactDomain>,
    expected_input: [u8; 32],
) -> Result<bool, ()> {
    if path.is_empty() || path.contains('\\') {
        return Err(());
    }
    let relative = Path::new(path);
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(());
    }
    let policy = SourceAdmissionPolicy::from_environment().map_err(|_| ())?;
    let bytes = ProjectRoot::open(root)
        .map_err(|_| ())?
        .read(relative, policy.limits().max_file_source_bytes)
        .map_err(|_| ())?;
    policy.admit_actual_file(bytes.len()).map_err(|_| ())?;
    match backend_compile::VerifiedSourceText::admit_stored(&bytes, expected_input, expected) {
        Ok(_) => Ok(true),
        Err(
            backend_compile::SourceVerificationError::InputRevisionMismatch
            | backend_compile::SourceVerificationError::SourceIdentityMismatch
            | backend_compile::SourceVerificationError::NonUtf8,
        ) => Ok(false),
        Err(_) => Err(()),
    }
}

/// Compares one selected native-only evidence claim with the same confined host
/// bytes used for length and both content-domain checks. The caller must first
/// admit the selected local project/root, publication, program/image and entity.
/// A claim is never a source extent or proof of a complete declaration inventory.
/// `Err` means no trustworthy comparison; `Ok(false)` means the host is stale.
pub(super) fn indexed_native_evidence_matches(
    root: &Path,
    evidence: &backend_engine::builtin::NativeSourceEvidence,
    image_source: backend_semantic::ir::SourceIdentity,
) -> Result<bool, ()> {
    evidence.validate().map_err(|_| ())?;
    if image_source.byte_len != evidence.byte_length
        || image_source.identity.as_ref() != evidence.source_digest.as_slice()
    {
        return Err(());
    }
    let policy = SourceAdmissionPolicy::from_environment().map_err(|_| ())?;
    let expected = evidence.byte_length as usize;
    policy.admit_actual_file(expected).map_err(|_| ())?;
    let bytes = match ProjectRoot::open(root).map_err(|_| ())?
        .read(Path::new(&evidence.path), expected)
    {
        Ok(bytes) => bytes,
        Err(SourceFault::Unavailable(SourceUnavailableReason::TooLarge)
            | SourceFault::UnavailableRead(SourceUnavailableReason::TooLarge, _)) => return Ok(false),
        Err(_) => return Err(()),
    };
    policy.admit_actual_file(bytes.len()).map_err(|_| ())?;
    Ok(evidence.verify_host(&bytes, image_source.identity).is_ok())
}

/// Reads a caller-admitted native extent from one confined, reverified host
/// buffer. The evidence row supplies no extent; the caller must independently
/// admit the selected project, program/image, entity, and nonempty owner extent.
pub(super) fn read_indexed_native_evidence_range(
    root: &Path,
    evidence: &backend_engine::builtin::NativeSourceEvidence,
    image_source: backend_semantic::ir::SourceIdentity,
    extent: (u32, u32),
    offset: u32,
    limit: u32,
) -> Result<super::source_range::OwnedSourceRange, super::source_range::SourceRangeError> {
    use super::source_range::{OwnedSourceRange, SourceRangeError};
    if extent.0 >= extent.1 {
        return Err(SourceRangeError::InvalidExtent);
    }
    let bytes = read_native_evidence_host(root, evidence, image_source)?;
    let page = super::source_range::native_page(
        &bytes,
        image_source.identity,
        evidence.input_digest,
        extent,
        offset,
        limit,
    )?;
    Ok(OwnedSourceRange {
        text: page.text.to_owned(),
        start: page.start,
        end: page.end,
        total: page.total,
        source_identity: *image_source.identity.as_ref(),
        owner_start_line: page.owner_start_line,
    })
}

/// The default declaration excerpt: the first `limit` bytes of the native extent,
/// preceded by the modifiers and leading comment the compiler keeps outside it.
/// The head counts against `limit`, so the excerpt never exceeds the product bound.
pub(super) fn read_indexed_native_evidence_excerpt(
    root: &Path,
    evidence: &backend_engine::builtin::NativeSourceEvidence,
    image_source: backend_semantic::ir::SourceIdentity,
    extent: (u32, u32),
    limit: u32,
) -> Result<(String, super::source_range::OwnedSourceRange, u32), super::source_range::SourceRangeError> {
    use super::source_range::{OwnedSourceRange, SourceRangeError};
    if extent.0 >= extent.1 {
        return Err(SourceRangeError::InvalidExtent);
    }
    let bytes = read_native_evidence_host(root, evidence, image_source)?;
    let head = super::source_range::native_context_head(
        &bytes,
        image_source.identity,
        evidence.input_digest,
        extent,
    )?;
    let room = limit.saturating_sub(u32::try_from(head.text.len()).unwrap_or(limit)).max(4);
    let (head_text, head_lines) = if head.text.len() + 4 > limit as usize {
        (String::new(), 0)
    } else {
        (head.text.to_owned(), head.lines)
    };
    let page = super::source_range::native_page(
        &bytes,
        image_source.identity,
        evidence.input_digest,
        extent,
        0,
        if head_text.is_empty() { limit } else { room },
    )?;
    let page = OwnedSourceRange {
        text: page.text.to_owned(),
        start: page.start,
        end: page.end,
        total: page.total,
        source_identity: *image_source.identity.as_ref(),
        owner_start_line: page.owner_start_line,
    };
    Ok((head_text, page, head_lines))
}

fn read_native_evidence_host(
    root: &Path,
    evidence: &backend_engine::builtin::NativeSourceEvidence,
    image_source: backend_semantic::ir::SourceIdentity,
) -> Result<Vec<u8>, super::source_range::SourceRangeError> {
    use super::source_range::SourceRangeError;
    evidence
        .validate()
        .map_err(|_| SourceRangeError::Unavailable)?;
    if !evidence.admits_image()
        || image_source.byte_len != evidence.byte_length
        || image_source.identity.as_ref() != evidence.source_digest.as_slice()
    {
        return Err(SourceRangeError::Unavailable);
    }
    let policy =
        SourceAdmissionPolicy::from_environment().map_err(|_| SourceRangeError::Unavailable)?;
    let expected = evidence.byte_length as usize;
    policy.admit_actual_file(expected).map_err(|_| SourceRangeError::Unavailable)?;
    let bytes = ProjectRoot::open(root)
        .map_err(|_| SourceRangeError::Unavailable)?
        .read(Path::new(&evidence.path), expected)
        .map_err(|error| match error {
            SourceFault::Unavailable(SourceUnavailableReason::TooLarge)
            | SourceFault::UnavailableRead(SourceUnavailableReason::TooLarge, _) => SourceRangeError::Stale,
            _ => SourceRangeError::Unavailable,
        })?;
    policy
        .admit_actual_file(bytes.len())
        .map_err(|_| SourceRangeError::Unavailable)?;
    if bytes.len() != evidence.byte_length as usize {
        return Err(SourceRangeError::Stale);
    }
    evidence.verify_host(&bytes, image_source.identity)
        .map_err(|_| SourceRangeError::Stale)?;
    Ok(bytes.to_vec())
}

/// Reads a header extent already joined to the selected compiler entity.
/// The source is reopened under the same digest and policy as native ranges.
pub(super) fn read_indexed_native_evidence_header(
    root: &Path,
    evidence: &backend_engine::builtin::NativeSourceEvidence,
    image_source: backend_semantic::ir::SourceIdentity,
    extent: (u32, u32),
) -> Result<String, super::source_range::SourceRangeError> {
    use super::source_range::SourceRangeError;
    let length = extent.1.checked_sub(extent.0).ok_or(SourceRangeError::InvalidExtent)? as usize;
    if length == 0 || length > backend_compile::SourceDeclaration::MAX_TEXT_BYTES {
        return Err(SourceRangeError::InvalidExtent);
    }
    let bytes = read_native_evidence_host(root, evidence, image_source)?;
    let text = std::str::from_utf8(bytes.get(extent.0 as usize..extent.1 as usize)
        .ok_or(SourceRangeError::InvalidExtent)?)
        .map_err(|_| SourceRangeError::InvalidExtent)?;
    Ok(text.to_owned())
}

/// Reopens one confined indexed host and reads only a verified declaration range.
/// The caller is responsible for selecting the exact revision, file fact, and
/// declaration binding before invoking this helper.
pub(super) fn read_indexed_source_range(
    root: &Path,
    path: &str,
    expected_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    expected_input: [u8; 32],
    binding: backend_compile::SourceBindingCapture,
    name: &str,
    offset: u32,
    limit: u32,
) -> Result<super::source_range::OwnedSourceRange, super::source_range::SourceRangeError> {
    use super::source_range::{OwnedSourceRange, SourceRangeError};
    if path.is_empty() || path.contains('\\') {
        return Err(SourceRangeError::Unavailable);
    }
    let relative = Path::new(path);
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(SourceRangeError::Unavailable);
    }
    let policy =
        SourceAdmissionPolicy::from_environment().map_err(|_| SourceRangeError::Unavailable)?;
    let bytes = ProjectRoot::open(root)
        .map_err(|_| SourceRangeError::Unavailable)?
        .read(relative, policy.limits().max_file_source_bytes)
        .map_err(|_| SourceRangeError::Unavailable)?;
    policy
        .admit_actual_file(bytes.len())
        .map_err(|_| SourceRangeError::Unavailable)?;
    let page = super::source_range::page(
        &bytes,
        expected_source,
        expected_input,
        binding,
        name,
        offset,
        limit,
    )?;
    Ok(OwnedSourceRange {
        text: page.text.to_owned(),
        start: page.start,
        end: page.end,
        total: page.total,
        source_identity: *expected_source.as_ref(),
        owner_start_line: page.owner_start_line,
    })
}

/// Reads a compiler-selected extent from the current confined host bytes.
/// Publication identity and extent semantics must be admitted by the caller.
/// The default excerpt of a native extent read by path: the first `limit` bytes of the extent,
/// preceded by the modifiers and leading comment the compiler keeps outside it. The head counts
/// against `limit`, so the excerpt never exceeds the product bound.
pub(super) fn read_indexed_native_excerpt(
    root: &Path,
    path: &str,
    expected_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    expected_input: [u8; 32],
    extent: (u32, u32),
    limit: u32,
) -> Result<(String, super::source_range::OwnedSourceRange, u32), super::source_range::SourceRangeError> {
    use super::source_range::{OwnedSourceRange, SourceRangeError};
    if path.is_empty() || path.contains('\\') {
        return Err(SourceRangeError::Unavailable);
    }
    let relative = Path::new(path);
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(SourceRangeError::Unavailable);
    }
    let policy =
        SourceAdmissionPolicy::from_environment().map_err(|_| SourceRangeError::Unavailable)?;
    let bytes = ProjectRoot::open(root)
        .map_err(|_| SourceRangeError::Unavailable)?
        .read(relative, policy.limits().max_file_source_bytes)
        .map_err(|_| SourceRangeError::Unavailable)?;
    policy
        .admit_actual_file(bytes.len())
        .map_err(|_| SourceRangeError::Unavailable)?;
    let head = super::source_range::native_context_head(&bytes, expected_source, expected_input, extent)?;
    let (head_text, head_lines, room) = if head.text.len() + 4 > limit as usize {
        (String::new(), 0, limit)
    } else {
        (
            head.text.to_owned(),
            head.lines,
            limit - u32::try_from(head.text.len()).unwrap_or(0),
        )
    };
    let page = super::source_range::native_page(&bytes, expected_source, expected_input, extent, 0, room)?;
    Ok((
        head_text,
        OwnedSourceRange {
            text: page.text.to_owned(),
            start: page.start,
            end: page.end,
            total: page.total,
            source_identity: *expected_source.as_ref(),
            owner_start_line: page.owner_start_line,
        },
        head_lines,
    ))
}

pub(super) fn read_indexed_native_range(
    root: &Path,
    path: &str,
    expected_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    expected_input: [u8; 32],
    extent: (u32, u32),
    offset: u32,
    limit: u32,
) -> Result<super::source_range::OwnedSourceRange, super::source_range::SourceRangeError> {
    use super::source_range::{OwnedSourceRange, SourceRangeError};
    if path.is_empty() || path.contains('\\') {
        return Err(SourceRangeError::Unavailable);
    }
    let relative = Path::new(path);
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(SourceRangeError::Unavailable);
    }
    let policy =
        SourceAdmissionPolicy::from_environment().map_err(|_| SourceRangeError::Unavailable)?;
    let bytes = ProjectRoot::open(root)
        .map_err(|_| SourceRangeError::Unavailable)?
        .read(relative, policy.limits().max_file_source_bytes)
        .map_err(|_| SourceRangeError::Unavailable)?;
    policy
        .admit_actual_file(bytes.len())
        .map_err(|_| SourceRangeError::Unavailable)?;
    let page = super::source_range::native_page(
        &bytes,
        expected_source,
        expected_input,
        extent,
        offset,
        limit,
    )?;
    Ok(OwnedSourceRange {
        text: page.text.to_owned(),
        start: page.start,
        end: page.end,
        total: page.total,
        source_identity: *expected_source.as_ref(),
        owner_start_line: page.owner_start_line,
    })
}

/// Recovers one declaration excerpt from its already indexed source file.
///
/// This only serves bytes that still match the indexed source identity. It
/// reads through the confined project-root capability and reruns the admitted
/// baseline parser, retaining its declaration matching and excerpt bounds.
pub(super) fn recover_indexed_excerpt(
    root: &Path,
    path: &str,
    expected_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    expected_input: [u8; 32],
    language: SourceLanguage,
    label: &str,
    line: u32,
    kind: Option<DeclarationKind>,
) -> Option<SourceExcerpt> {
    if path.is_empty() || path.contains('\\') {
        return None;
    }
    let relative = Path::new(path);
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return None;
    }
    let source_policy = SourceAdmissionPolicy::from_environment().ok()?;
    let bytes = ProjectRoot::open(root)
        .ok()?
        .read(relative, source_policy.limits().max_file_source_bytes)
        .ok()?;
    source_policy.admit_actual_file(bytes.len()).ok()?;
    backend_compile::VerifiedSourceText::admit_stored(&bytes, expected_input, expected_source)
        .ok()?;
    let frontend = frontends().ok()?.for_language(language)?;
    let analysis = frontend.baseline.analyze(relative, &bytes).ok()?;
    let name = label.rsplit("::").next()?;
    let mut matches = analysis.declarations().iter().filter(|declaration| {
        declaration.location().path() == path
            && declaration.location().start_line() == line
            && declaration.name() == name
            && kind.is_none_or(|kind| declaration.kind() == kind)
    });
    let declaration = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    match declaration.source_excerpt() {
        SourceExcerpt::Captured { .. } => Some(declaration.source_excerpt().clone()),
        SourceExcerpt::NotCaptured | SourceExcerpt::NotHydrated | SourceExcerpt::Unconfigured => {
            None
        }
    }
}

/// Reads supported sources and reuses prior analyses behind exact content and
/// producer-version fences.
pub(super) fn scan_project(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
) -> Result<IndexSnapshot, String> {
    scan_project_with_configuration_policy(
        coordinate,
        project,
        reusable,
        source_selection_policy(),
        true,
    )
}

/// Scans one project for compiler lanes that cannot prove a complete input
/// read set. Bounded configuration observations still contribute to freshness;
/// observing them does not prove dynamic or negative reads or authorize reuse.
pub(super) fn scan_project_for_unproven_authorities(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
) -> Result<IndexSnapshot, String> {
    scan_project_with_configuration_policy(
        coordinate,
        project,
        reusable,
        source_selection_policy(),
        true,
    )
}

/// Cancellable variant used by asynchronous local index jobs. Cancellation is
/// checked during directory discovery, source analysis, and configuration
/// capture so a large project scan can stop without cancelling compiler work.
pub(super) fn scan_project_for_unproven_authorities_cancellable(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    cancellation: &AtomicBool,
) -> Result<IndexSnapshot, String> {
    scan_project_for_unproven_authorities_cancellable_with_interpreter(
        coordinate,
        project,
        reusable,
        cancellation,
        None,
    )
}

pub(super) fn scan_project_for_unproven_authorities_cancellable_with_interpreter(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    cancellation: &AtomicBool,
    interpreter: Option<&backend_engine::application::PythonInterpreterHostAdmission>,
) -> Result<IndexSnapshot, String> {
    scan_project_for_unproven_authorities_cancellable_with_observation(
        coordinate,
        project,
        reusable,
        cancellation,
        interpreter,
        None,
    )
}

pub(super) fn scan_project_for_unproven_authorities_cancellable_with_observation(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    cancellation: &AtomicBool,
    interpreter: Option<&backend_engine::application::PythonInterpreterHostAdmission>,
    observation: Option<&crate::builtin::index_work_observation::IndexWorkObservation>,
) -> Result<IndexSnapshot, String> {
    let source_policy = SourceAdmissionPolicy::from_environment()?;
    check_scan_cancellation(Some(cancellation))?;
    scan_project_with_configuration_policy_attempt(
        coordinate,
        project,
        reusable,
        source_selection_policy(),
        true,
        true,
        Some(cancellation),
        source_policy,
        interpreter,
        None,
        observation,
        NativeTsScanMode::Legacy,
    )
}

/// Refusal for a native TypeScript scan that also meets another supported
/// source language. Only the forced and native-only modes can produce it.
const NATIVE_TS_MIXED_REFUSAL: &str = "native TypeScript scan cannot admit mixed legacy source lanes";

/// Chooses the TS-native lane from the same bounded walk. A prior selected
/// NSE row forces that lane even after its last TS path disappears.
///
/// Native TypeScript admits only a project whose supported sources are all
/// TypeScript. A walk with a TypeScript path and any other supported source
/// language (Rust, Python, Go, Java, C#, C or C++) is rescanned in the legacy
/// structural lane, which indexes every language exactly as it does without
/// TypeScript routing. A project that already has selected native evidence
/// cannot make that move here, so it refuses and says why instead of
/// publishing a partial or empty result.
pub(super) fn scan_project_with_native_typescript_auto(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    cancellation: Arc<AtomicBool>,
    force_native: bool,
    interpreter: Option<&backend_engine::application::PythonInterpreterHostAdmission>,
    observation: Option<&crate::builtin::index_work_observation::IndexWorkObservation>,
) -> Result<IndexSnapshot, String> {
    let policy = SourceAdmissionPolicy::from_environment()?;
    let mut snapshot = scan_project_with_configuration_policy_attempt(
        coordinate, project, reusable, source_selection_policy(), true, true,
        Some(cancellation.as_ref()), policy, interpreter, None, observation,
        if force_native { NativeTsScanMode::Native } else { NativeTsScanMode::Auto },
    )
    .map_err(|error| {
        if force_native && error == NATIVE_TS_MIXED_REFUSAL {
            format!(
                "{error}: this project already has selected native TypeScript evidence and also \
                 contains another supported source language; nothing was indexed or dropped, and \
                 the workspace is unchanged"
            )
        } else {
            error
        }
    })?;
    if let Some(capture) = snapshot.native_typescript_capture.as_mut() {
        capture.cancellation = Some(cancellation);
    }
    Ok(snapshot)
}

/// Native-only TS scan. Mixed legacy-source lanes are refused; no syntax
/// frontend registry is initialized. Product routing reaches this lane through
/// `scan_project_with_native_typescript_auto`.
pub(super) fn scan_native_typescript_only(
    coordinate: &str,
    project: [u8; 32],
    cancellation: Arc<AtomicBool>,
) -> Result<IndexSnapshot, String> {
    let source_policy = SourceAdmissionPolicy::from_environment()?;
    let mut snapshot = scan_project_with_configuration_policy_attempt(
        coordinate, project, &BTreeMap::new(), source_selection_policy(), true, false,
        Some(cancellation.as_ref()), source_policy, None, None, None, NativeTsScanMode::Native,
    )?;
    check_scan_cancellation(Some(cancellation.as_ref()))?;
    let capture = snapshot.native_typescript_capture.as_mut()
        .ok_or("native TypeScript scan did not retain its capture")?;
    capture.cancellation = Some(cancellation);
    Ok(snapshot)
}

/// Reads supported sources under one shared discovery policy.
///
/// Keeping the policy as an argument makes the same selection contract usable
/// by local ingest and future package/archive graph adapters.  The default
/// entry point above preserves the production behavior for existing callers.
pub(super) fn scan_project_with_policy(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    discovery: DiscoveryPolicy,
) -> Result<IndexSnapshot, String> {
    scan_project_with_configuration_policy(coordinate, project, reusable, discovery, true)
}

fn scan_project_with_configuration_policy(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    discovery: DiscoveryPolicy,
    capture_configuration_contents: bool,
) -> Result<IndexSnapshot, String> {
    let source_policy = SourceAdmissionPolicy::from_environment()?;
    scan_project_with_configuration_policy_attempt(
        coordinate,
        project,
        reusable,
        discovery,
        capture_configuration_contents,
        true,
        None,
        source_policy,
        None,
        None,
        None,
        NativeTsScanMode::Legacy,
    )
}

#[derive(Clone, Copy)]
enum NativeTsScanMode {
    /// Structural frontends only; TypeScript is an ordinary legacy language.
    Legacy,
    /// Native TypeScript only; any other supported language is refused.
    Native,
    /// Native when TypeScript is the only supported language, else legacy.
    Auto,
}

fn scan_project_with_configuration_policy_attempt(
    coordinate: &str,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    discovery: DiscoveryPolicy,
    capture_configuration_contents: bool,
    allow_frontier_reuse: bool,
    cancellation: Option<&AtomicBool>,
    source_policy: SourceAdmissionPolicy,
    interpreter: Option<&backend_engine::application::PythonInterpreterHostAdmission>,
    captured_python: Option<PythonEnvironmentCapture>,
    observation: Option<&crate::builtin::index_work_observation::IndexWorkObservation>,
    mode: NativeTsScanMode,
) -> Result<IndexSnapshot, String> {
    check_scan_cancellation(cancellation)?;
    let requested_root = absolute_path_spelling(Path::new(coordinate))?;
    let root = requested_root
        .canonicalize()
        .map_err(|error| format!("open project {coordinate}: {error}"))?;
    if !root.is_dir() {
        return Err(format!("project {} is not a directory", root.display()));
    }
    let root_capability = ProjectRoot::open(&root)?;
    // Classify the same bounded walk before constructing a frontend. A walk
    // whose only supported language is TS takes the native lane, including
    // when CSS also appears. Any other supported language beside TS is
    // rescanned in the legacy lane below.
    let static_discovery = !matches!(mode, NativeTsScanMode::Legacy);
    let mut selected_frontends = if static_discovery { None } else { Some(frontends()?) };
    if let Some(observed) = observation {
        observed.begin_source_attempt();
    }
    let mut project_paths = project_paths_with_policy(
        &root,
        &root_capability,
        selected_frontends,
        static_discovery,
        discovery.clone(),
        cancellation,
    )?;
    check_scan_cancellation(cancellation)?;
    let mut paths = std::mem::take(&mut project_paths.sources);
    paths.sort();
    let mut native_paths = std::mem::take(&mut project_paths.native_typescript_paths);
    native_paths.sort();
    let native_typescript = match mode {
        NativeTsScanMode::Legacy => false,
        NativeTsScanMode::Native => true,
        NativeTsScanMode::Auto => !native_paths.is_empty(),
    };
    if static_discovery && !native_typescript {
        selected_frontends = Some(frontends()?);
    }
    let frontends = selected_frontends;
    let frontier_policy_allowed = !native_typescript && discovery == source_selection_policy();
    if native_typescript && !paths.is_empty() {
        if matches!(mode, NativeTsScanMode::Auto) {
            // Native evidence cannot carry a second language (the v13 intent
            // admits no structural rows), and dropping those files would hide
            // them. The legacy lane covers every language, TypeScript
            // included, so nothing is dropped and the result is the one
            // this project gets without TypeScript routing. This rewalks the
            // tree once, only for mixed projects.
            return scan_project_with_configuration_policy_attempt(
                coordinate,
                project,
                reusable,
                discovery,
                capture_configuration_contents,
                allow_frontier_reuse,
                cancellation,
                source_policy,
                interpreter,
                captured_python,
                observation,
                NativeTsScanMode::Legacy,
            );
        }
        return Err(NATIVE_TS_MIXED_REFUSAL.into());
    }
    // Admission is project-wide and precedes every revision/delta/row/fact key.
    // A negative environment honestly uses the existing source-only 3.14 default.
    let no_cancellation = AtomicBool::new(false);
    let python_control = PythonProjectControl {
        cancelled: cancellation.unwrap_or(&no_cancellation),
        deadline: std::time::Instant::now() + std::time::Duration::from_secs(15 * 60),
    };
    let python_environment = if paths.iter().any(|path| {
        frontends
            .and_then(|set| set.for_path(path))
            .is_some_and(|frontend| frontend.language() == SourceLanguage::Python)
    }) {
        Some(match captured_python {
            Some(captured) => captured,
            None => match interpreter {
                Some(admission) => admission.capture_project(&root, python_control),
                None => PythonEnvironmentCapture::capture(&root, python_control),
            }
            .map_err(|error| error.to_string())?,
        })
    } else {
        None
    };
    check_scan_cancellation(cancellation)?;
    let python_version = python_environment
        .as_ref()
        .map(PythonEnvironmentCapture::selected_profile)
        .transpose()
        .map_err(|error| error.to_string())?
        .flatten()
        .unwrap_or(PythonVersion::Python314);
    let directories = std::mem::take(&mut project_paths.directories);
    let (local_policy_paths, mut local_policy_paths_complete) =
        match source_frontier::source_policy_candidates(&root, &directories) {
            Some(paths) => (paths, true),
            None => (Vec::new(), false),
        };
    let mut file_revisions = BTreeMap::<PathBuf, CompilerFileRevision>::new();
    for path in paths.iter().chain(&native_paths) {
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| "discovered source escaped its project root".to_owned())?
            .to_path_buf();
        file_revisions.insert(
            relative.clone(),
            CompilerFileRevision {
                relative_path: relative,
                metadata: project_paths.file_revisions.get(path).copied().flatten(),
            },
        );
    }
    for (_, path) in &project_paths.configurations {
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| "discovered compiler configuration escaped its project root".to_owned())?
            .to_path_buf();
        file_revisions
            .entry(relative.clone())
            .or_insert_with(|| CompilerFileRevision {
                relative_path: relative,
                metadata: project_paths.file_revisions.get(path).copied().flatten(),
            });
    }
    for (path, metadata) in &project_paths.file_revisions {
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(source_frontier::is_policy_file_name)
        {
            let relative = path
                .strip_prefix(&root)
                .map_err(|_| "discovered policy file escaped its project root".to_owned())?
                .to_path_buf();
            file_revisions
                .entry(relative.clone())
                .or_insert_with(|| CompilerFileRevision {
                    relative_path: relative,
                    metadata: *metadata,
                });
        }
    }
    for path in &local_policy_paths {
        let relative = path
            .strip_prefix(&root)
            .map_err(|_| "discovered policy file escaped its project root".to_owned())?
            .to_path_buf();
        let metadata = file_system_revision(path);
        local_policy_paths_complete &= metadata.is_some();
        file_revisions
            .entry(relative.clone())
            .or_insert_with(|| CompilerFileRevision {
                relative_path: relative,
                metadata,
            });
    }
    let mut vue_files = Vec::with_capacity(project_paths.vue_paths.len());
    for path in &project_paths.vue_paths {
        let relative_path = path.strip_prefix(&root).map_err(|_| "Vue candidate escaped project root")?.to_path_buf();
        vue_files.push(CompilerFileRevision { relative_path, metadata: project_paths.file_revisions.get(path).copied().flatten() });
    }
    vue_files.sort_by(|a,b| a.relative_path.cmp(&b.relative_path));
    let vue_walk = VueWalkEvidence {
        project, root: root.clone(), files: vue_files,
        complete: project_paths.vue_inventory_complete,
        discovery_options: discovery.source_scope_identity_bytes(),
        source_policy: source_policy.identity(),
        local_policy_paths: local_policy_paths.clone(),
    };
    let mut stylo_files = Vec::with_capacity(project_paths.stylo_paths.len());
    for path in &project_paths.stylo_paths {
        let relative_path = path
            .strip_prefix(&root)
            .map_err(|_| "Stylo candidate escaped project root")?
            .to_path_buf();
        stylo_files.push(CompilerFileRevision {
            relative_path,
            metadata: project_paths.file_revisions.get(path).copied().flatten(),
        });
    }
    stylo_files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let stylo_walk = StyloWalkEvidence {
        project,
        root: root.clone(),
        files: stylo_files,
        inventory: project_paths.stylo_inventory,
        inventory_complete: project_paths.stylo_inventory_complete,
        discovery_options: discovery.source_scope_identity_bytes(),
        source_policy: source_policy.identity(),
        local_policy_paths: local_policy_paths.clone(),
    };
    let mut sql_files = Vec::with_capacity(project_paths.sql_paths.len());
    for path in &project_paths.sql_paths {
        let relative_path = path.strip_prefix(&root).map_err(|_| "SQL candidate escaped project root")?.to_path_buf();
        sql_files.push(CompilerFileRevision { relative_path, metadata: project_paths.file_revisions.get(path).copied().flatten() });
    }
    sql_files.sort_by(|left, right| left.relative_path.cmp(&right.relative_path));
    let sql_walk = SqlWalkEvidence { project, root: root.clone(), files: sql_files, inventory_complete: project_paths.sql_inventory_complete };
    let revision_fence = CompilerRevisionFence {
        requested_root,
        root: root.clone(),
        directories: directories.clone(),
        files: file_revisions.values().cloned().collect(),
        typescript_reuse: Vec::new(),
    };
    // Parse module languages from the same bounded/confined configuration
    // bytes used by freshness, before any analysis/frontier/row key is made.
    let relevant_languages = paths
        .iter()
        .filter_map(|path| frontends.and_then(|set| profile_of(set, path)))
        .map(LanguageProfile::language)
        .chain((native_typescript && !native_paths.is_empty()).then_some(Language::TypeScript))
        .collect::<BTreeSet<_>>();
    let has_go = relevant_languages.contains(&Language::Go);
    if let Some(observed) = observation {
        observed.phase("source-configuration-capture");
    }
    let mut compiler_configuration = if capture_configuration_contents || has_go {
        read_compiler_configuration_snapshot(
            &root,
            &root_capability,
            project_paths.configurations.clone(),
            project_paths.incomplete_configurations.clone(),
            if capture_configuration_contents {
                relevant_languages
            } else {
                BTreeSet::from([Language::Go])
            },
            cancellation,
        )?
    } else {
        CompilerConfigurationSnapshot::default()
    };
    if has_go
        && !compiler_configuration
            .complete_languages
            .contains(&Language::Go)
    {
        return Err(
            "Go module language requires a complete bounded configuration capture".to_owned(),
        );
    }
    for directory in &project_paths.go_absent_modules {
        compiler_configuration.go_capture.absent(directory.clone());
    }
    let go_capture = &compiler_configuration.go_capture;
    for path in &paths {
        if frontends
            .and_then(|set| set.for_path(path))
            .is_some_and(|frontend| frontend.language() == SourceLanguage::Go)
        {
            go_capture.validate_source(
                path.strip_prefix(&root)
                    .map_err(|_| "Go source escaped root")?,
            )?;
        }
    }
    let frontier_sources = paths
        .iter()
        .filter_map(|path| {
            let frontend = frontends.and_then(|set| set.for_path(path))?;
            let relative = path
                .strip_prefix(&root)
                .ok()?
                .to_str()?
                .replace(std::path::MAIN_SEPARATOR, "/");
            let analysis = project_analysis_version(
                frontend,
                python_version,
                go_capture,
                Path::new(&relative),
            );
            Some(source_frontier::SourcePath {
                absolute: path.clone(),
                relative,
                language: frontend.language(),
                analysis,
                revision: project_paths.file_revisions.get(path).copied().flatten(),
            })
        })
        .collect::<Vec<_>>();
    let mut configuration_paths = project_paths
        .configurations
        .iter()
        .map(|(_, path)| path.clone())
        .collect::<Vec<_>>();
    configuration_paths.extend(project_paths.file_revisions.keys().filter_map(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(source_frontier::is_policy_file_name)
            .then(|| path.clone())
    }));
    configuration_paths.sort();
    configuration_paths.dedup();
    let frontier_paths_complete = frontier_sources.len() == paths.len();
    let wanted_paths = frontier_paths_complete
        .then(|| {
            source_frontier::source_wanted_paths(&root, &frontier_sources, &configuration_paths)
        })
        .flatten();
    let git_before =
        (frontier_policy_allowed && frontier_paths_complete && local_policy_paths_complete)
            .then(|| {
                wanted_paths.as_ref().and_then(|wanted| {
                    source_frontier::git_source_state(&root, wanted, &local_policy_paths)
                })
            })
            .flatten();
    let source_delta = if frontier_policy_allowed && allow_frontier_reuse {
        git_before.as_ref().and_then(|state| {
            source_frontier::source_delta(
                &root,
                project,
                &frontier_sources,
                &configuration_paths,
                directories.iter().all(|(_, revision)| revision.is_some()),
                reusable,
                state,
                source_policy.identity(),
                |path| frontends.and_then(|set| set.for_path(path)).is_some(),
            )
        })
    } else {
        None
    };
    if let Some(observed) = observation {
        observed.phase("source-budget-preflight");
    }
    let mut preflight_paths = paths.clone();
    preflight_paths.extend(native_paths.iter().cloned());
    preflight_source_bytes(&root, &preflight_paths, source_policy, cancellation)?;
    if let Some(observed) = observation {
        observed.sources(paths.len().saturating_add(native_paths.len()));
        observed.phase("structural-source-analysis");
    }
    let mut source_ledger = SourceAdmissionLedger::default();
    let mut scanned = if let Some(frontends) = frontends {
        scan_source_paths(
            &root, &root_capability, &paths, project, reusable, frontends,
            python_version, go_capture, source_delta.as_ref(), source_policy,
            &mut source_ledger, cancellation, observation,
        )?
    } else { Vec::new() };
    let mut native_bytes_read = 0usize;
    let mut native_typescript_capture = if native_typescript {
        let mut capture = capture_native_typescript_sources(
            &root, &native_paths, project, source_policy, &mut source_ledger,
            &revision_fence, &mut native_bytes_read, cancellation,
        ).map_err(|error| format!("{error}; native source bytes read: {native_bytes_read}"))?;
        if discovery != source_selection_policy() {
            return Err("native TypeScript scan policy differs from its fixed recipe".into());
        }
        capture.fixed_discovery_policy = true;
        Some(capture)
    } else { None };
    let native_source_bytes = match &native_typescript_capture {
        Some(capture) => capture.sources.iter().try_fold(0usize, |total, source| total.checked_add(source.evidence.byte_length as usize))
            .ok_or("native source bytes overflow")?,
        None => 0,
    };
    let native_record_bytes = native_typescript_capture.as_ref().map_or(0, |capture| capture.encoded_bytes);
    let native_record_count = native_typescript_capture.as_ref().map_or(0, |capture| capture.sources.len());
    let mut native_policy_bytes_read = 0usize;
    let native_discovery = if native_typescript {
        if !local_policy_paths_complete { return Err("native TypeScript discovery policy inputs are incomplete".into()); }
        Some(native_typescript_discovery_basis(&stylo_walk, &revision_fence, cancellation, &mut native_policy_bytes_read)
            .map_err(|error| format!("{error}; native policy bytes read: {native_policy_bytes_read}"))?)
    } else { None };
    let stylo_budget_baseline = StyloBudgetBaseline {
        source_bytes: scanned.iter().try_fold(native_source_bytes, |total, file| total.checked_add(file.source_bytes))
            .ok_or("source budget baseline overflow")?,
        record_bytes: scanned.iter().try_fold(native_record_bytes, |total, file| {
            total.checked_add(file.encoded_record_bytes)?.checked_add(file.encoded_fact_bytes)
        }).ok_or("record budget baseline overflow")?,
        records: scanned.len().checked_add(native_record_count).ok_or("source record count overflow")?,
    };
    let initial_read = native_bytes_read.checked_add(native_policy_bytes_read).ok_or("source read count overflow")?;
    let source_bytes_read = scanned.iter().try_fold(initial_read, |total, file| total.checked_add(file.source_bytes_read))
        .ok_or("source read count overflow")?;
    let cache_witness = git_before;
    if source_delta.is_some() {
        #[cfg(test)]
        source_frontier::trigger_source_frontier_race(&root);
        let after = wanted_paths.as_ref().and_then(|wanted| {
            source_frontier::git_source_state(&root, wanted, &local_policy_paths)
        });
        let stable = cache_witness
            .as_ref()
            .zip(after.as_ref())
            .is_some_and(|(before, after)| {
                source_frontier::git_states_same_during_scan(before, after)
            });
        if !stable
            || !compiler_revision_is_current_with_cancellation(&revision_fence, cancellation)?
        {
            // This attempt is superseded. Keep its scalar read charge, but do
            // not retain every decoded fact page while the full retry builds
            // another complete source snapshot.
            #[cfg(test)]
            let discarded_row = scanned
                .iter()
                .find(|file| file.compiler_source.is_some())
                .and_then(|file| file.record.file_fields())
                .map(|file| std::sync::Arc::downgrade(file.declarations));
            drop(scanned);
            #[cfg(test)]
            assert!(
                discarded_row.is_none_or(|row| row.strong_count() == 0),
                "fresh superseded scan rows must retire before the full retry"
            );
            let mut full = scan_project_with_configuration_policy_attempt(
                coordinate,
                project,
                reusable,
                discovery,
                capture_configuration_contents,
                false,
                cancellation,
                source_policy,
                interpreter,
                python_environment.clone(),
                observation,
                if native_typescript { NativeTsScanMode::Native } else { NativeTsScanMode::Legacy },
            )?;
            full.source_bytes_read = full.source_bytes_read.saturating_add(source_bytes_read);
            return Ok(full);
        }
    }
    if let Some(observed) = observation {
        observed.phase("source-facts-assembly-and-final-fence");
    }
    scanned.sort_by(|left, right| left.relative.cmp(&right.relative));

    let mut source = blake3::Hasher::new();
    source.update(b"backend.project-snapshot.v2\0");
    if let Some(capture) = &native_typescript_capture {
        source.update(b"nudox.native-typescript-source-set.v1\0");
        source.update(&capture.source_version);
    }
    let mut files = Vec::with_capacity(scanned.len());
    let mut source_facts = Vec::with_capacity(scanned.len());
    let mut encoded_fact_bytes = Vec::with_capacity(scanned.len());
    let mut compiler_sources = Vec::with_capacity(scanned.len());
    let mut native_by_profile =
        BTreeMap::<LanguageProfile, Vec<(String, NativePathDisposition)>>::new();
    let mut native_clang_candidates = Vec::new();
    let mut native_csharp_candidates = Vec::new();
    for file in &scanned {
        if let Some((profile, disposition)) = &file.native_frontier {
            native_by_profile
                .entry(*profile)
                .or_default()
                .push((file.relative.clone(), disposition.clone()));
        }
        native_clang_candidates.extend(file.native_clang_candidate.iter().cloned());
        native_csharp_candidates.extend(file.native_csharp_candidate.iter().cloned());
    }
    let mut native_frontier = Vec::new();
    for (profile, paths) in native_by_profile {
        let mut content = Vec::new();
        for file in &mut scanned {
            if let Some(candidate) = file.native_clang_candidate.as_mut()
                && candidate.profile == profile
            {
                let bytes = candidate
                    .source_bytes
                    .take()
                    .ok_or_else(|| "native candidate bytes were already consumed".to_owned())?;
                content.push((candidate.relative_path.clone(), bytes.into_vec()));
            }
            if let Some(candidate) = file.native_csharp_candidate.as_mut()
                && candidate.profile == profile
            {
                let bytes = candidate
                    .source_bytes
                    .take()
                    .ok_or_else(|| "native candidate bytes were already consumed".to_owned())?;
                content.push((candidate.relative_path.clone(), bytes.into_vec()));
            }
        }
        let records = build_native_frontier_records_with_content(project, profile, paths, content)
            .map_err(|error| format!("build native Clang frontier: {error}"))?;
        native_frontier.extend(records.into_iter().map(|(_, record)| record));
    }
    // Parser-unavailable C/C++ rows retain the ordinary zero content version.
    // Bind their exact byte identity through the separate native frontier so
    // a malformed-source edit still changes the selected project source root.
    update_source_version_native_frontier(&mut source, &native_frontier);
    let mut reused_compiler_files = Vec::with_capacity(scanned.len());
    for scanned in scanned {
        let ScannedFile {
            key,
            record,
            compiler_source,
            reused_compiler,
            source_facts: facts,
            source_bytes_read: _,
            encoded_fact_bytes: fact_bytes,
            ..
        } = scanned;
        let file = record
            .file_fields()
            .ok_or_else(|| "frontend produced a non-file record".to_owned())?;
        source.update(&key);
        source.update(&file.content_version);
        source.update(&file.analysis_version);
        encoded_fact_bytes.push((key, fact_bytes));
        files.push((key, record));
        source_facts.extend(facts);
        compiler_sources.extend(compiler_source);
        reused_compiler_files.extend(reused_compiler);
    }
    if let Some(capture) = &native_typescript_capture {
        compiler_sources.extend(capture.sources.iter().map(|source| CompilerSourceHandle {
            relative_path: source.evidence.path.clone(),
            profile: source.evidence.profile,
            content: source.evidence.input_digest,
            source_fact_identity: source.source_fact_identity,
            expected_byte_length: Some(source.evidence.byte_length),
        }));
    }
    files.sort_by_key(|(key, _)| *key);
    encoded_fact_bytes.sort_unstable_by_key(|(key, _)| *key);
    let relevant_languages = compiler_sources
        .iter()
        .map(|source| source.profile.language())
        .chain(
            native_clang_candidates
                .iter()
                .map(|source| source.profile.language()),
        )
        .chain(
            native_csharp_candidates
                .iter()
                .map(|source| source.profile.language()),
        )
        .chain(
            reused_compiler_files
                .iter()
                .map(|source| source.profile.language()),
        )
        .chain((native_typescript && !native_paths.is_empty()).then_some(Language::TypeScript))
        .collect();
    let compiler_configuration = if capture_configuration_contents {
        read_compiler_configuration_snapshot(
            &root,
            &root_capability,
            project_paths.configurations,
            project_paths.incomplete_configurations,
            relevant_languages,
            cancellation,
        )?
    } else {
        CompilerConfigurationSnapshot::default()
    };
    if let Some(discovery_basis) = native_discovery {
        if !native_paths.is_empty() && !compiler_configuration.complete_languages.contains(&Language::TypeScript) {
            return Err("native TypeScript configuration inputs are incomplete".into());
        }
        let mut configurations = Vec::new();
        for file in &compiler_configuration.files {
            if file.language == Language::TypeScript {
                configurations.push((normalized_workspace_path(&file.relative_path)?, file.content));
            }
        }
        configurations.sort_unstable_by(|left, right| left.0.cmp(&right.0));
        if configurations.windows(2).any(|pair| pair[0].0 == pair[1].0) {
            return Err("duplicate native TypeScript configuration path".into());
        }
        source.update(b"nudox.native-typescript-input-basis.v1\0");
        source.update(&discovery_basis);
        source.update(&[u8::from(compiler_configuration.complete_languages.contains(&Language::TypeScript))]);
        source.update(&(configurations.len() as u64).to_be_bytes());
        for (path, content) in configurations {
            source.update(&(path.len() as u64).to_be_bytes());
            source.update(path.as_bytes());
            source.update(&content);
        }
        if !compiler_revision_is_current_with_cancellation(&revision_fence, cancellation)? {
            return Err("native TypeScript source or configuration changed before scan completion".into());
        }
    }
    let source_version = *source.finalize().as_bytes();
    if let Some(capture) = &mut native_typescript_capture {
        capture.project_source_version = Some(source_version);
    }
    let snapshot = IndexSnapshot {
        vue_scan_identity: std::sync::Arc::new(()),
        source_selection: backend_library::SourceSelectionObservation::project_extensions(
            project_paths.unsupported_css,
            project_paths.unsupported_sql,
            project_paths.unsupported_vue,
        ),
        stylo_walk,
        sql_walk,
        vue_walk,
        stylo_budget_baseline,
        python_environment: python_environment.map(PythonEnvironmentInput::Captured),
        source_version,
        files,
        source_facts,
        encoded_fact_bytes,
        compiler_sources,
        native_typescript_capture,
        native_frontier,
        native_clang_candidates,
        native_csharp_candidates,
        reused_compiler_files,
        compiler_configuration,
        revision_fence,
        source_bytes_read,
        source_admission_policy: source_policy,
    };
    if let (Some(before), Some(wanted)) = (cache_witness.as_ref(), wanted_paths.as_ref()) {
        let after = source_frontier::git_source_state(&root, wanted, &local_policy_paths);
        if after.as_ref().is_some_and(|after| {
            source_frontier::git_states_same_during_scan(before, after)
                && compiler_revision_is_current(&snapshot.revision_fence).unwrap_or(false)
        }) {
            source_frontier::remember_snapshot_frontier(&root, project, &snapshot, before);
        }
    }
    Ok(snapshot)
}

fn update_source_version_native_frontier(
    source: &mut blake3::Hasher,
    records: &[ProductNativeFrontierRecord],
) {
    if records.is_empty() {
        return;
    }
    source.update(b"nudox.project-native-frontier-source-version.v1\0");
    for record in records
        .iter()
        .filter(|record| matches!(record, ProductNativeFrontierRecord::Profile { .. }))
    {
        let mut encoded = Vec::new();
        <ProductNativeFrontierRelation as Relation>::encode_value(record, &mut encoded);
        source.update(&(encoded.len() as u64).to_be_bytes());
        source.update(&encoded);
    }
}

/// Revalidates the scanner's source/configuration revision without a second
/// recursive discovery pass or another source-content read.
pub(super) fn compiler_revision_is_current(fence: &CompilerRevisionFence) -> Result<bool, String> {
    compiler_revision_is_current_with_cancellation(fence, None)
}

pub(super) fn compiler_revision_is_current_with_cancellation(
    fence: &CompilerRevisionFence,
    cancellation: Option<&AtomicBool>,
) -> Result<bool, String> {
    if fence
        .typescript_reuse
        .iter()
        .any(|proof| !proof.is_current(cancellation))
    {
        return Ok(false);
    }
    if fence.open_bound_root(&fence.requested_root).is_err() {
        return Ok(false);
    }
    #[cfg(windows)]
    {
        let root_capability = match ProjectRoot::open(&fence.root) {
            Ok(root) => root,
            Err(_) => return Ok(false),
        };
        for (relative, expected) in &fence.directories {
            check_scan_cancellation(cancellation)?;
            if expected.is_none() || root_capability.revision_relative(relative).ok() != *expected {
                return Ok(false);
            }
        }
        for file in &fence.files {
            check_scan_cancellation(cancellation)?;
            if file.metadata.is_none()
                || root_capability.revision_relative(&file.relative_path).ok() != file.metadata
            {
                return Ok(false);
            }
        }
        Ok(fence.open_bound_root(&fence.requested_root).is_ok())
    }
    #[cfg(not(windows))]
    {
        for (relative, expected) in &fence.directories {
            check_scan_cancellation(cancellation)?;
            let path = if relative.as_os_str().is_empty() {
                fence.root.clone()
            } else {
                fence.root.join(relative)
            };
            if expected.is_none() || file_system_revision(&path) != *expected {
                return Ok(false);
            }
        }
        for file in &fence.files {
            check_scan_cancellation(cancellation)?;
            let path = fence.root.join(&file.relative_path);
            if file.metadata.is_none() || file_system_revision(&path) != file.metadata {
                return Ok(false);
            }
        }
        Ok(fence.open_bound_root(&fence.requested_root).is_ok())
    }
}

#[cfg(not(windows))]
fn file_system_revision(path: &Path) -> Option<FileSystemRevision> {
    let metadata = fs::symlink_metadata(path).ok()?;
    Some(file_system_revision_from_metadata(&metadata))
}

/// Reads the revision of the object `path` names, walking every component
/// relative to a held handle. A reparse point anywhere, including the final
/// component, makes the revision unavailable rather than describing its target.
#[cfg(windows)]
fn file_system_revision(path: &Path) -> Option<FileSystemRevision> {
    backend_platform::win32::project_fs::revision_of_path(path)
        .ok()
        .map(file_system_revision_from_windows)
}

#[cfg(not(windows))]
fn file_system_revision_from_metadata(metadata: &fs::Metadata) -> FileSystemRevision {
    #[cfg(unix)]
    let unix = {
        use std::os::unix::fs::MetadataExt as _;
        (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        )
    };
    FileSystemRevision {
        length: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        unix,
    }
}

#[cfg(windows)]
fn file_system_revision_from_windows(
    revision: backend_platform::win32::project_fs::FileRevision,
) -> FileSystemRevision {
    FileSystemRevision {
        length: revision.length,
        modified: None,
        windows: revision,
    }
}

fn file_system_revision_from_open_file(file: &fs::File) -> Result<FileSystemRevision, String> {
    #[cfg(unix)]
    {
        let metadata = file
            .metadata()
            .map_err(|error| format!("stat opened workspace file: {error}"))?;
        Ok(file_system_revision_from_metadata(&metadata))
    }
    #[cfg(windows)]
    {
        backend_platform::win32::project_fs::revision_for_file(file)
            .map(file_system_revision_from_windows)
            .map_err(|error| format!("stat opened workspace file: {error}"))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = file
            .metadata()
            .map_err(|error| format!("stat opened workspace file: {error}"))?;
        Ok(file_system_revision_from_metadata(&metadata))
    }
}

fn supported_paths(root: &Path, frontends: &FrontendSet) -> Result<Vec<PathBuf>, String> {
    supported_paths_with_policy(root, frontends, source_selection_policy())
}

fn supported_paths_with_policy(
    root: &Path,
    frontends: &FrontendSet,
    discovery: DiscoveryPolicy,
) -> Result<Vec<PathBuf>, String> {
    let root_capability = ProjectRoot::open(root)?;
    Ok(
        project_paths_with_policy(root, &root_capability, Some(frontends), false, discovery, None)?
            .sources,
    )
}

struct ProjectPaths {
    sources: Vec<PathBuf>,
    // Direct native TS candidates from the same discovery walk. They are not
    // structural declarations or proof that the compiler includes the file.
    native_typescript_paths: Vec<PathBuf>,
    // Captured from the same nonignored walk; unsupported disclosure remains
    // in force until native Stylo facts can actually be published.
    stylo_paths: Vec<PathBuf>,
    vue_paths: Vec<PathBuf>,
    vue_inventory_complete: bool,
    stylo_inventory_complete: bool,
    // None means even the bounded full-walk path witness could not be formed.
    stylo_inventory: Option<(u64, [u8; 32])>,
    sql_paths: Vec<PathBuf>,
    sql_inventory_complete: bool,
    unsupported_css: u64,
    unsupported_sql: u64,
    unsupported_vue: u64,
    configurations: Vec<(Language, PathBuf)>,
    directories: Vec<(PathBuf, Option<FileSystemRevision>)>,
    file_revisions: BTreeMap<PathBuf, Option<FileSystemRevision>>,
    incomplete_configurations: BTreeSet<Language>,
    go_absent_modules: Vec<PathBuf>,
}

fn project_paths_with_policy(
    root: &Path,
    _root_capability: &ProjectRoot,
    frontends: Option<&FrontendSet>,
    native_typescript: bool,
    discovery: DiscoveryPolicy,
    cancellation: Option<&AtomicBool>,
) -> Result<ProjectPaths, String> {
    if native_typescript && frontends.is_some() {
        return Err("native TypeScript discovery cannot use a syntax frontend registry".into());
    }
    let mut sources = Vec::new();
    let mut native_typescript_paths = Vec::new();
    let mut stylo_paths = Vec::new();
    let mut sql_paths = Vec::new();
    let mut sql_inventory_complete = true;
    let mut vue_paths = Vec::new();
    let mut vue_inventory_complete = true;
    let mut stylo_inventory_complete = true;
    let mut stylo_witness_paths = Vec::<String>::new();
    let mut stylo_witness_bytes = 0usize;
    let mut stylo_witness_available = true;
    let mut unsupported_css = 0_u64;
    let mut unsupported_sql = 0_u64;
    let mut unsupported_vue = 0_u64;
    let mut configurations = Vec::new();
    #[cfg(windows)]
    let root_revision = Some(_root_capability.revision()?);
    #[cfg(not(windows))]
    let root_revision = file_system_revision(root);
    let mut directories = vec![(PathBuf::new(), root_revision)];
    let mut file_revisions = BTreeMap::new();
    let mut configuration_counts = BTreeMap::<Language, usize>::new();
    let mut incomplete_configurations = BTreeSet::new();
    let mut budget = DiscoveryBudget::default();
    let mut current_directory = None;
    let mut directory_entries = 0_usize;
    for entry in discover_source_entries(root, discovery) {
        check_scan_cancellation(cancellation)?;
        let entry = entry.map_err(|error| error.to_string())?;
        if entry.path() == root {
            continue;
        }
        let parent = entry.path().parent().unwrap_or(root);
        if current_directory.as_deref() != Some(parent) {
            current_directory = Some(parent.to_owned());
            directory_entries = 0;
        }
        budget
            .admit_entry(directory_entries)
            .map_err(|error| format!("{error}: {}", entry.path().display()))?;
        directory_entries = directory_entries.saturating_add(1);
        if entry.is_file() {
            // Count only recognized unsupported source containers from this
            // same nonignored walk. Do not read them, assign TS authority, or
            // insert them into supported/compiler membership.
            let claimed = frontends.and_then(|set| set.for_path(entry.path()));
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|_| "discovered file escaped its project root".to_owned())?;
            let exact_relative = relative.to_str();
            let known_extension = entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase);
            if native_typescript
                && exact_relative.is_none()
                && known_extension.as_deref().is_some_and(|ext| {
                    matches!(
                        ext,
                        "ts" | "tsx"
                            | "mts"
                            | "cts"
                            | "js"
                            | "jsx"
                            | "mjs"
                            | "cjs"
                            | "rs"
                            | "py"
                            | "pyi"
                            | "pyw"
                            | "go"
                            | "java"
                            | "cs"
                            | "c"
                            | "h"
                            | "cc"
                            | "cpp"
                            | "cxx"
                            | "hh"
                            | "hpp"
                            | "hxx"
                            | "m"
                            | "mm"
                    )
                })
            {
                return Err("native discovery source path is not exact UTF-8".to_owned());
            }
            let native_ts = native_typescript
                && exact_relative.is_some_and(|path| {
                    admitted_file_profile(SourceLanguage::TypeScript, path).is_some()
                });
            let native_other = native_typescript
                && exact_relative.is_some_and(|path| {
                    [
                        SourceLanguage::Rust,
                        SourceLanguage::Python,
                        SourceLanguage::Go,
                        SourceLanguage::Java,
                        SourceLanguage::CSharp,
                        SourceLanguage::Clang,
                    ]
                    .into_iter()
                    .any(|language| admitted_file_profile(language, path).is_some())
                });
            let recognized = claimed.is_some() || native_ts || native_other;
            if (frontends.is_some() || native_typescript) && !recognized {
                let ext = entry.path().extension().and_then(|ext| ext.to_str());
                let counter = match ext.map(str::to_ascii_lowercase).as_deref() {
                    Some("css") => Some(&mut unsupported_css),
                    Some("sql") => Some(&mut unsupported_sql),
                    Some("vue") => Some(&mut unsupported_vue),
                    _ => None,
                };
                if let Some(counter) = counter {
                    *counter = counter
                        .checked_add(1)
                        .ok_or("unsupported source count overflow")?;
                }
            }
            let is_vue_source = (frontends.is_some() || native_typescript)
                && !recognized
                && entry.path().extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("vue"));
            if is_vue_source {
                if exact_relative.is_none() || vue_paths.len() >= ProductSourceRecord::MAX_PROJECT_FILES {
                    vue_inventory_complete = false;
                } else {
                    vue_paths.push(entry.path().to_owned());
                }
            }
            let is_stylo_source = (frontends.is_some() || native_typescript)
                && !recognized
                && entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("css"));
            // The static discovery walk carries no frontends but is flagged native;
            // like CSS, SQL is collected by both the static and the legacy walk.
            let is_sql_source = (frontends.is_some() || native_typescript) && !recognized
                && entry.path().extension().and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("sql"));
            if is_sql_source {
                if sql_paths.len() < backend_engine::builtin::MAX_SQL_HOSTS as usize {
                    sql_paths.push(entry.path().to_owned());
                } else { sql_inventory_complete = false; }
            }
            if is_stylo_source {
                if stylo_witness_available {
                    let relative = entry
                        .path()
                        .strip_prefix(root)
                        .ok()
                        .and_then(Path::to_str)
                        .map(|value| value.replace(std::path::MAIN_SEPARATOR, "/"));
                    match relative.and_then(|path| {
                        stylo_witness_bytes
                            .checked_add(path.len())
                            .map(|total| (path, total))
                    }) {
                        Some((path, total)) if total <= 16 * 1024 * 1024 => {
                            stylo_witness_bytes = total;
                            stylo_witness_paths.push(path);
                        }
                        _ => {
                            stylo_witness_available = false;
                            stylo_inventory_complete = false;
                            stylo_witness_paths.clear();
                        }
                    }
                }
                if stylo_paths.len() < ProductSourceRecord::MAX_PROJECT_FILES {
                    stylo_paths.push(entry.path().to_owned());
                } else {
                    stylo_inventory_complete = false;
                }
            }
            let mut revision_path = entry
                .path()
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(source_frontier::is_policy_file_name);
            if is_stylo_source || is_sql_source || is_vue_source {
                revision_path = true;
            }
            if recognized {
                if sources.len().saturating_add(native_typescript_paths.len())
                    >= ProductSourceRecord::MAX_PROJECT_FILES
                {
                    return Err("project contains too many supported source files".to_owned());
                }
                if native_ts {
                    native_typescript_paths.push(entry.path().to_owned());
                } else {
                    sources.push(entry.path().to_owned());
                }
                revision_path = true;
            }
            if let Some(language) = compiler_configuration_language(entry.path()) {
                revision_path = true;
                let count = configuration_counts.entry(language).or_default();
                if *count >= MAX_COMPILER_CONFIGURATION_FILES_PER_LANGUAGE {
                    incomplete_configurations.insert(language);
                } else {
                    configurations.push((language, entry.path().to_owned()));
                }
                *count = count.saturating_add(1);
            }
            if revision_path {
                let _relative = entry
                    .path()
                    .strip_prefix(root)
                    .map_err(|_| "discovered file escaped its project root".to_owned())?;
                #[cfg(windows)]
                let revision = _root_capability.revision_relative(_relative).ok();
                #[cfg(not(windows))]
                let revision = file_system_revision(entry.path());
                file_revisions.insert(entry.path().to_owned(), revision);
            }
        } else if entry.is_directory() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|_| "discovered directory escaped its project root".to_owned())?
                .to_path_buf();
            #[cfg(windows)]
            let revision = _root_capability.revision_relative(&relative).ok();
            #[cfg(not(windows))]
            let revision = file_system_revision(entry.path());
            directories.push((relative, revision));
        }
    }
    // Go consumes module files even when ignore rules hide them from ordinary
    // source discovery. Admit each unique ancestor candidate once, with a hard
    // count bound. Missing candidates are witnessed by their already captured
    // parent directory revisions; existing candidates enter the same bounded
    // configuration byte reader and final file fence as visible go.mod files.
    let mut go_candidates = BTreeSet::new();
    for source in &sources {
        check_scan_cancellation(cancellation)?;
        let native_go = native_typescript && source.strip_prefix(root).ok().and_then(Path::to_str)
            .is_some_and(|path| admitted_file_profile(SourceLanguage::Go, path).is_some());
        if !native_go && !frontends
            .and_then(|frontends| frontends.for_path(source))
            .is_some_and(|frontend| frontend.language() == SourceLanguage::Go)
        {
            continue;
        }
        let mut directory = source.parent();
        while let Some(parent) = directory.filter(|parent| parent.starts_with(root)) {
            if !go_candidates.insert(parent.join("go.mod")) {
                break; // This directory's entire ancestor chain was already admitted.
            }
            if go_candidates.len() > MAX_COMPILER_CONFIGURATION_FILES_PER_LANGUAGE {
                return Err(
                    "Go module candidate count exceeds bounded configuration capture".to_owned(),
                );
            }
            directory = parent.parent();
        }
    }
    let captured_directories = directories
        .iter()
        .map(|(path, _)| path.clone())
        .collect::<BTreeSet<_>>();
    let mut go_absent_modules = Vec::new();
    for candidate in go_candidates {
        if configurations.iter().any(|(_, path)| path == &candidate) {
            continue;
        }
        let relative = candidate
            .strip_prefix(root)
            .map_err(|_| "Go module candidate escaped root")?;
        if !relative
            .parent()
            .is_some_and(|parent| captured_directories.contains(parent))
        {
            return Err("Go module absence has no captured directory fence".to_owned());
        }
        if let Some(revision) =
            _root_capability.compiler_configuration_revision_or_absent(relative)?
        {
            let count = configuration_counts.entry(Language::Go).or_default();
            if *count >= MAX_COMPILER_CONFIGURATION_FILES_PER_LANGUAGE {
                return Err("Go module file count exceeds bounded configuration capture".to_owned());
            }
            *count += 1;
            file_revisions.insert(candidate.clone(), Some(revision));
            configurations.push((Language::Go, candidate));
        } else {
            go_absent_modules.push(
                relative
                    .parent()
                    .ok_or("Go module candidate parent")?
                    .to_path_buf(),
            );
        }
    }
    let stylo_inventory = if stylo_witness_available {
        stylo_witness_paths.sort_unstable();
        let mut witness = backend_engine::builtin::StyloInventoryWitness::default();
        if stylo_witness_paths
            .iter()
            .all(|path| witness.push(path).is_ok())
        {
            Some(witness.finish())
        } else {
            stylo_inventory_complete = false;
            None
        }
    } else {
        None
    };
    Ok(ProjectPaths {
        sources,
        native_typescript_paths,
        stylo_paths,
        vue_paths,
        vue_inventory_complete,
        stylo_inventory_complete,
        stylo_inventory,
        sql_paths,
        sql_inventory_complete,
        unsupported_css,
        unsupported_sql,
        unsupported_vue,
        configurations,
        directories,
        file_revisions,
        incomplete_configurations,
        go_absent_modules,
    })
}

fn read_compiler_configuration_snapshot(
    root: &Path,
    root_capability: &ProjectRoot,
    configurations: Vec<(Language, PathBuf)>,
    incomplete: BTreeSet<Language>,
    relevant_languages: BTreeSet<Language>,
    cancellation: Option<&AtomicBool>,
) -> Result<CompilerConfigurationSnapshot, String> {
    let mut complete_languages = relevant_languages
        .into_iter()
        .filter(|language| !incomplete.contains(language))
        .collect::<BTreeSet<_>>();
    let mut used_bytes = BTreeMap::<Language, usize>::new();
    let mut files = Vec::with_capacity(configurations.len());
    let mut go_capture = GoProjectCapture::default();
    for (language, path) in configurations {
        check_scan_cancellation(cancellation)?;
        if !complete_languages.contains(&language) {
            continue;
        }
        let relative_path = path
            .strip_prefix(root)
            .map_err(|_| "compiler configuration escaped its project root".to_owned())?;
        let used = used_bytes.get(&language).copied().unwrap_or_default();
        let remaining = MAX_COMPILER_CONFIGURATION_BYTES_PER_LANGUAGE.saturating_sub(used);
        let maximum = MAX_COMPILER_CONFIGURATION_FILE_BYTES.min(remaining);
        let Ok(bytes) = root_capability.read_compiler_configuration(relative_path, maximum) else {
            complete_languages.remove(&language);
            continue;
        };
        *used_bytes.entry(language).or_default() = used.saturating_add(bytes.len());
        let content = typed_of::<InputContentSchema>(&bytes).to_bytes();
        if language == Language::Go
            && relative_path
                .file_name()
                .is_some_and(|name| name == "go.mod")
        {
            go_capture.admit(relative_path, &bytes, content)?;
        }
        files.push(CompilerConfigurationFile {
            language,
            relative_path: relative_path.to_path_buf(),
            content,
        });
    }
    files.sort_by(|left, right| {
        (left.language, &left.relative_path).cmp(&(right.language, &right.relative_path))
    });
    Ok(CompilerConfigurationSnapshot {
        files,
        complete_languages,
        go_capture,
    })
}

fn compiler_languages() -> [Language; 7] {
    [
        Language::Rust,
        Language::TypeScript,
        Language::Python,
        Language::Go,
        Language::Java,
        Language::CSharp,
        Language::Clang,
    ]
}

fn compiler_configuration_language(path: &Path) -> Option<Language> {
    let name = path.file_name()?.to_str()?;
    if matches!(
        name,
        "Cargo.toml" | "Cargo.lock" | "rust-toolchain" | "rust-toolchain.toml" | "build.rs"
    ) || matches!(name, "config" | "config.toml")
        && path
            .parent()
            .is_some_and(|parent| parent.ends_with(".cargo"))
    {
        return Some(Language::Rust);
    }
    if matches!(
        name,
        "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "bun.lock"
            | "bun.lockb"
            | ".npmrc"
            | ".yarnrc.yml"
            | "deno.json"
            | "deno.jsonc"
    ) || ["tsconfig", "jsconfig"]
        .iter()
        .any(|prefix| name.starts_with(prefix) && name.ends_with(".json"))
        || [
            "vite.config.",
            "webpack.config.",
            "rollup.config.",
            "esbuild.config.",
            "babel.config.",
            "jest.config.",
            "vitest.config.",
        ]
        .iter()
        .any(|prefix| name.starts_with(prefix))
        || name.starts_with(".babelrc")
    {
        return Some(Language::TypeScript);
    }
    if matches!(
        name,
        "pyproject.toml"
            | "Pipfile"
            | "Pipfile.lock"
            | "poetry.lock"
            | "uv.lock"
            | "setup.cfg"
            | "setup.py"
            | "tox.ini"
            | "pyrefly.toml"
            | "pyrightconfig.json"
            | "mypy.ini"
    ) || name.starts_with("requirements") && (name.ends_with(".txt") || name.ends_with(".in"))
    {
        return Some(Language::Python);
    }
    if matches!(name, "go.mod" | "go.sum" | "go.work" | "go.work.sum")
        || name == "modules.txt"
            && path
                .parent()
                .is_some_and(|parent| parent.ends_with("vendor"))
    {
        return Some(Language::Go);
    }
    if matches!(
        name,
        "pom.xml"
            | "build.gradle"
            | "build.gradle.kts"
            | "settings.gradle"
            | "settings.gradle.kts"
            | "gradle.lockfile"
            | "gradle.properties"
            | "gradle-wrapper.properties"
    ) {
        return Some(Language::Java);
    }
    let lower = name.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "directory.build.props"
            | "directory.build.targets"
            | "directory.packages.props"
            | "packages.lock.json"
            | "nuget.config"
            | "global.json"
    ) || lower.ends_with(".csproj")
        || lower.ends_with(".sln")
        || lower.ends_with(".slnx")
    {
        return Some(Language::CSharp);
    }
    if matches!(
        name,
        "compile_commands.json"
            | "compile_flags.txt"
            | ".clang-tidy"
            | "CMakeLists.txt"
            | "Makefile"
    ) {
        return Some(Language::Clang);
    }
    None
}

/// Charges the aggregate source budget before any file is read.
///
/// A file that is oversized or unreadable is not a reason to refuse the
/// project: it is charged as zero bytes here and reported per file by
/// [`scan_file`].  Only the aggregate budget, which protects the process
/// rather than describing a file, can still stop the scan.
fn preflight_source_bytes(
    root: &Path,
    paths: &[PathBuf],
    source_policy: SourceAdmissionPolicy,
    cancellation: Option<&AtomicBool>,
) -> Result<(), String> {
    let mut total = 0_usize;
    let mut largest = Vec::new();
    for path in paths {
        check_scan_cancellation(cancellation)?;
        let Ok(metadata) = fs::metadata(path) else {
            continue;
        };
        let Ok(length) = usize::try_from(metadata.len()) else {
            continue;
        };
        if length > source_policy.limits().max_file_source_bytes {
            continue;
        }
        // Bounded diagnostics describe only the supported-source candidates
        // charged below, never arbitrary tracked data or compiler resources.
        largest.push((length, path));
        largest.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| left.1.cmp(right.1)));
        largest.truncate(3);
        total = total
            .checked_add(length)
            .ok_or_else(|| "source byte count overflow".to_owned())?;
    }
    source_policy
        .admit_preflight(total, paths.len())
        .map_err(|refusal| {
            let contributors = largest
                .iter()
                .map(|(bytes, path)| {
                    let relative = path.strip_prefix(root).unwrap_or(path);
                    let label = relative
                        .to_string_lossy()
                        .chars()
                        .map(|character| if character.is_control() { '�' } else { character })
                        .take(160)
                        .collect::<String>();
                    format!("{label} ({bytes} bytes)")
                })
                .collect::<Vec<_>>()
                .join(", ");
            let setting = if total > source_policy.limits().max_project_source_bytes {
                "NUDOX_SOURCE_MAX_PROJECT_BYTES"
            } else {
                "NUDOX_SOURCE_MAX_PROJECT_RECORDS"
            };
            format!(
                "{refusal}; selected {} supported-source candidates totaling {total} charged bytes; \
                largest: {contributors}. Scope indexing with .nudoxignore (Git-ignore syntax), \
                or configure {setting} for the owner before starting it. \
                Compiler manifests and resource bindings are validated separately.",
                paths.len()
            )
        })
}

/// Scans one discovered file into a row, or into a typed per-file fault.
///
/// `Ok(None)` means the file vanished between discovery and read, so the
/// project no longer contains it and the frontier simply omits it.
fn scan_file(
    root: &Path,
    root_capability: &ProjectRoot,
    path: &Path,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
    python_version: PythonVersion,
    go_capture: &GoProjectCapture,
    source_policy: SourceAdmissionPolicy,
) -> Result<Option<ScannedFile>, String> {
    match scan_one(
        root,
        root_capability,
        path,
        project,
        reusable,
        frontends,
        python_version,
        go_capture,
        source_policy,
    ) {
        Ok(file) => Ok(Some(file)),
        Err(SourceFault::Vanished) => Ok(None),
        Err(SourceFault::Fatal(message)) => {
            Err(format!("read source {}: {message}", path.display()))
        }
        Err(SourceFault::Unavailable(reason)) => unavailable_file(
            root,
            path,
            project,
            frontends,
            python_version,
            go_capture,
            reason,
            0,
        )
        .map(Some),
        Err(SourceFault::UnavailableRead(reason, bytes_read)) => unavailable_file(
            root,
            path,
            project,
            frontends,
            python_version,
            go_capture,
            reason,
            bytes_read,
        )
        .map(Some),
    }
}

/// Builds the row for a file whose contents could not be extracted.
fn unavailable_file(
    root: &Path,
    path: &Path,
    project: [u8; 32],
    frontends: &FrontendSet,
    python_version: PythonVersion,
    go_capture: &GoProjectCapture,
    reason: SourceUnavailableReason,
    source_bytes_read: usize,
) -> Result<ScannedFile, String> {
    let relative = relative_coordinate(root, path)?;
    let frontend = frontends
        .for_path(path)
        .ok_or_else(|| "unsupported source language".to_owned())?;
    let key = product_source_file_key(project, &relative);
    let record = ProductSourceRecord::profiled_file_unavailable(
        project,
        relative.clone(),
        frontend.language(),
        project_analysis_version(frontend, python_version, go_capture, Path::new(&relative)),
        reason,
        (frontend.language() == SourceLanguage::Python).then_some(python_version),
    )?
    .with_go_version(if frontend.language() == SourceLanguage::Go {
        go_capture.literal_version(Path::new(&relative))
    } else {
        None
    })?;
    let mut encoded = Vec::new();
    ProductSourceRelation::encode_value(&record, &mut encoded);
    let native_frontier = profile_of(frontends, path)
        .filter(|profile| {
            matches!(
                profile,
                LanguageProfile::C(_) | LanguageProfile::Cxx(_) | LanguageProfile::CSharp(_)
            )
        })
        .map(|profile| {
            let refusal = match reason {
                SourceUnavailableReason::Unreadable => NativePathRefusal::ReadUnavailable,
                SourceUnavailableReason::NotText => NativePathRefusal::InvalidUtf8,
                SourceUnavailableReason::TooLarge => NativePathRefusal::Oversized,
                SourceUnavailableReason::Unparsed => NativePathRefusal::MalformedOther,
            };
            (profile, NativePathDisposition::Refused(refusal))
        });
    Ok(ScannedFile {
        // An unavailable source must not be turned into an empty compiler
        // artifact.  The relation row carries the typed terminal and the
        // semantic lane must retain partial coverage until a later scan can
        // read the real bytes.
        compiler_source: None,
        reused_compiler: None,
        relative,
        key,
        record,
        source_bytes: 0,
        source_bytes_read,
        encoded_record_bytes: encoded.len(),
        encoded_fact_bytes: 0,
        source_facts: None,
        native_frontier,
        native_clang_candidate: None,
        native_csharp_candidate: None,
    })
}

fn relative_coordinate(root: &Path, path: &Path) -> Result<String, String> {
    Ok(path
        .strip_prefix(root)
        .map_err(|_| "source path escaped its project root".to_owned())?
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/"))
}

fn scan_one(
    root: &Path,
    root_capability: &ProjectRoot,
    path: &Path,
    project: [u8; 32],
    reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
    frontends: &FrontendSet,
    python_version: PythonVersion,
    go_capture: &GoProjectCapture,
    source_policy: SourceAdmissionPolicy,
) -> Result<ScannedFile, SourceFault> {
    let relative_path = path
        .strip_prefix(root)
        .map_err(|_| SourceFault::Fatal("source path escaped its project root".to_owned()))?;
    let bytes =
        root_capability.read(relative_path, source_policy.limits().max_file_source_bytes)?;
    let source_bytes = bytes.len();
    source_policy.admit_actual_file(source_bytes).map_err(|_| {
        SourceFault::UnavailableRead(SourceUnavailableReason::TooLarge, source_bytes)
    })?;
    let relative = relative_path
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/");
    let frontend = frontends
        .for_path(path)
        .ok_or_else(|| SourceFault::Fatal("unsupported source language".to_owned()))?;
    let go_version = if frontend.language() == SourceLanguage::Go {
        go_capture.literal_version(relative_path)
    } else {
        None
    };
    let profile = match project_profile_fault(frontends, path, python_version, go_version) {
        Ok(profile) => profile,
        Err(SourceFault::Unavailable(reason)) => {
            return Err(SourceFault::UnavailableRead(reason, bytes.len()));
        }
        Err(error) => return Err(error),
    };
    let key = product_source_file_key(project, &relative);
    let content = typed_of::<InputContentSchema>(&bytes).to_bytes();
    let analysis =
        project_analysis_version(frontend, python_version, go_capture, Path::new(&relative));
    let reusable_record = reusable.get(&key).filter(|record| {
        record.file_fields().is_some_and(|fields| {
            fields.project == project
                && fields.path == relative
                && fields.language == frontend.language()
                && fields.content_version == content
                // An unavailable row stores a zero content version because its
                // bytes were never admitted. A real digest is never that value,
                // and this fence keeps such a row from becoming compiler input.
                && fields.content_version != [0; 32]
                && fields.python_version == (frontend.language() == SourceLanguage::Python).then_some(python_version)
                && fields.go_version == go_version
                && fields.analysis_version == analysis
                && fields.source_identity.is_some()
        })
    });

    // Move the bounded read buffer into its UTF-8 owner to avoid a second
    // per-file source allocation. The scan-wide source, retained-text, row,
    // and in-flight budgets are separate policy limits.
    let source = match String::from_utf8(bytes) {
        Ok(source) => source,
        Err(error) if matches!(profile, LanguageProfile::Python(_)) => {
            // Raw Python resources remain in the independent compiler frontier.
            // The browsing row stays NotText; native intake records the exact
            // codec/byte refusal without manufacturing replacement source.
            let mut unavailable = unavailable_file(
                root,
                path,
                project,
                frontends,
                python_version,
                go_capture,
                SourceUnavailableReason::NotText,
                source_bytes,
            )
            .map_err(SourceFault::Fatal)?;
            unavailable.compiler_source = Some(CompilerSourceHandle {
                relative_path: relative,
                profile,
                content,
                source_fact_identity: SourceFactIdentity::from_canonical_bytes(error.as_bytes()),
                expected_byte_length: None,
            });
            unavailable.source_bytes = source_bytes;
            return Ok(unavailable);
        }
        Err(error) => {
            return Err(SourceFault::UnavailableRead(
                SourceUnavailableReason::NotText,
                error.as_bytes().len(),
            ));
        }
    };
    // Structural parsing is an explicit baseline projection for local browsing.
    // Package semantics are compiled and published by the engine application module.
    let analyzed = match frontend
        .baseline
        .analyze(Path::new(&relative), source.as_bytes())
    {
        Ok(analyzed) => analyzed,
        Err(error)
            if matches!(
                profile,
                LanguageProfile::Python(_) | LanguageProfile::C(_) | LanguageProfile::Cxx(_)
            ) && !matches!(&error, SyntaxError::MalformedSource(SourceLanguage::Clang)) =>
        {
            // Browsing keeps the truthful Unparsed row. A structural grammar
            // cannot decide whether the native compiler accepts this source:
            // C/C++ declaration macros in particular require preprocessing.
            // Preserve the exact compiler input so native authority can publish
            // its declarations or record its own diagnostic/coverage gap.
            let mut unavailable = unavailable_file(
                root,
                path,
                project,
                frontends,
                python_version,
                go_capture,
                SourceUnavailableReason::Unparsed,
                source_bytes,
            )
            .map_err(SourceFault::Fatal)?;
            unavailable.compiler_source = Some(CompilerSourceHandle {
                relative_path: relative,
                profile,
                content,
                source_fact_identity: SourceFactIdentity::from_canonical_bytes(source.as_bytes()),
                expected_byte_length: None,
            });
            unavailable.source_bytes = source_bytes;
            return Ok(unavailable);
        }
        Err(SyntaxError::MalformedSource(SourceLanguage::CSharp))
            if matches!(profile, LanguageProfile::CSharp(_)) =>
        {
            let mut unavailable = unavailable_file(
                root,
                path,
                project,
                frontends,
                python_version,
                go_capture,
                SourceUnavailableReason::Unparsed,
                source_bytes,
            )
            .map_err(SourceFault::Fatal)?;
            let source_fact_identity = SourceFactIdentity::from_canonical_bytes(source.as_bytes());
            unavailable.native_frontier = Some((
                profile,
                NativePathDisposition::CSharpMalformed {
                    content,
                    source_identity: source_fact_identity,
                    byte_length: u32::try_from(source_bytes).map_err(|_| {
                        SourceFault::Fatal("native source length exceeds u32".to_owned())
                    })?,
                    chunk_count: u32::try_from(source_bytes.div_ceil(NATIVE_SOURCE_CHUNK_BYTES))
                        .map_err(|_| {
                            SourceFault::Fatal("native source chunk count exceeds u32".to_owned())
                        })?,
                },
            ));
            unavailable.native_csharp_candidate = Some(NativeCSharpCandidateHandle {
                relative_path: relative,
                profile,
                content,
                source_fact_identity,
                source_bytes: Some(source.into_bytes().into_boxed_slice()),
            });
            unavailable.source_bytes = source_bytes;
            return Ok(unavailable);
        }
        Err(SyntaxError::MalformedSource(SourceLanguage::Clang))
            if matches!(profile, LanguageProfile::C(_) | LanguageProfile::Cxx(_)) =>
        {
            let mut unavailable = unavailable_file(
                root,
                path,
                project,
                frontends,
                PythonVersion::Python314,
                go_capture,
                SourceUnavailableReason::Unparsed,
                source_bytes,
            )
            .map_err(SourceFault::Fatal)?;
            let source_fact_identity = SourceFactIdentity::from_canonical_bytes(source.as_bytes());
            unavailable.native_frontier = Some((
                profile,
                NativePathDisposition::ClangMalformed {
                    content,
                    source_identity: source_fact_identity,
                    byte_length: u32::try_from(source_bytes).map_err(|_| {
                        SourceFault::Fatal("native source length exceeds u32".to_owned())
                    })?,
                    chunk_count: u32::try_from(source_bytes.div_ceil(NATIVE_SOURCE_CHUNK_BYTES))
                        .map_err(|_| {
                            SourceFault::Fatal("native source chunk count exceeds u32".to_owned())
                        })?,
                },
            ));
            unavailable.native_clang_candidate = Some(NativeClangCandidateHandle {
                relative_path: relative,
                profile,
                content,
                source_fact_identity,
                source_bytes: Some(source.into_bytes().into_boxed_slice()),
            });
            unavailable.source_bytes = source_bytes;
            return Ok(unavailable);
        }
        Err(_) if matches!(profile, LanguageProfile::C(_) | LanguageProfile::Cxx(_)) => {
            let mut unavailable = unavailable_file(
                root,
                path,
                project,
                frontends,
                PythonVersion::Python314,
                go_capture,
                SourceUnavailableReason::Unparsed,
                source_bytes,
            )
            .map_err(SourceFault::Fatal)?;
            unavailable.native_frontier = Some((
                profile,
                NativePathDisposition::Refused(NativePathRefusal::MalformedOther),
            ));
            unavailable.source_bytes = source_bytes;
            return Ok(unavailable);
        }
        Err(_) => {
            return Err(SourceFault::UnavailableRead(
                SourceUnavailableReason::Unparsed,
                source_bytes,
            ));
        }
    };
    debug_assert_eq!(analyzed.language(), frontend.language());
    debug_assert_eq!(analyzed.content().to_bytes(), content);
    // Facts are produced from the full frontend result before the compact
    // source relation applies its bounded-row retention policy. A reused
    // compact source row is never treated as a complete facts source.
    let source_fact_identity = SourceFactIdentity::from_canonical_bytes(source.as_bytes());
    let source_facts = build_product_source_file_facts(
        project,
        relative.clone(),
        analyzed.language(),
        analyzed.content().to_bytes(),
        analysis,
        source_fact_identity,
        analyzed.declarations(),
    )
    .map_err(SourceFault::Fatal)?;
    // A real source file routinely extracts more detail than one canonical
    // relation row can carry: `memchr 2.8.3` alone produces a 65 686 byte row
    // for `src/arch/x86_64/avx2/memchr.rs` against a 65 464 byte capacity.
    // Constructing through the capacity-aware path sheds derived detail in a
    // fixed order and records how far it had to go, instead of failing the
    // whole project when the relation delta is later prepared.
    let record = if let Some(record) = reusable_record {
        record.clone()
    } else if let Some(go_version) = go_version {
        ProductSourceRecord::go_profiled_file_within_row_capacity(
            project,
            relative.clone(),
            analyzed.content().to_bytes(),
            analysis,
            analyzed.declarations().clone(),
            Some(source_fact_identity),
            go_version,
        )
        .map_err(SourceFault::Fatal)?
    } else {
        // Bind identity before compaction: its final wire format also encodes
        // containment per declaration, so reserving only a header is insufficient.
        ProductSourceRecord::profiled_file_within_row_capacity(
            project,
            relative.clone(),
            analyzed.language(),
            analyzed.content().to_bytes(),
            analysis,
            analyzed.declarations().clone(),
            source_fact_identity,
            (frontend.language() == SourceLanguage::Python).then_some(python_version),
        )
        .map_err(SourceFault::Fatal)?
    };
    if reusable_record.is_some() {
        let mut scanned = reused_scanned_file(
            relative,
            key,
            record,
            source_bytes,
            source_bytes,
            content,
            profile,
            source_fact_identity,
            None,
        )
        .map_err(SourceFault::Fatal)?;
        scanned.encoded_fact_bytes = source_facts.encoded_bytes();
        scanned.source_facts = Some(source_facts);
        return Ok(scanned);
    }
    scanned_file(
        relative,
        key,
        record,
        source_facts,
        source_bytes,
        source_bytes,
        path,
        profile,
        content,
        source_fact_identity,
    )
    .map_err(SourceFault::Fatal)
}

fn scanned_file(
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    source_facts: ProductSourceFileFactsUpdate,
    source_bytes: usize,
    source_bytes_read: usize,
    path: &Path,
    profile: LanguageProfile,
    content: [u8; 32],
    source_fact_identity: SourceFactIdentity,
) -> Result<ScannedFile, String> {
    let mut scanned = finish_scanned_file(
        relative.clone(),
        key,
        record,
        source_bytes,
        source_bytes_read,
        path,
        Some(CompilerSourceHandle {
            profile,
            relative_path: relative,
            content,
            source_fact_identity,
            expected_byte_length: None,
        }),
        None,
        None,
        Some(source_facts),
    )?;
    if matches!(
        profile,
        LanguageProfile::C(_) | LanguageProfile::Cxx(_) | LanguageProfile::CSharp(_)
    ) {
        scanned.native_frontier = Some((
            profile,
            NativePathDisposition::Structural {
                content,
                source_identity: source_fact_identity,
            },
        ));
    }
    Ok(scanned)
}

fn reused_scanned_file(
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    source_bytes: usize,
    source_bytes_read: usize,
    content: [u8; 32],
    profile: LanguageProfile,
    source_fact_identity: SourceFactIdentity,
    encoded_record_bytes: Option<usize>,
) -> Result<ScannedFile, String> {
    let path = PathBuf::from(&relative);
    let mut scanned = finish_scanned_file(
        relative.clone(),
        key,
        record,
        source_bytes,
        source_bytes_read,
        &path,
        None,
        Some(ReusedCompilerFile {
            profile,
            relative_path: relative,
            content,
            source_fact_identity,
        }),
        encoded_record_bytes,
        None,
    )?;
    if matches!(
        profile,
        LanguageProfile::C(_) | LanguageProfile::Cxx(_) | LanguageProfile::CSharp(_)
    ) {
        scanned.native_frontier = Some((
            profile,
            NativePathDisposition::Structural {
                content,
                source_identity: source_fact_identity,
            },
        ));
    }
    Ok(scanned)
}

fn finish_scanned_file(
    relative: String,
    key: [u8; 32],
    record: ProductSourceRecord,
    source_bytes: usize,
    source_bytes_read: usize,
    path: &Path,
    compiler_source: Option<CompilerSourceHandle>,
    reused_compiler: Option<ReusedCompilerFile>,
    known_encoded_record_bytes: Option<usize>,
    source_facts: Option<ProductSourceFileFactsUpdate>,
) -> Result<ScannedFile, String> {
    let encoded_record_bytes = if let Some(size) = known_encoded_record_bytes {
        size
    } else {
        let mut encoded = Vec::new();
        ProductSourceRelation::encode_value(&record, &mut encoded);
        encoded.len()
    };
    if encoded_record_bytes > MAX_ENCODED_RECORD_BYTES {
        return Err(format!(
            "source projection {} exceeds the {} byte record limit",
            path.display(),
            MAX_ENCODED_RECORD_BYTES
        ));
    }
    Ok(ScannedFile {
        compiler_source,
        reused_compiler,
        relative,
        key,
        record,
        source_bytes,
        source_bytes_read,
        encoded_record_bytes,
        encoded_fact_bytes: source_facts
            .as_ref()
            .map_or(0, ProductSourceFileFactsUpdate::encoded_bytes),
        source_facts,
        native_frontier: None,
        native_clang_candidate: None,
        native_csharp_candidate: None,
    })
}

/// Reopens fresh and reused source handles through [`ProjectRoot`]. Both paths
/// must still hash to the identity admitted during scanning before exact byte
/// owners enter the compiler boundary. Non-Python sources must remain UTF-8. The immutable policy
/// comes from the same scan that created these handles; it is never re-read
/// from the environment midway through an index attempt.
pub(super) fn admit_compiler_sources_for_scan(
    root: &Path,
    revision_fence: &CompilerRevisionFence,
    fresh: Vec<CompilerSourceHandle>,
    reused: Vec<ReusedCompilerFile>,
    source_policy: SourceAdmissionPolicy,
    cancellation: Option<&AtomicBool>,
) -> Result<Vec<CompilerSource>, String> {
    check_scan_cancellation(cancellation)?;
    let capability = revision_fence.open_bound_root(root)?;
    let admitted = materialize_compiler_sources(
        &capability,
        fresh,
        reused,
        source_policy,
        cancellation,
        |_| {},
    )?;
    if !compiler_revision_is_current_with_cancellation(revision_fence, cancellation)? {
        return Err(
            "project source root changed during compiler-source materialization".to_owned(),
        );
    }
    check_scan_cancellation(cancellation)?;
    Ok(admitted)
}

/// Reopens normal source handles and distinct parser-malformed Clang/C#
/// candidates through the same pinned root and one shared byte budget.
pub(super) fn admit_compiler_sources_with_native_candidates_for_scan(
    root: &Path,
    revision_fence: &CompilerRevisionFence,
    mut fresh: Vec<CompilerSourceHandle>,
    reused: Vec<ReusedCompilerFile>,
    clang_candidates: Vec<NativeClangCandidateHandle>,
    csharp_candidates: Vec<NativeCSharpCandidateHandle>,
    source_policy: SourceAdmissionPolicy,
    cancellation: Option<&AtomicBool>,
) -> Result<(Vec<CompilerSource>, NativeCandidateProfiles), String> {
    let mut candidate_profiles = NativeCandidateProfiles::default();
    for candidate in clang_candidates {
        if !matches!(
            candidate.profile,
            LanguageProfile::C(_) | LanguageProfile::Cxx(_)
        ) {
            return Err("native candidate carries a non-Clang profile".to_owned());
        }
        candidate_profiles.clang.insert(candidate.profile);
        fresh.push(CompilerSourceHandle {
            relative_path: candidate.relative_path,
            profile: candidate.profile,
            content: candidate.content,
            source_fact_identity: candidate.source_fact_identity,
            expected_byte_length: None,
        });
    }
    for candidate in csharp_candidates {
        if !matches!(candidate.profile, LanguageProfile::CSharp(_)) {
            return Err("native candidate carries a non-C# profile".to_owned());
        }
        candidate_profiles.csharp.insert(candidate.profile);
        fresh.push(CompilerSourceHandle {
            relative_path: candidate.relative_path,
            profile: candidate.profile,
            content: candidate.content,
            source_fact_identity: candidate.source_fact_identity,
            expected_byte_length: None,
        });
    }
    let sources = admit_compiler_sources_for_scan(
        root,
        revision_fence,
        fresh,
        reused,
        source_policy,
        cancellation,
    )?;
    Ok((sources, candidate_profiles))
}

#[cfg(test)]
pub(super) fn admit_compiler_sources_with_policy(
    root: &Path,
    fresh: Vec<CompilerSourceHandle>,
    reused: Vec<ReusedCompilerFile>,
    source_policy: SourceAdmissionPolicy,
) -> Result<Vec<CompilerSource>, String> {
    let canonical = root
        .canonicalize()
        .map_err(|error| format!("open project {}: {error}", root.display()))?;
    if !canonical.is_dir() {
        return Err(format!(
            "project {} is not a directory",
            canonical.display()
        ));
    }
    let capability = ProjectRoot::open(&canonical)?;
    materialize_compiler_sources(&capability, fresh, reused, source_policy, None, |_| {})
}

fn materialize_compiler_sources(
    capability: &ProjectRoot,
    fresh: Vec<CompilerSourceHandle>,
    reused: Vec<ReusedCompilerFile>,
    source_policy: SourceAdmissionPolicy,
    cancellation: Option<&AtomicBool>,
    mut after_source: impl FnMut(usize),
) -> Result<Vec<CompilerSource>, String> {
    let mut pending = BTreeMap::<String, (LanguageProfile, [u8; 32], SourceFactIdentity, Option<u32>)>::new();
    for source in fresh {
        let path = source.relative_path.clone();
        if pending
            .insert(
                path.clone(),
                (source.profile, source.content, source.source_fact_identity, source.expected_byte_length),
            )
            .is_some()
        {
            return Err(format!("compiler source {path} was admitted twice"));
        }
    }
    for source in reused {
        let path = source.relative_path.clone();
        if pending
            .insert(
                path.clone(),
                (source.profile, source.content, source.source_fact_identity, None),
            )
            .is_some()
        {
            return Err(format!("compiler source {path} was admitted twice"));
        }
    }
    let mut admitted = Vec::with_capacity(pending.len());
    let mut retained_bytes = 0_usize;
    for (path, (profile, expected_content, expected_source_fact_identity, expected_byte_length)) in pending {
        check_scan_cancellation(cancellation)?;
        let bytes = capability
            .read(
                Path::new(&path),
                source_policy.limits().max_file_source_bytes,
            )
            .map_err(|error| reread_fault(&path, error))?;
        check_scan_cancellation(cancellation)?;
        source_policy
            .admit_actual_file(bytes.len())
            .map_err(|refusal| refusal.to_string())?;
        if expected_byte_length.is_some_and(|length| bytes.len() != length as usize) {
            return Err(format!("source {path} changed after its byte length was admitted"));
        }
        let content = typed_of::<InputContentSchema>(&bytes).to_bytes();
        if content != expected_content {
            return Err(format!(
                "source {path} changed after its content hash was admitted"
            ));
        }
        let source_fact_identity = SourceFactIdentity::from_canonical_bytes(&bytes);
        if source_fact_identity != expected_source_fact_identity {
            return Err(format!(
                "source {path} changed after its source-fact identity was admitted"
            ));
        }
        retained_bytes = source_policy
            .admit_retained_total(retained_bytes, bytes.len())
            .map_err(|refusal| refusal.to_string())?;
        check_scan_cancellation(cancellation)?;
        if !matches!(profile, LanguageProfile::Python(_)) && std::str::from_utf8(&bytes).is_err() {
            return Err(format!("source {path} is no longer UTF-8"));
        }
        let source = bytes;
        admitted.push(CompilerSource {
            relative_path: path,
            profile,
            source,
            content: expected_content,
            source_fact_identity,
        });
        after_source(admitted.len());
    }
    Ok(admitted)
}

/// Replaces selected C# source owners with fresh post-selection confined
/// reads. Exact original scan identities are checked before replacing buffers.
pub(super) fn rematerialize_csharp_source_buffers(
    fence: &CompilerRevisionFence,
    sources: &mut [CompilerSource],
    project_path: &str,
    profile: LanguageProfile,
    cancellation: &AtomicBool,
) -> Result<bool, String> {
    if !compiler_revision_is_current_with_cancellation(fence, Some(cancellation))? { return Ok(false); }
    let root = fence.open_bound_root(&fence.requested_root)?;
    let directory = project_path.rsplit_once('/').map_or("", |(parent, _)| parent);
    let mut count = 0usize;
    for source in sources.iter_mut() {
        check_scan_cancellation(Some(cancellation))?;
        if source.profile != profile || (!directory.is_empty() && !source.relative_path.strip_prefix(directory).is_some_and(|suffix| suffix.starts_with('/'))) {
            return Ok(false);
        }
        if source.source.len() > 8 * 1024 * 1024 { return Err("C# source exceeds witness reread bound".to_owned()); }
        let bytes = root.read(Path::new(&source.relative_path), 8 * 1024 * 1024)
            .map_err(|error| reread_fault(&source.relative_path, error))?;
        if bytes.len() != source.source.len() || bytes != source.source
            || typed_of::<InputContentSchema>(&bytes).to_bytes() != source.content
            || SourceFactIdentity::from_canonical_bytes(&bytes) != source.source_fact_identity {
            return Ok(false);
        }
        source.source = bytes;
        count = count.checked_add(1).ok_or("C# source count overflow")?;
    }
    if count == 0 { return Ok(false); }
    compiler_revision_is_current_with_cancellation(fence, Some(cancellation))
}

#[cfg(test)]
pub(super) fn csharp_test_revision_fence(snapshot: &CompilerWorkspaceSnapshot) -> CompilerRevisionFence {
    CompilerRevisionFence {
        requested_root: snapshot.requested_root.clone(), root: snapshot.root.clone(),
        directories: snapshot.entries.iter().filter(|entry| entry.kind == CompilerWorkspaceEntryKind::Directory)
            .map(|entry| (PathBuf::from(&entry.path), Some(entry.revision))).collect(),
        files: snapshot.entries.iter().filter(|entry| entry.kind == CompilerWorkspaceEntryKind::File)
            .map(|entry| CompilerFileRevision { relative_path: PathBuf::from(&entry.path), metadata: Some(entry.revision) }).collect(),
        typescript_reuse: Vec::new(),
    }
}

/// Reopens selected C# source bytes through the original confined root after
/// source selection. This does not establish project membership or authority.
pub(super) fn revalidate_csharp_source_buffers(
    fence: &CompilerRevisionFence,
    sources: &[CompilerSource],
    project_path: &str,
    profile: LanguageProfile,
    cancellation: &AtomicBool,
) -> Result<bool, String> {
    if !compiler_revision_is_current_with_cancellation(fence, Some(cancellation))? {
        return Ok(false);
    }
    let root = fence.open_bound_root(&fence.requested_root)?;
    let directory = project_path.rsplit_once('/').map_or("", |(parent, _)| parent);
    let mut previous: Option<&str> = None;
    let mut count = 0usize;
    for source in sources.iter().filter(|source| source.profile == profile) {
        if !directory.is_empty() && !source.relative_path.strip_prefix(directory).is_some_and(|suffix| suffix.starts_with('/')) {
            continue;
        }
        check_scan_cancellation(Some(cancellation))?;
        if previous.is_some_and(|path| path >= source.relative_path.as_str()) { return Ok(false); }
        previous = Some(&source.relative_path);
        // This independent reread is charged to the 16 MiB C# capture lease;
        // larger configured source files are explicitly outside this profile.
        if source.source.len() > 8 * 1024 * 1024 { return Err("C# source exceeds witness reread bound".to_owned()); }
        let bytes = root.read(Path::new(&source.relative_path), 8 * 1024 * 1024)
            .map_err(|error| reread_fault(&source.relative_path, error))?;
        if bytes != source.source
            || typed_of::<InputContentSchema>(&bytes).to_bytes() != source.content
            || SourceFactIdentity::from_canonical_bytes(&bytes) != source.source_fact_identity {
            return Ok(false);
        }
        count = count.checked_add(1).ok_or("C# source count overflow")?;
    }
    if count == 0 { return Ok(false); }
    compiler_revision_is_current_with_cancellation(fence, Some(cancellation))
}

/// Profiles that still have compiler input in this scan.
///
/// The set is taken from every fresh and reused file, before unchanged
/// profiles are dropped from the compile. A selected publication whose
/// profile is absent from this set no longer has source.
pub(super) fn live_compiler_profiles(
    source_root: &Path,
    fresh: &[CompilerSourceHandle],
    reused: &[ReusedCompilerFile],
) -> BTreeSet<LanguageProfile> {
    compiler_profile_source_counts(source_root, fresh, reused)
        .into_keys()
        .collect()
}

/// Counts fresh and retained inputs under the same effective profile used by
/// compilation. A Rust file's extension default is not its Cargo edition.
pub(super) fn compiler_profile_source_counts(
    source_root: &Path,
    fresh: &[CompilerSourceHandle],
    reused: &[ReusedCompilerFile],
) -> BTreeMap<LanguageProfile, usize> {
    let mut counts = BTreeMap::new();
    for (path, profile) in fresh
        .iter()
        .map(|source| (source.relative_path.as_str(), source.profile))
        .chain(
            reused
                .iter()
                .map(|source| (source.relative_path.as_str(), source.profile)),
        )
    {
        let count = counts
            .entry(compilation_profile(source_root, path, profile))
            .or_insert(0_usize);
        *count = count.saturating_add(1);
    }
    counts
}

/// Selected profiles that this scan no longer contains.
pub(super) fn retired_selected_profiles(
    selected: &[LanguageProfile],
    live: &BTreeSet<LanguageProfile>,
) -> BTreeSet<LanguageProfile> {
    selected
        .iter()
        .copied()
        .filter(|profile| !live.contains(profile))
        .collect()
}

/// The paths whose compiler text is still part of this scan.
pub(super) fn present_compiler_paths(
    fresh: &[CompilerSourceHandle],
    reused: &[ReusedCompilerFile],
) -> BTreeSet<String> {
    let mut present = BTreeSet::new();
    present.extend(fresh.iter().map(|source| source.relative_path.clone()));
    present.extend(reused.iter().map(|source| source.relative_path.clone()));
    present
}

/// Profiles that lost a previously compiled file.
///
/// A zero content version was never compiler input. A path that is still
/// fresh or reused is still part of its profile.
pub(super) fn lost_compiler_profiles(
    source_root: &Path,
    previous: &BTreeMap<[u8; 32], ProductSourceRecord>,
    present: &BTreeSet<String>,
) -> Result<BTreeSet<LanguageProfile>, String> {
    let mut lost = BTreeSet::new();
    for record in previous.values() {
        let Some(fields) = record.file_fields() else {
            continue;
        };
        if fields.content_version == [0; 32] || present.contains(fields.path) {
            continue;
        }
        let Some(profile) = source_record_profile(fields)? else {
            continue;
        };
        lost.insert(compilation_profile(source_root, fields.path, profile));
    }
    Ok(lost)
}

/// Keeps compiler inputs whose profile must be compiled again.
///
/// A profile is compiled when one of its files is fresh or a previous file
/// of that profile disappeared. Every other profile is left out, so its text
/// is not read again and its package compiler is not invoked.
pub(super) fn select_compiler_inputs(
    source_root: &Path,
    fresh: Vec<CompilerSourceHandle>,
    reused: Vec<ReusedCompilerFile>,
    lost: &BTreeSet<LanguageProfile>,
) -> (Vec<CompilerSourceHandle>, Vec<ReusedCompilerFile>) {
    let mut dirty = lost.clone();
    let mut classified = Vec::with_capacity(fresh.len());
    for source in fresh {
        let profile = compilation_profile(source_root, &source.relative_path, source.profile);
        dirty.insert(profile);
        classified.push((source, profile));
    }
    let fresh = classified
        .into_iter()
        .filter(|(_, profile)| dirty.contains(profile))
        .map(|(source, _)| source)
        .collect();
    let reused = reused
        .into_iter()
        .filter(|source| {
            dirty.contains(&compilation_profile(
                source_root,
                &source.relative_path,
                source.profile,
            ))
        })
        .collect();
    (fresh, reused)
}

/// Selects the exact profile one source is compiled under.
///
/// A file's extension names its language, but a Rust file's edition is a
/// fact of the crate that owns it: the authority checks it against Cargo's
/// own metadata and refuses a mismatch. Compiling every `.rs` file as edition
/// 2024 therefore sent every 2015, 2018, and 2021 crate (most of crates.io)
/// to a terminal `ProjectAuthority` failure and a structural-only answer. The
/// edition is read from the nearest `Cargo.toml` with a `[package]` table
/// between the file and the source root, exactly as Cargo resolves it.
pub(super) fn compilation_profile(
    source_root: &Path,
    relative_path: &str,
    profile: LanguageProfile,
) -> LanguageProfile {
    let LanguageProfile::Rust(_) = profile else {
        return profile;
    };
    let mut directory = source_root.join(relative_path);
    while directory.pop() && directory.starts_with(source_root) {
        let manifest = directory.join("Cargo.toml");
        let declares_package = fs::read_to_string(&manifest)
            .is_ok_and(|contents| contents.lines().any(|line| line.trim() == "[package]"));
        if declares_package {
            // An unreadable or unknown edition keeps the default profile; the
            // authority then reports the exact mismatch as a typed terminal
            // rather than this scan failing the whole package.
            return backend_frontend_rust::legacy::manifest_edition(&directory)
                .map_or(profile, LanguageProfile::Rust);
        }
    }
    profile
}

fn reread_fault(path: &str, fault: SourceFault) -> String {
    match fault {
        SourceFault::Vanished => {
            format!("source {path} vanished after its content hash was admitted")
        }
        SourceFault::Unavailable(reason) => format!(
            "source {path} became unavailable ({}) after its content hash was admitted",
            reason.name()
        ),
        SourceFault::UnavailableRead(reason, _) => format!(
            "source {path} became unavailable ({}) after its content hash was admitted",
            reason.name()
        ),
        SourceFault::Fatal(message) => format!("reread source {path}: {message}"),
    }
}

/// Derives one file's semantic profile from the frontend that claimed it.
///
/// Discovery admits a file because some frontend claims its extension, so the
/// claimed set is the only table that decides which extensions exist.  This
/// function therefore never repeats that list: it asks the frontend set which
/// language owns the path and then picks a dialect, which is the one question
/// the extension still answers for the two languages that have two.
///
/// `None` means the path has no profile — either no frontend claims it, or the
/// language's dialects do not cover this extension.  Callers treat that as one
/// unavailable file, never as a broken project.
///
/// # Errors
/// Returns an error only when the frontend registry itself cannot be built.
pub(super) fn source_profile(path: &Path) -> Result<Option<LanguageProfile>, String> {
    source_profile_with_python_version(path, None)
}

pub(super) fn source_profile_with_python_version(
    path: &Path,
    python_version: Option<PythonVersion>,
) -> Result<Option<LanguageProfile>, String> {
    Ok(profile_of(frontends()?, path).map(|profile| match profile {
        LanguageProfile::Python(_) => {
            LanguageProfile::Python(python_version.unwrap_or(PythonVersion::Python314))
        }
        other => other,
    }))
}

/// Resolves a discovered path's profile, or the typed fault its absence is.
///
/// A claimed extension with no semantic profile is one odd file, not a broken
/// project: it is reported per file as
/// [`SourceUnavailableReason::Unparsed`] — nothing could be extracted — and
/// the scan continues.  Returning [`SourceFault::Fatal`] here is what made a
/// single `postcss.config.js` sink an entire checkout.
fn profile_fault(frontends: &FrontendSet, path: &Path) -> Result<LanguageProfile, SourceFault> {
    profile_of(frontends, path).ok_or(SourceFault::Unavailable(SourceUnavailableReason::Unparsed))
}

/// Resolves a path's profile against an explicit frontend set.
fn profile_of(frontends: &FrontendSet, path: &Path) -> Option<LanguageProfile> {
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)?;
    let language = frontends.for_path(path)?.language();
    dialect(language, &extension)
}

/// Persisted Python rows carry the actual admitted minor. Only historical rows
/// without the field use the historical 3.14 producer interpretation.
pub(super) fn source_record_profile(
    file: backend_engine::builtin::ProductFileRef<'_>,
) -> Result<Option<LanguageProfile>, String> {
    if file.language == SourceLanguage::Go {
        return Ok(Some(LanguageProfile::Go(
            file.go_version.unwrap_or(GoVersion::Go125),
        )));
    }
    if file.language == SourceLanguage::Python {
        return Ok(Some(LanguageProfile::Python(
            file.python_version.unwrap_or(PythonVersion::Python314),
        )));
    }
    Ok(admitted_file_profile(file.language, file.path))
}

/// Resolves an already-admitted language/path without constructing a parser.
pub(super) fn admitted_file_profile(
    language: SourceLanguage,
    path: &str,
) -> Option<LanguageProfile> {
    let extension = Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)?;
    // The persisted file already carries its admitted language. Check the
    // producer's extension set without constructing a syntax frontend.
    let admitted = match language {
        SourceLanguage::Rust => matches!(extension.as_str(), "rs"),
        SourceLanguage::Java => matches!(extension.as_str(), "java"),
        SourceLanguage::CSharp => matches!(extension.as_str(), "cs"),
        SourceLanguage::TypeScript => matches!(
            extension.as_str(),
            "ts" | "mts" | "cts" | "js" | "mjs" | "cjs" | "tsx" | "jsx"
        ),
        SourceLanguage::Clang => matches!(
            extension.as_str(),
            "c" | "h" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "m" | "mm"
        ),
        SourceLanguage::Go => matches!(extension.as_str(), "go"),
        SourceLanguage::Python => matches!(extension.as_str(), "py" | "pyi" | "pyw"),
    };
    admitted.then(|| dialect(language, &extension)).flatten()
}


fn project_analysis_version(
    frontend: &ProductFrontend,
    python_version: PythonVersion,
    go_capture: &GoProjectCapture,
    relative: &Path,
) -> [u8; 32] {
    let analysis = frontend.analysis_version();
    if frontend.language() == SourceLanguage::Go {
        return go_capture.analysis_identity(analysis, relative);
    }
    if frontend.language() != SourceLanguage::Python {
        return analysis;
    }
    let mut identity = blake3::Hasher::new();
    identity.update(b"nudox.python.structural-project-profile.v1\0");
    identity.update(&analysis);
    identity.update(
        &backend_frontend_python::legacy::checker::python_version_numbers(python_version)[1]
            .to_be_bytes(),
    );
    *identity.finalize().as_bytes()
}

fn project_profile_fault(
    frontends: &FrontendSet,
    path: &Path,
    python_version: PythonVersion,
    go_version: Option<GoVersion>,
) -> Result<LanguageProfile, SourceFault> {
    let profile = profile_fault(frontends, path)?;
    Ok(match profile {
        LanguageProfile::Python(_) => LanguageProfile::Python(python_version),
        LanguageProfile::Go(_) => LanguageProfile::Go(go_version.unwrap_or(GoVersion::Go125)),
        other => other,
    })
}
pub(super) fn native_clang_profile_for_relative(relative_path: &str) -> Option<LanguageProfile> {
    let extension = Path::new(relative_path)
        .extension()
        .and_then(|value| value.to_str())?
        .to_ascii_lowercase();
    dialect(SourceLanguage::Clang, &extension)
}

pub(super) fn native_csharp_profile_for_relative(relative_path: &str) -> Option<LanguageProfile> {
    let extension = Path::new(relative_path)
        .extension()
        .and_then(|value| value.to_str())?;
    extension
        .eq_ignore_ascii_case("cs")
        .then_some(LanguageProfile::CSharp(CSharpVersion::CSharp14))
}

/// Returns the key the view uses to pair a publication with its files.
///
/// A path names its language and dialect, never a Rust crate's edition: the
/// edition belongs to the crate and is chosen when it is compiled. A
/// publication compiled as edition 2021 must still be recognized as the
/// semantic answer for the `.rs` files [`source_profile`] keys as the default
/// edition, so every Rust edition shares one lane key.
pub(super) const fn lane_profile(profile: LanguageProfile) -> LanguageProfile {
    match profile {
        LanguageProfile::Rust(_) => LanguageProfile::Rust(RustEdition::Rust2024),
        other => other,
    }
}

/// Selects the dialect a claimed extension names inside one language.
///
/// TypeScript and Clang extensions select their dialect. Python's path-only
/// default is 3.14; project admission overrides it before producing any source
/// record or compiler frontier. Saved Python records carry their literal minor.
pub(super) fn dialect(language: SourceLanguage, extension: &str) -> Option<LanguageProfile> {
    Some(match language {
        SourceLanguage::Rust => LanguageProfile::Rust(RustEdition::Rust2024),
        SourceLanguage::Python => LanguageProfile::Python(PythonVersion::Python314),
        SourceLanguage::Go => LanguageProfile::Go(GoVersion::Go125),
        SourceLanguage::Java => LanguageProfile::Java(JavaRelease::Java25),
        SourceLanguage::CSharp => LanguageProfile::CSharp(CSharpVersion::CSharp14),
        SourceLanguage::TypeScript => LanguageProfile::TypeScript(match extension {
            "tsx" | "jsx" => TypeScriptSource::Tsx,
            _ => TypeScriptSource::TypeScript,
        }),
        SourceLanguage::Clang => match extension {
            "c" | "h" | "m" => LanguageProfile::C(CStandard::C23),
            "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" | "mm" => {
                LanguageProfile::Cxx(CxxStandard::Cxx23)
            }
            _ => return None,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::CanonicalRelation;

    #[test]
    fn structural_readiness_preserves_baseline_until_a_manifest_is_observed() {
        use backend_engine::{CapabilityFamily, CapabilityInventory, CapabilityLifecycle, CapabilityTarget};
        let inventory = CapabilityInventory::explicitly_unavailable();
        let baseline = *inventory.as_slice().iter().find(|status|
            matches!(status.family(), CapabilityFamily::StructuralFrontend { .. })
        ).expect("structural slot");
        let target = CapabilityTarget::Native { os: native_os(), architecture: native_architecture() };
        assert_eq!(structural_capability_from_observation(baseline, None, target), baseline);
        let observed = structural_capability_from_observation(baseline, Some([7; 32]), target);
        assert_eq!(observed.lifecycle(), CapabilityLifecycle::Ready);
        assert_eq!(observed.manifest(), Some([7; 32]));
        assert_eq!(structural_capability_from_observation(observed, None, target), observed,
            "a previously admitted manifest must not become an invented negative");
    }

    #[test]
    fn native_evidence_reader_checks_same_buffer_and_refuses_missing_or_retargeted_files() {
        use backend_engine::builtin::NativeSourceEvidence;
        use backend_semantic::ir::SourceIdentity;
        let root = tempfile::tempdir().expect("root");
        let source = "package fixture\n// π\n".as_bytes();
        let file = root.path().join("fixture.go");
        std::fs::write(&file, source).expect("source");
        let image = SourceIdentity::from_bytes(source).expect("image source");
        let mut evidence = NativeSourceEvidence {
            project: [1; 32],
            profile: LanguageProfile::Go(GoVersion::Go125),
            path: "fixture.go".into(),
            input_digest: typed_of::<InputContentSchema>(source).to_bytes(),
            source_digest: *image.identity.as_ref(),
            byte_length: source.len() as u32,
            kind: backend_engine::builtin::NativeSourceEvidenceKind::Image,
            line_starts: None,
            headers: None,
        };
        assert_eq!(
            indexed_native_evidence_matches(root.path(), &evidence, image),
            Ok(true)
        );
        let extent = (0, source.len() as u32);
        let page =
            read_indexed_native_evidence_range(root.path(), &evidence, image, extent, 0, 4096)
                .expect("verified range");
        assert_eq!(page.text.as_bytes(), source);
        assert_eq!(
            read_indexed_native_evidence_range(root.path(), &evidence, image, (0, 0), 0, 4096),
            Err(super::super::source_range::SourceRangeError::InvalidExtent)
        );
        std::fs::write(&file, "package fixture\n// λ\n".as_bytes()).expect("same-length change");
        assert_eq!(
            read_indexed_native_evidence_range(root.path(), &evidence, image, extent, 0, 4096),
            Err(super::super::source_range::SourceRangeError::Stale)
        );
        assert_eq!(
            indexed_native_evidence_matches(root.path(), &evidence, image),
            Ok(false)
        );
        std::fs::write(&file, vec![b'x'; source.len() + 4096]).expect("grown source");
        assert_eq!(indexed_native_evidence_matches(root.path(), &evidence, image), Ok(false));
        assert_eq!(
            read_indexed_native_evidence_range(root.path(), &evidence, image, extent, 0, 4096),
            Err(super::super::source_range::SourceRangeError::Stale),
        );
        std::fs::write(&file, b"longer source").expect("length change");
        assert_eq!(
            indexed_native_evidence_matches(root.path(), &evidence, image),
            Ok(false)
        );
        let mut invalid_utf8 = source.to_vec();
        invalid_utf8[0] = 0xff;
        std::fs::write(&file, invalid_utf8).expect("invalid UTF-8");
        assert_eq!(
            indexed_native_evidence_matches(root.path(), &evidence, image),
            Ok(false)
        );
        std::fs::write(&file, source).expect("restore");
        evidence.input_digest[0] ^= 1;
        assert_eq!(
            indexed_native_evidence_matches(root.path(), &evidence, image),
            Ok(false)
        );
        evidence.input_digest[0] ^= 1;
        evidence.source_digest[0] ^= 1;
        assert_eq!(
            indexed_native_evidence_matches(root.path(), &evidence, image),
            Err(())
        );
        evidence.source_digest[0] ^= 1;
        evidence.byte_length += 1;
        assert_eq!(
            indexed_native_evidence_matches(root.path(), &evidence, image),
            Err(())
        );
        evidence.byte_length -= 1;
        std::fs::remove_file(&file).expect("remove");
        assert_eq!(
            indexed_native_evidence_matches(root.path(), &evidence, image),
            Err(())
        );
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().expect("outside");
            std::fs::write(outside.path().join("other.go"), source).expect("outside source");
            std::os::unix::fs::symlink(outside.path().join("other.go"), &file).expect("symlink");
            assert_eq!(
                indexed_native_evidence_matches(root.path(), &evidence, image),
                Err(())
            );
        }
    }

    #[test]
    fn native_typescript_walk_and_capture_use_exact_confined_bytes_without_frontends()
    -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let root = tmp.path().canonicalize().map_err(|e| e.to_string())?;
        std::fs::write(root.join("view.tsx"), "export const View = 'π';\n")
            .map_err(|e| e.to_string())?;
        std::fs::write(root.join("dep.ts"), "export const dep = 1;\n")
            .map_err(|e| e.to_string())?;
        std::fs::write(root.join("main.go"), "package main\n").map_err(|e| e.to_string())?;
        std::fs::write(root.join("theme.css"), "body {}\n").map_err(|e| e.to_string())?;
        std::fs::write(root.join("tsconfig.json"), "{}\n").map_err(|e| e.to_string())?;
        let capability = ProjectRoot::open(&root)?;
        let mut walk = project_paths_with_policy(
            &root,
            &capability,
            None,
            true,
            source_selection_policy(),
            None,
        )?;
        walk.native_typescript_paths.sort();
        assert_eq!(
            walk.native_typescript_paths,
            [root.join("dep.ts"), root.join("view.tsx")]
        );
        assert_eq!(walk.sources, [root.join("main.go")]);
        assert_eq!(walk.unsupported_css, 1);
        let project =
            backend_engine::PackageKey::from_value(root.to_str().ok_or("root UTF-8")?).to_bytes();
        let make_fence = |walk: &ProjectPaths| -> Result<CompilerRevisionFence, String> {
            let files = walk
                .file_revisions
                .iter()
                .map(|(path, metadata)| {
                    Ok(CompilerFileRevision {
                        relative_path: path
                            .strip_prefix(&root)
                            .map_err(|e| e.to_string())?
                            .to_path_buf(),
                        metadata: *metadata,
                    })
                })
                .collect::<Result<Vec<_>, String>>()?;
            Ok(CompilerRevisionFence {
                requested_root: root.clone(),
                root: root.clone(),
                directories: walk.directories.clone(),
                files,
                typescript_reuse: Vec::new(),
            })
        };
        let fence = make_fence(&walk)?;
        let policy = SourceAdmissionPolicy::from_environment()?;
        let mut bytes_read = 0;
        let mut ledger = SourceAdmissionLedger::default();
        let captured = capture_native_typescript_sources(
            &root,
            &walk.native_typescript_paths,
            project,
            policy,
            &mut ledger,
            &fence,
            &mut bytes_read,
            None,
        )?;
        assert_eq!(captured.project, project);
        assert_eq!(captured.canonical_root, root);
        assert_eq!(captured.sources.len(), 2);
        assert_eq!(
            bytes_read,
            captured
                .sources
                .iter()
                .map(|source| source.evidence.byte_length as usize)
                .sum::<usize>()
        );
        assert!(compiler_revision_is_current(&captured.revision_fence)?);
        for source in &captured.sources {
            let bytes = std::fs::read(root.join(&source.evidence.path)).map_err(|e| e.to_string())?;
            assert_eq!(source.evidence.byte_length as usize, bytes.len());
            assert!(
                source
                    .evidence
                    .verify_host(
                        &bytes,
                        source.source_fact_identity
                    )
                    .is_ok()
            );
        }
        let mut reversed = walk.native_typescript_paths.clone();
        reversed.reverse();
        let mut reversed_ledger = SourceAdmissionLedger::default();
        let mut reversed_read = 0;
        let reordered = capture_native_typescript_sources(
            &root,
            &reversed,
            project,
            policy,
            &mut reversed_ledger,
            &fence,
            &mut reversed_read,
            None,
        )?;
        assert_eq!(captured.source_version, reordered.source_version);
        let first_len = captured.sources[0].evidence.byte_length as usize;
        let limited = SourceAdmissionPolicy::new(SourceAdmissionLimits {
            max_file_source_bytes: 64,
            max_project_source_bytes: 128,
            max_retained_compiler_source_bytes: first_len,
            max_in_flight_source_bytes: 128,
            ..SourceAdmissionLimits::default()
        })
        .map_err(|e| e.to_string())?;
        let mut limited_ledger = SourceAdmissionLedger::default();
        let mut limited_read = 0;
        assert!(
            capture_native_typescript_sources(
                &root,
                &walk.native_typescript_paths,
                project,
                limited,
                &mut limited_ledger,
                &fence,
                &mut limited_read,
                None,
            )
            .is_err()
        );
        assert_eq!(
            limited_read, first_len,
            "a refused second source was not read past remaining budget"
        );
        let other = tempfile::tempdir().map_err(|e| e.to_string())?;
        let other_root = other.path().canonicalize().map_err(|e| e.to_string())?;
        std::fs::write(other_root.join("dep.ts"), b"export const dep = 1;\n")
            .map_err(|e| e.to_string())?;
        let other_project =
            backend_engine::PackageKey::from_value(other_root.to_str().ok_or("other root UTF-8")?)
                .to_bytes();
        let mut foreign_ledger = SourceAdmissionLedger::default();
        let mut foreign_read = 0;
        assert!(
            capture_native_typescript_sources(
                &other_root,
                &[other_root.join("dep.ts")],
                other_project,
                policy,
                &mut foreign_ledger,
                &fence,
                &mut foreign_read,
                None
            )
            .is_err()
        );
        assert_eq!(foreign_read, 0);
        std::fs::write(root.join("tsconfig.json"), "{\"compilerOptions\":{}}\n")
            .map_err(|e| e.to_string())?;
        assert!(!compiler_revision_is_current(&captured.revision_fence)?);
        let first_version = captured.source_version;
        std::fs::write(root.join("view.tsx"), "export const View = 'λ';\n")
            .map_err(|e| e.to_string())?;
        // Even if filesystem metadata happens to retain the same timestamp,
        // a fresh confined read must change the content-bound source version.
        let mut walk = project_paths_with_policy(
            &root,
            &capability,
            None,
            true,
            source_selection_policy(),
            None,
        )?;
        walk.native_typescript_paths.sort();
        let fence = make_fence(&walk)?;
        let mut bytes_read = 0;
        let mut ledger = SourceAdmissionLedger::default();
        let changed = capture_native_typescript_sources(
            &root,
            &walk.native_typescript_paths,
            project,
            policy,
            &mut ledger,
            &fence,
            &mut bytes_read,
            None,
        )?;
        assert_ne!(first_version, changed.source_version);
        std::fs::remove_file(root.join("dep.ts")).map_err(|e| e.to_string())?;
        let mut ledger = SourceAdmissionLedger::default();
        assert!(
            capture_native_typescript_sources(
                &root,
                &walk.native_typescript_paths,
                project,
                policy,
                &mut ledger,
                &fence,
                &mut bytes_read,
                None
            )
            .is_err()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn native_typescript_walk_rejects_non_utf8_candidate_path() -> Result<(), String> {
        use std::os::unix::ffi::OsStringExt;
        let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let root = tmp.path().canonicalize().map_err(|e| e.to_string())?;
        let relative = PathBuf::from(std::ffi::OsString::from_vec(b"bad-\xff.ts".to_vec()));
        assert!(normalized_workspace_path(&relative).is_err());
        let path = root.join(relative);
        if let Err(error) = std::fs::write(path, b"export const value = 1;\n") {
            if cfg!(target_os = "macos") && error.raw_os_error() == Some(92) {
                eprintln!("filesystem rejected non-UTF-8 name; checked path admission directly");
                return Ok(());
            }
            return Err(error.to_string());
        }
        let capability = ProjectRoot::open(&root)?;
        assert!(project_paths_with_policy(&root, &capability, None, true, source_selection_policy(), None).is_err());
        Ok(())
    }

    #[test]
    fn native_typescript_scan_composes_sources_configuration_and_discovery_without_syntax()
    -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let root = tmp.path().canonicalize().map_err(|e| e.to_string())?;
        std::fs::write(root.join("view.tsx"), "export const View = 'π';\n")
            .map_err(|e| e.to_string())?;
        std::fs::write(root.join("dep.ts"), "export const dep = 1;\n")
            .map_err(|e| e.to_string())?;
        std::fs::write(root.join("tsconfig.json"), "{}\n").map_err(|e| e.to_string())?;
        std::fs::write(root.join("theme.css"), "body {}\n").map_err(|e| e.to_string())?;
        let root_text = root.to_str().ok_or("root UTF-8")?;
        let project = backend_engine::PackageKey::from_value(root_text).to_bytes();
        let cancelled = Arc::new(AtomicBool::new(false));
        let automatic = scan_project_with_native_typescript_auto(
            root_text, project, &BTreeMap::new(), Arc::clone(&cancelled), false, None, None,
        )?;
        assert!(automatic.native_typescript_capture.is_some());
        assert!(automatic.files.is_empty());
        assert_eq!(automatic.stylo_walk.files.len(), 1);
        let first = scan_native_typescript_only(root_text, project, Arc::clone(&cancelled))?;
        assert!(first.files.is_empty(), "no fake structural TS rows");
        assert_eq!(first.compiler_sources.len(), 2);
        assert_eq!(
            first
                .native_typescript_capture
                .as_ref()
                .ok_or("native capture")?
                .sources
                .len(),
            2
        );
        assert_eq!(
            first.stylo_walk.files.len(),
            1,
            "CSS stays available to its own native lane"
        );
        let mut rewalk_bytes = 0;
        first.native_typescript_capture.as_ref().ok_or("capture")?
            .revalidate(Some(cancelled.as_ref()), &mut rewalk_bytes)?;
        assert!(rewalk_bytes > 0);
        let mut repeated = scan_native_typescript_only(root_text, project, Arc::clone(&cancelled))?;
        assert_eq!(repeated.source_version, first.source_version);
        let mut admitted_bytes = 0;
        let verified = repeated.native_typescript_capture.take().ok_or("repeated capture")?
            .into_revalidated(&mut admitted_bytes)?;
        assert_eq!(verified.project(), project);
        assert_eq!(verified.project_source_version(), first.source_version);
        assert_eq!(verified.bytes_read(), admitted_bytes);
        assert!(admitted_bytes > 0);
        assert!(verified.matches_cancellation(&cancelled));
        assert!(!verified.matches_cancellation(&Arc::new(AtomicBool::new(false))));
        assert!(verified.checked_retained_bytes().ok_or("retained accounting")? >= std::mem::size_of_val(&verified));
        cancelled.store(true, Ordering::Release);
        assert!(verified.cancelled(), "revalidation retains the original cancellation handle");
        assert!(!verified.fence_is_current().unwrap_or(false));
        cancelled.store(false, Ordering::Release);
        let initial = first.source_version;
        std::fs::write(root.join("view.tsx"), "export const View = 'λ';\n")
            .map_err(|e| e.to_string())?;
        let mut changed_rewalk_bytes = 0;
        assert!(first.native_typescript_capture.as_ref().ok_or("capture")?
            .revalidate(Some(cancelled.as_ref()), &mut changed_rewalk_bytes).is_err());
        let changed = scan_native_typescript_only(root_text, project, Arc::clone(&cancelled))?;
        assert_ne!(
            initial, changed.source_version,
            "same-length source edit changes input basis"
        );
        let before_config = changed.source_version;
        std::fs::write(root.join("tsconfig.json"), "{\"compilerOptions\":{}}\n")
            .map_err(|e| e.to_string())?;
        let configured = scan_native_typescript_only(root_text, project, Arc::clone(&cancelled))?;
        assert_ne!(
            before_config, configured.source_version,
            "config edit changes input basis"
        );
        let before_policy = configured.source_version;
        std::fs::write(root.join(".ignore"), "# nonmatching rule\nnot-present\n")
            .map_err(|e| e.to_string())?;
        let policy = scan_native_typescript_only(root_text, project, Arc::clone(&cancelled))?;
        assert_ne!(
            before_policy, policy.source_version,
            "discovery policy edit changes input basis"
        );
        std::fs::remove_file(root.join("dep.ts")).map_err(|e| e.to_string())?;
        let removed = scan_native_typescript_only(root_text, project, Arc::clone(&cancelled))?;
        assert_ne!(
            policy.source_version, removed.source_version,
            "source removal changes input basis"
        );
        std::fs::remove_file(root.join("view.tsx")).map_err(|e| e.to_string())?;
        let empty = scan_native_typescript_only(root_text, project, Arc::clone(&cancelled))?;
        assert_ne!(removed.source_version, empty.source_version, "last source removal changes input basis");
        assert!(empty.native_typescript_discovery_incomplete());
        let forced_empty = scan_project_with_native_typescript_auto(
            root_text, project, &BTreeMap::new(), Arc::clone(&cancelled), true, None, None,
        )?;
        assert!(forced_empty.native_typescript_discovery_incomplete(), "prior evidence keeps native mode after last deletion");
        assert!(forced_empty.compiler_sources.is_empty());
        assert!(empty.compiler_sources.is_empty());
        assert!(empty.files.is_empty(), "empty observation is not a Complete structural inventory");
        std::fs::write(root.join("main.rs"), "pub fn example() {}\n").map_err(|e| e.to_string())?;
        assert!(
            scan_native_typescript_only(root_text, project, Arc::clone(&cancelled)).is_err(),
            "mixed legacy source lanes refuse without syntax fallback"
        );
        Ok(())
    }

    /// A TypeScript project that also holds C# and Rust must index, not fail.
    /// It leaves the native lane (which admits no second language), loses no
    /// source row, and still discloses the CSS and Vue it cannot analyze.
    #[test]
    fn mixed_language_typescript_project_indexes_and_reports_coverage() -> Result<(), String> {
        let tmp = tempfile::tempdir().map_err(|e| e.to_string())?;
        let root = tmp.path().canonicalize().map_err(|e| e.to_string())?;
        let root_text = root.to_str().ok_or("root UTF-8")?;
        let project = backend_engine::PackageKey::from_value(root_text).to_bytes();
        let cancelled = Arc::new(AtomicBool::new(false));
        let write = |relative: &str, text: &str| -> Result<(), String> {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(path, text).map_err(|e| e.to_string())
        };
        write("tsconfig.json", "{}\n")?;
        write("web/app.ts", "export function renderShell(depth: number): number { return depth; }\n")?;
        write("web/theme.css", "body { margin: 0; }\n")?;
        // Control: with TypeScript as the only supported language the native lane is kept.
        let ts_only = scan_project_with_native_typescript_auto(
            root_text, project, &BTreeMap::new(), Arc::clone(&cancelled), false, None, None,
        )?;
        assert!(ts_only.native_typescript_capture.is_some(), "TypeScript-only keeps the native lane");
        assert!(ts_only.files.is_empty(), "native lane publishes no structural rows");

        write("api/Program.cs", "public class OrderService { public int Count() { return 1; } }\n")?;
        write("core/lib.rs", "pub fn ledger() {}\n")?;
        write("web/Card.vue", "<template><div/></template>\n")?;
        let mixed = scan_project_with_native_typescript_auto(
            root_text, project, &BTreeMap::new(), Arc::clone(&cancelled), false, None, None,
        )?;
        assert!(mixed.native_typescript_capture.is_none(), "a second language leaves the native lane");
        assert!(!mixed.native_typescript_discovery_incomplete());

        // No source is dropped: every supported file has a complete row with its declarations.
        let mut rows = BTreeMap::new();
        for (_, record) in &mixed.files {
            let fields = record.file_fields().ok_or("project file row")?;
            let names = fields.declarations.iter().map(|d| d.name().to_owned()).collect::<Vec<_>>();
            rows.insert(fields.path.to_owned(), (fields.retention, names));
        }
        assert_eq!(
            rows.keys().map(String::as_str).collect::<Vec<_>>(),
            ["api/Program.cs", "core/lib.rs", "web/app.ts"],
        );
        for (path, expected) in [
            ("api/Program.cs", "OrderService"),
            ("core/lib.rs", "ledger"),
            ("web/app.ts", "renderShell"),
        ] {
            let (retention, names) = &rows[path];
            assert_eq!(*retention, backend_engine::DeclarationRetention::Complete, "{path}");
            assert!(names.iter().any(|name| name == expected), "{path}: {names:?}");
        }

        // Unanalyzed languages stay visible as counted exclusions and a CSS inventory.
        let unsupported = mixed
            .source_selection
            .unsupported_extensions
            .iter()
            .map(|row| (row.extension.as_str(), row.files))
            .collect::<Vec<_>>();
        assert_eq!(unsupported, [("css", 1), ("sql", 0), ("vue", 1)]);
        assert_eq!(mixed.stylo_walk.files().len(), 1);

        // It is the same scan the project gets with no TypeScript routing at all.
        let legacy = scan_project(root_text, project, &BTreeMap::new())?;
        assert_eq!(mixed.source_version, legacy.source_version);
        assert_eq!(mixed.files.len(), legacy.files.len());

        // Prior native evidence cannot move to the structural lane here: refuse with a reason.
        let Err(refusal) = scan_project_with_native_typescript_auto(
            root_text, project, &BTreeMap::new(), Arc::clone(&cancelled), true, None, None,
        ) else {
            return Err("forced native lane must refuse a mixed project".into());
        };
        assert!(refusal.contains(NATIVE_TS_MIXED_REFUSAL), "{refusal}");
        assert!(refusal.contains("selected native TypeScript evidence"), "{refusal}");
        Ok(())
    }

    #[test]
    fn admitted_profiles_use_language_and_extension_without_a_frontend() {
        assert_eq!(
            admitted_file_profile(SourceLanguage::TypeScript, "src/VIEW.TSX"),
            Some(LanguageProfile::TypeScript(TypeScriptSource::Tsx))
        );
        assert_eq!(
            admitted_file_profile(SourceLanguage::TypeScript, "src/util.mjs"),
            Some(LanguageProfile::TypeScript(TypeScriptSource::TypeScript))
        );
        assert_eq!(
            admitted_file_profile(SourceLanguage::Clang, "src/a.hpp"),
            Some(LanguageProfile::Cxx(CxxStandard::Cxx23))
        );
        assert_eq!(admitted_file_profile(SourceLanguage::Rust, "src/a.ts"), None);
        assert_eq!(
            admitted_file_profile(SourceLanguage::TypeScript, "src/a.txt"),
            None
        );
        assert_eq!(
            admitted_file_profile(SourceLanguage::Java, "src/no_extension"),
            None
        );
    }

    #[test]
    fn checked_source_range_reopens_confined_file_and_refuses_changed_bytes() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
        }
        let unique = format!("nudox-source-range-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error| error.to_string())?.as_nanos());
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        let body = format!("function ferris() {{\r\n{}\r\n}}\r\n", "const x = \"é漢🦀\";\r\n".repeat(200));
        let host = format!("// prefix\n{body}// suffix");
        fs::write(scratch.0.join("file.ts"), &host).map_err(|error| error.to_string())?;
        let start = host.find("function ferris").ok_or("missing owner")?;
        let name = host.find("ferris").ok_or("missing name")?;
        let capture = backend_compile::SourceBindingCapture::new(start as u32, (start + body.len()) as u32, name as u32, (name + 6) as u32).ok_or("invalid capture")?;
        let identity = backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(host.as_bytes());
        let input = typed_of::<InputContentSchema>(host.as_bytes()).to_bytes();
        let mut offset = 0; let mut output = String::new();
        loop {
            let page = read_indexed_source_range(&scratch.0, "file.ts", identity, input, capture, "ferris", offset, 4096)
                .map_err(|error| format!("range: {error:?}"))?;
            output.push_str(&page.text);
            if page.end == page.total { break; }
            assert!(page.end > offset); offset = page.end;
        }
        assert_eq!(output.as_bytes(), body.as_bytes());
        fs::write(scratch.0.join("file.ts"), host.replace("prefix", "PREFIX")).map_err(|error| error.to_string())?;
        assert_eq!(read_indexed_source_range(&scratch.0, "file.ts", identity, input, capture, "ferris", 0, 4096), Err(super::super::source_range::SourceRangeError::Stale));
        Ok(())
    }

    #[test]
    fn malformed_native_bytes_change_project_source_version() -> Result<(), String> {
        let profile = LanguageProfile::Cxx(CxxStandard::Cxx23);
        let frontier = |content: u8| {
            let bytes = vec![content];
            build_native_frontier_records_with_content(
                [7; 32],
                profile,
                vec![(
                    "src/broken.cpp".to_owned(),
                    NativePathDisposition::ClangMalformed {
                        content: typed_of::<InputContentSchema>(&bytes).to_bytes(),
                        source_identity: backend_version::ContentId::<
                            backend_version::SourceFactDomain,
                        >::from_canonical_bytes(&bytes),
                        byte_length: 0,
                        chunk_count: 0,
                    },
                )],
                vec![("src/broken.cpp".to_owned(), bytes)],
            )
            .expect("construct exact malformed-source frontier")
            .into_iter()
            .map(|(_, record)| record)
            .collect::<Vec<_>>()
        };
        let first = frontier(1);
        let second = frontier(2);
        let version = |records: &[ProductNativeFrontierRecord]| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"same ordinary source root");
            update_source_version_native_frontier(&mut hasher, records);
            *hasher.finalize().as_bytes()
        };
        if version(&first) == version(&second) {
            return Err(
                "malformed native byte changes did not invalidate project source version"
                    .to_owned(),
            );
        }
        Ok(())
    }
    use std::sync::Arc;

    #[test]
    fn cancellable_project_scan_stops_before_opening_the_workspace() {
        let cancellation = AtomicBool::new(true);
        let result = scan_project_for_unproven_authorities_cancellable(
            "workspace-cancelled-before-open",
            [17; 32],
            &BTreeMap::new(),
            &cancellation,
        );
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("a pre-cancelled scan must not open the workspace"),
        };
        assert_eq!(error, INDEX_SCAN_CANCELLED);
    }

    #[test]
    fn project_python_analysis_keys_and_saved_profile_are_consistent_for_all_minors()
    -> Result<(), String> {
        let frontends = frontends()?;
        let frontend = frontends
            .for_path(Path::new("main.py"))
            .ok_or("missing Python parser")?;
        let mut analyses = BTreeSet::new();
        for version in backend_frontend_python::legacy::checker::SUPPORTED_PYTHON_VERSIONS {
            let analysis = project_analysis_version(
                frontend,
                version,
                &GoProjectCapture::default(),
                Path::new("main.py"),
            );
            assert!(
                analyses.insert(analysis),
                "each minor must fence parser facts and reuse"
            );
            let row = ProductSourceRecord::profiled_file_unavailable(
                [1; 32],
                "main.py",
                SourceLanguage::Python,
                analysis,
                SourceUnavailableReason::NotText,
                Some(version),
            )?;
            let mut bytes = Vec::new();
            ProductSourceRelation::encode_value(&row, &mut bytes);
            let reopened = ProductSourceRelation::decode_value(&bytes)
                .map_err(|_| "saved Python profile decode")?;
            assert_eq!(
                source_record_profile(reopened.file_fields().ok_or("missing Python row")?)?,
                Some(LanguageProfile::Python(version))
            );
            assert_eq!(
                project_profile_fault(frontends, Path::new("main.py"), version, None)
                    .map_err(|_| "Python source profile")?,
                LanguageProfile::Python(version)
            );
        }
        // A historical row has exactly the original default, never an inferred hash profile.
        let legacy = ProductSourceRecord::file_unavailable(
            [1; 32],
            "old.py",
            SourceLanguage::Python,
            [2; 32],
            SourceUnavailableReason::NotText,
        )?;
        assert_eq!(
            source_record_profile(legacy.file_fields().ok_or("legacy Python row")?)?,
            Some(LanguageProfile::Python(PythonVersion::Python314))
        );
        Ok(())
    }

    #[test]
    #[ignore = "requires managed genuine CPython 3.12 and 3.14; ordinary venv admission then real native checker"]
    fn genuine_ordinary_venv_selects_profile_before_rows_facts_and_native_compilation()
    -> Result<(), Box<dyn std::error::Error>> {
        use backend_engine::application::{
            ClosedLocalHostEnvironmentSnapshot, LocalCompilerHost, LocalHostDiscovery,
            LocalHostEnvironment, LocalHostVariable,
        };
        use backend_frontend_python::legacy::checker::{
            NativePythonProjectAuthority, PythonProjectBytesSource,
        };
        use std::ffi::OsString;
        use std::time::{Duration, Instant};
        struct Closed(ClosedLocalHostEnvironmentSnapshot);
        impl LocalHostEnvironment for Closed {
            fn value(&self, role: LocalHostVariable) -> Option<OsString> {
                self.0
                    .path(role)
                    .map(|path| path.as_os_str().to_os_string())
            }
        }
        let hosts = [
            (
                PythonVersion::Python314,
                PathBuf::from(
                    std::env::var_os("NUDOX_TEST_ENV_CPYTHON")
                        .ok_or("managed gate requires genuine CPython314")?,
                ),
            ),
            (
                PythonVersion::Python312,
                PathBuf::from(
                    std::env::var_os("NUDOX_TEST_ENV_CPYTHON312")
                        .ok_or("managed gate requires genuine CPython312")?,
                ),
            ),
        ];
        let source = "from pathlib import Path\nvalue: Path = Path('ordinary')\n";
        let cancelled = AtomicBool::new(false);
        for (expected, selected) in &hosts {
            let scratch = crate::test_support::private_temp_dir("ordinary-python-profile")?;
            let root = scratch.path().canonicalize()?;
            let venv = root.join("venv");
            let selected = selected.canonicalize()?;
            // Only the independently configured host executable creates the ordinary venv.
            let output = std::process::Command::new(&selected)
                .args(["-I", "-S", "-B", "-m", "venv", "--without-pip"])
                .arg(&venv)
                .output()?;
            assert!(
                output.status.success(),
                "ordinary venv creation: {:?}",
                output
            );
            fs::write(root.join("main.py"), source)?;
            let snapshot = ClosedLocalHostEnvironmentSnapshot::from_paths([
                (
                    LocalHostVariable::NudoxPythonDefault,
                    hosts[0].1.canonicalize()?,
                ),
                (
                    LocalHostVariable::NudoxPythonCandidate1,
                    hosts[1].1.canonicalize()?,
                ),
            ])?;
            let host = LocalCompilerHost::new(Closed(snapshot), LocalHostDiscovery::ClosedSnapshot);
            let admission = host.capture_python_interpreter_admission()?;
            let coordinate = root.to_str().ok_or("non-text gate path")?;
            let scan = scan_project_for_unproven_authorities_cancellable_with_interpreter(
                coordinate,
                [19; 32],
                &BTreeMap::new(),
                &cancelled,
                Some(&admission),
            )?;
            let environment = scan
                .python_environment
                .as_ref()
                .ok_or("missing installed capture")?
                .captured()?;
            assert_eq!(environment.selected_profile()?, Some(*expected));
            let (_, record) = scan
                .files
                .iter()
                .find(|(_, record)| record.label() == "main.py")
                .ok_or("missing actual source row")?;
            let file = record.file_fields().ok_or("source row")?;
            assert_eq!(file.python_version, Some(*expected));
            assert_eq!(
                file.analysis_version,
                project_analysis_version(
                    frontends()?
                        .for_path(Path::new("main.py"))
                        .ok_or("Python parser")?,
                    *expected,
                    &GoProjectCapture::default(),
                    Path::new("main.py")
                )
            );
            let main_source = scan
                .compiler_sources
                .iter()
                .find(|source| source.relative_path == "main.py")
                .ok_or("missing exact compiler frontier source")?;
            assert_eq!(main_source.profile, LanguageProfile::Python(*expected));
            let main_facts = scan
                .source_facts
                .iter()
                .find(|update| {
                    update.manifest().file_key() == product_source_file_key([19; 32], "main.py")
                })
                .ok_or("missing actual structural fact pages")?;
            assert!(main_facts.manifest().matches_file_identity(file));
            let control = PythonProjectControl {
                cancelled: &cancelled,
                deadline: Instant::now() + Duration::from_secs(180),
            };
            let authority = NativePythonProjectAuthority::admit_controlled(control)?
                .with_timeout(Duration::from_secs(180));
            let sources = [PythonProjectBytesSource {
                relative_path: "main.py",
                source: source.as_bytes(),
            }];
            let report = authority.analyze_project_bytes_with_environment(
                &root,
                "ordinary-minor-profile",
                &sources,
                Some(environment),
                *expected,
                control,
            )?;
            let module = report
                .module("main.py")
                .ok_or("missing genuine native source")?;
            assert!(!module.imports.is_empty());
            assert!(
                module.imports.iter().all(|import| import.resolved),
                "actual stdlib capture must resolve pathlib"
            );
            let mut bytes = Vec::new();
            ProductSourceRelation::encode_value(record, &mut bytes);
            let reopened =
                ProductSourceRelation::decode_value(&bytes).map_err(|_| "source row restart")?;
            assert_eq!(
                source_record_profile(reopened.file_fields().ok_or("saved source")?)?,
                Some(LanguageProfile::Python(*expected))
            );
        }
        Ok(())
    }

    #[test]
    fn structural_frontend_inventory_has_exactly_one_parser_per_language() -> Result<(), String> {
        let frontends = FrontendSet::build()?;
        for language in SourceLanguage::ALL {
            let frontend = frontends
                .for_language(language)
                .ok_or_else(|| format!("missing {language:?} parser capability"))?;
            if frontend.analysis_version() == [0; 32] {
                return Err(format!(
                    "{language:?} parser has an empty producer identity"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn explicit_index_scope_bounds_source_without_charging_opaque_artifacts() -> Result<(), String>
    {
        let project = crate::test_support::private_temp_dir("source-admission-scope")
            .map_err(|error| error.to_string())?;
        let root = project.path();
        fs::create_dir(root.join("unrelated-cache")).map_err(|error| error.to_string())?;
        fs::write(root.join("lib.rs"), [b' '; 32]).map_err(|error| error.to_string())?;
        fs::write(root.join("unrelated-cache/bundle.ts"), [b' '; 48])
            .map_err(|error| error.to_string())?;
        fs::write(
            root.join("Cargo.toml"),
            b"[package]\nname='kept'\nversion='0.1.0'\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(root.join("template.json"), b"{\"required\":true}")
            .map_err(|error| error.to_string())?;
        fs::write(root.join("README.md"), b"kept resource").map_err(|error| error.to_string())?;
        let artifact =
            fs::File::create(root.join("opaque.bin")).map_err(|error| error.to_string())?;
        artifact
            .set_len(1024 * 1024)
            .map_err(|error| error.to_string())?;
        let mut limits = SourceAdmissionLimits::default();
        limits.max_file_source_bytes = 64;
        limits.max_project_source_bytes = 64;
        limits.max_in_flight_source_bytes = 128;
        let policy = SourceAdmissionPolicy::new(limits).map_err(|error| error.to_string())?;
        let frontends = frontends()?;
        let before = supported_paths(root, frontends)?;
        assert_eq!(
            before.len(),
            2,
            "opaque artifacts/manifests are not syntax source"
        );
        let error = preflight_source_bytes(root, &before, policy, None)
            .err()
            .ok_or("expected bound")?;
        assert!(error.contains("80") && error.contains("2 supported-source candidates"));
        assert!(
            error.contains("unrelated-cache/bundle.ts (48 bytes)")
                && error.contains(".nudoxignore")
        );
        assert!(!error.contains("opaque.bin"));
        fs::write(root.join(".nudoxignore"), b"/unrelated-cache/\n")
            .map_err(|error| error.to_string())?;
        let capability = ProjectRoot::open(root)?;
        let paths = project_paths_with_policy(
            root,
            &capability,
            Some(frontends),
            false,
            source_selection_policy(),
            None,
        )?;
        assert_eq!(paths.sources, [root.join("lib.rs")]);
        assert!(
            paths
                .configurations
                .iter()
                .any(|(_, path)| path == &root.join("Cargo.toml"))
        );
        assert!(
            paths
                .file_revisions
                .contains_key(&root.join(".nudoxignore"))
        );
        preflight_source_bytes(root, &paths.sources, policy, None)?;
        assert_eq!(
            capability
                .read(Path::new("template.json"), 64)
                .map_err(|error| format!("{error:?}"))?,
            b"{\"required\":true}"
        );
        assert_eq!(
            capability
                .read(Path::new("README.md"), 64)
                .map_err(|error| format!("{error:?}"))?,
            b"kept resource"
        );
        fs::remove_file(root.join(".nudoxignore")).map_err(|error| error.to_string())?;
        assert!(
            preflight_source_bytes(root, &supported_paths(root, frontends)?, policy, None).is_err(),
            "source re-enters the finite budget when the explicit scope is removed"
        );
        Ok(())
    }

    #[test]
    fn discovery_budget_is_checked_before_an_entry_is_retained() {
        let mut global = DiscoveryBudget {
            entries: MAX_DISCOVERY_ENTRIES,
        };
        assert!(global.admit_entry(0).is_err());

        let mut directory = DiscoveryBudget::default();
        assert!(directory.admit_entry(MAX_DIRECTORY_ENTRIES).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn opened_project_root_rejects_symlink_substitution() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-confined-ingest-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        let project = scratch.0.join("project");
        let outside = scratch.0.join("outside.rs");
        fs::create_dir_all(&project).map_err(|error| error.to_string())?;
        fs::write(&outside, b"pub fn secret() {}").map_err(|error| error.to_string())?;
        let root = ProjectRoot::open(&project)?;

        symlink(&outside, project.join("victim.rs")).map_err(|error| error.to_string())?;
        let maximum = SourceAdmissionLimits::default().max_file_source_bytes;
        assert!(root.read(Path::new("victim.rs"), maximum).is_err());

        fs::create_dir(project.join("real")).map_err(|error| error.to_string())?;
        fs::write(project.join("real/lib.rs"), b"pub fn safe() {}")
            .map_err(|error| error.to_string())?;
        // A confined read still succeeds; a substituted symlink is refused
        // rather than admitted, and the refusal is now a typed per-file fault.
        assert_eq!(
            root.read(Path::new("real/lib.rs"), maximum)
                .map_err(|error| format!("{error:?}"))?,
            b"pub fn safe() {}"
        );
        assert_eq!(
            read_stylo_selected_source(&project, "real/lib.rs", maximum)
                .map_err(|error| format!("{error:?}"))?,
            b"pub fn safe() {}"
        );
        assert_eq!(
            read_stylo_selected_source(&project, "missing.css", maximum),
            Err(SelectedSourceReadError::Vanished)
        );
        assert_eq!(
            read_stylo_selected_source(&project, "../outside.rs", maximum),
            Err(SelectedSourceReadError::Invalid)
        );
        assert_eq!(
            read_stylo_selected_source(&project, "real/lib.rs", 1),
            Err(SelectedSourceReadError::TooLarge)
        );
        fs::remove_dir_all(project.join("real")).map_err(|error| error.to_string())?;
        symlink(&scratch.0, project.join("real")).map_err(|error| error.to_string())?;
        assert!(root.read(Path::new("real/outside.rs"), maximum).is_err());
        Ok(())
    }

    #[test]
    fn every_language_lane_uses_its_grammar_tags() -> Result<(), String> {
        let fixtures = [
            ("fixture.rs", "pub fn ferris() {}", "ferris"),
            ("fixture.py", "def monty():\n    pass", "monty"),
            ("fixture.ts", "export function turing() {}", "turing"),
            ("fixture.go", "func gopher() {}", "gopher"),
            ("fixture.java", "public class Duke {}", "Duke"),
            ("fixture.cs", "public class Anders {}", "Anders"),
            ("fixture.cpp", "struct Bjarne {};", "Bjarne"),
        ];
        for (path, source, expected) in fixtures {
            let frontend = frontends()?
                .for_path(Path::new(path))
                .ok_or_else(|| format!("missing frontend for {path}"))?;
            let found = frontend
                .baseline
                .analyze(Path::new(path), source.as_bytes())
                .map_err(|error| error.to_string())?;
            assert!(
                found
                    .declarations()
                    .iter()
                    .any(|item| item.name() == expected),
                "{path}: expected {expected}, found {:?}",
                found
                    .declarations()
                    .iter()
                    .map(|item| (item.kind(), item.name()))
                    .collect::<Vec<_>>()
            );
        }
        Ok(())
    }

    #[test]
    fn scanned_files_persist_the_semantic_source_content_identity() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let unique = format!(
            "backend-source-identity-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        let source = b"pub fn ferris() {}";
        fs::write(scratch.0.join("lib.rs"), source).map_err(|error| error.to_string())?;
        let scan = scan_project(
            scratch.0.to_str().ok_or("non-UTF-8 scratch path")?,
            [9; 32],
            &BTreeMap::new(),
        )?;
        let fields = scan.files[0]
            .1
            .file_fields()
            .ok_or("expected a source file record")?;
        // The persisted identity must be exactly the construction the engine's
        // semantic compiler derives from the same bytes, so a view comparing
        // image provenance against file records compares like with like.
        let expected =
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                source,
            );
        assert_eq!(fields.source_identity, Some(expected));
        assert_ne!(
            fields.source_identity,
            Some(backend_version::ContentId::<
                backend_version::SourceFactDomain,
            >::from_canonical_bytes(b"edited in place")),
            "a distinct byte stream must not share the file's identity"
        );
        Ok(())
    }

    #[test]
    fn unchanged_files_reuse_the_exact_declaration_allocation() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-syntax-reuse-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("lib.rs"), b"pub fn ferris() {}")
            .map_err(|error| error.to_string())?;
        let project = [7; 32];
        let first = scan_project(
            scratch.0.to_str().ok_or("non-UTF-8 scratch path")?,
            project,
            &BTreeMap::new(),
        )?;
        let reusable = first.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let second = scan_project(
            scratch.0.to_str().ok_or("non-UTF-8 scratch path")?,
            project,
            &reusable,
        )?;
        let first_declarations = first.files[0]
            .1
            .file_fields()
            .ok_or("expected first source file")?
            .declarations;
        let second_declarations = second.files[0]
            .1
            .file_fields()
            .ok_or("expected second source file")?
            .declarations;
        assert_eq!(first.source_version, second.source_version);
        assert!(Arc::ptr_eq(first_declarations, second_declarations));
        assert!(
            second.compiler_sources.is_empty(),
            "an unchanged scan must drop compiler text"
        );
        assert_eq!(second.reused_compiler_files.len(), 1);
        Ok(())
    }

    fn retained_compiler_handle_bytes(scan: &IndexSnapshot) -> usize {
        scan.compiler_sources
            .iter()
            .map(|source| {
                std::mem::size_of::<CompilerSourceHandle>() + source.relative_path.capacity()
            })
            .sum()
    }

    fn declarations_for<'a>(
        scan: &'a IndexSnapshot,
        path: &str,
    ) -> Result<&'a Arc<[backend_engine::SourceDeclaration]>, String> {
        scan.files
            .iter()
            .find_map(|(_, record)| {
                let fields = record.file_fields()?;
                (fields.path == path).then_some(fields.declarations)
            })
            .ok_or_else(|| format!("missing source file {path}"))
    }

    #[test]
    fn reused_scan_drops_compiler_text_until_a_sibling_edit() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let unique = format!(
            "backend-ingest-delta-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        // Include enough real source bytes that the retained path/profile/
        // content handles are smaller than the compiler text they replace.
        // Tiny one-line functions make fixed handle overhead dominate this
        // memory assertion and do not measure the intended tradeoff.
        let kept = format!("// {}\npub fn kept() {{}}\n", "k".repeat(512));
        let edited_v1 = format!("// {}\npub fn edited() {{}}\n", "e".repeat(512));
        let edited_v2 = "pub fn edited() { let _ = 1; }";
        let edited_v3 = "pub fn edited() { let _ = 3; }";
        let kept_v4 = "pub fn kept() { let _ = 4; }";
        fs::write(scratch.0.join("kept.rs"), &kept).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("edited.rs"), &edited_v1).map_err(|error| error.to_string())?;
        let project = [11; 32];

        let cold = scan_project(root, project, &BTreeMap::new())?;
        assert_eq!(
            cold.compiler_sources
                .iter()
                .map(|source| source.relative_path.as_str())
                .collect::<Vec<_>>(),
            ["edited.rs", "kept.rs"]
        );
        assert!(cold.reused_compiler_files.is_empty());
        let cold_retained = retained_compiler_handle_bytes(&cold);
        assert!(cold_retained < edited_v1.len() + kept.len());

        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let warm = scan_project(root, project, &reusable)?;
        let warm_retained = retained_compiler_handle_bytes(&warm);
        assert_eq!(warm_retained, 0);
        assert_eq!(warm.reused_compiler_files.len(), 2);
        assert_eq!(cold.source_version, warm.source_version);
        assert!(Arc::ptr_eq(
            declarations_for(&cold, "kept.rs")?,
            declarations_for(&warm, "kept.rs")?,
        ));
        assert!(Arc::ptr_eq(
            declarations_for(&cold, "edited.rs")?,
            declarations_for(&warm, "edited.rs")?,
        ));

        fs::write(scratch.0.join("edited.rs"), edited_v2).map_err(|error| error.to_string())?;
        let delta = scan_project(root, project, &reusable)?;
        assert_eq!(delta.compiler_sources.len(), 1);
        assert_eq!(delta.compiler_sources[0].relative_path, "edited.rs");
        assert_eq!(
            delta.compiler_sources[0].content,
            typed_of::<InputContentSchema>(edited_v2.as_bytes()).to_bytes()
        );
        assert_eq!(
            delta.compiler_sources[0].source_fact_identity,
            SourceFactIdentity::from_canonical_bytes(edited_v2.as_bytes())
        );
        assert_eq!(delta.reused_compiler_files.len(), 1);
        assert_eq!(delta.reused_compiler_files[0].relative_path, "kept.rs");
        assert_eq!(
            delta.reused_compiler_files[0].content,
            typed_of::<InputContentSchema>(kept.as_bytes()).to_bytes()
        );
        assert_eq!(
            delta.reused_compiler_files[0].source_fact_identity,
            SourceFactIdentity::from_canonical_bytes(kept.as_bytes())
        );
        assert!(Arc::ptr_eq(
            declarations_for(&warm, "kept.rs")?,
            declarations_for(&delta, "kept.rs")?,
        ));
        assert!(!Arc::ptr_eq(
            declarations_for(&warm, "edited.rs")?,
            declarations_for(&delta, "edited.rs")?,
        ));

        // Fresh and reused handles both reopen through the root capability,
        // compare the exact admitted content identity, then build text.
        let admitted = admit_compiler_sources_with_policy(
            scratch.0.as_path(),
            delta.compiler_sources.clone(),
            delta.reused_compiler_files.clone(),
            delta.source_admission_policy,
        )?;
        assert_eq!(
            admitted
                .iter()
                .map(|source| (
                    source.relative_path.as_str(),
                    std::str::from_utf8(&source.source).expect("UTF-8 fixture")
                ))
                .collect::<Vec<_>>(),
            [("edited.rs", edited_v2), ("kept.rs", kept.as_str())]
        );
        assert_eq!(
            admitted[0].source_fact_identity,
            SourceFactIdentity::from_canonical_bytes(edited_v2.as_bytes())
        );
        assert_eq!(
            admitted[1].source_fact_identity,
            SourceFactIdentity::from_canonical_bytes(kept.as_bytes())
        );
        drop(admitted);

        fs::write(scratch.0.join("edited.rs"), edited_v3).map_err(|error| error.to_string())?;
        let Err(error) = admit_compiler_sources_with_policy(
            scratch.0.as_path(),
            delta.compiler_sources.clone(),
            delta.reused_compiler_files.clone(),
            delta.source_admission_policy,
        ) else {
            return Err("a changed fresh file was compiled".to_owned());
        };
        assert!(
            error.contains("edited.rs") && error.contains("changed after its content hash"),
            "{error}"
        );

        let delta_fresh_bytes = retained_compiler_handle_bytes(&delta);
        fs::write(scratch.0.join("edited.rs"), edited_v2).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("kept.rs"), kept_v4).map_err(|error| error.to_string())?;
        let Err(error) = admit_compiler_sources_with_policy(
            scratch.0.as_path(),
            delta.compiler_sources,
            delta.reused_compiler_files,
            delta.source_admission_policy,
        ) else {
            return Err("a changed reused file was compiled".to_owned());
        };
        assert!(
            error.contains("kept.rs") && error.contains("changed after its content hash"),
            "{error}"
        );
        assert!(
            !error.contains("let _ = 4"),
            "the refusal must not echo the torn bytes: {error}"
        );
        eprintln!(
            "ingest_delta files=2 cold_source_handle_bytes={cold_retained} warm_source_handle_bytes={warm_retained} delta_source_handle_bytes={delta_fresh_bytes} delta_reused=1"
        );
        Ok(())
    }

    #[test]
    fn unavailable_bytes_are_not_reused_as_compiler_input() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let unique = format!(
            "backend-ingest-unavailable-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("lib.rs"), [0xff, 0xfe, b'n', b'o'])
            .map_err(|error| error.to_string())?;
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        let project = [12; 32];
        let first = scan_project(root, project, &BTreeMap::new())?;
        assert!(first.compiler_sources.is_empty());
        assert!(first.reused_compiler_files.is_empty());
        assert_eq!(first.files.len(), 1);
        let reusable = first.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let second = scan_project(root, project, &reusable)?;
        assert!(second.compiler_sources.is_empty());
        assert!(second.reused_compiler_files.is_empty());
        Ok(())
    }

    #[test]
    fn admit_rejects_a_path_present_in_both_lists() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let unique = format!(
            "backend-ingest-duplicate-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        fs::create_dir_all(&scratch.0).map_err(|error| error.to_string())?;
        let profile = LanguageProfile::Rust(RustEdition::Rust2024);
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let source = b"pub fn a() {}";
        let source_fact_identity = SourceFactIdentity::from_canonical_bytes(source);
        let Err(error) = admit_compiler_sources_with_policy(
            scratch.0.as_path(),
            vec![CompilerSourceHandle {
                relative_path: "a.rs".to_owned(),
                profile,
                content: typed_of::<InputContentSchema>(source).to_bytes(),
                source_fact_identity,
                expected_byte_length: None,
            }],
            vec![ReusedCompilerFile {
                relative_path: "a.rs".to_owned(),
                profile,
                content: [9; 32],
                source_fact_identity,
            }],
            policy,
        ) else {
            return Err("one path was admitted as both fresh and reused".to_owned());
        };
        assert!(
            error.contains("a.rs") && error.contains("admitted twice"),
            "{error}"
        );
        Ok(())
    }

    #[test]
    fn indexed_source_excerpt_recovers_only_exact_confined_bytes() -> Result<(), String> {
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }

        let unique = format!(
            "backend-indexed-source-recovery-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        let root = scratch.0.join("pkg");
        fs::create_dir_all(root.join("src")).map_err(|error| error.to_string())?;
        let source = b"pub fn recovered() { 7 }\n";
        fs::write(root.join("src/value.rs"), source).map_err(|error| error.to_string())?;
        let identity =
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                source,
            );
        let label = "pkg:cargo/example@1.0.0::recovered";
        let recover = |path: &str, line, kind| {
            recover_indexed_excerpt(
                &root,
                path,
                identity,
                typed_of::<InputContentSchema>(source).to_bytes(),
                SourceLanguage::Rust,
                label,
                line,
                kind,
            )
        };
        assert!(matches!(
            recover("src/value.rs", 1, Some(DeclarationKind::Function)),
            Some(SourceExcerpt::Captured {
                ref text,
                extent: backend_compile::SourceExcerptExtent::Complete,
            })
                if text.as_ref() == "pub fn recovered() { 7 }"
        ));
        assert!(recover("src/value.rs", 2, Some(DeclarationKind::Function)).is_none());
        assert!(recover("src/value.rs", 1, Some(DeclarationKind::Struct)).is_none());
        assert!(recover("../outside.rs", 1, Some(DeclarationKind::Function)).is_none());

        fs::write(root.join("src/value.rs"), b"pub fn recovered() { 8 }\n")
            .map_err(|error| error.to_string())?;
        assert!(recover("src/value.rs", 1, Some(DeclarationKind::Function)).is_none());
        fs::remove_file(root.join("src/value.rs")).map_err(|error| error.to_string())?;
        assert!(recover("src/value.rs", 1, Some(DeclarationKind::Function)).is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let outside = scratch.0.join("outside.rs");
            fs::write(&outside, source).map_err(|error| error.to_string())?;
            symlink(&outside, root.join("src/escape.rs")).map_err(|error| error.to_string())?;
            assert!(recover("src/escape.rs", 1, Some(DeclarationKind::Function)).is_none());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn admit_refuses_a_reused_file_replaced_by_a_symlink() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let unique = format!(
            "backend-ingest-symlink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let scratch = Scratch(std::env::temp_dir().join(unique));
        let project = scratch.0.join("project");
        fs::create_dir_all(&project).map_err(|error| error.to_string())?;
        let outside = scratch.0.join("outside.rs");
        fs::write(&outside, b"pub fn secret() {}").map_err(|error| error.to_string())?;
        fs::write(project.join("kept.rs"), b"pub fn kept() {}")
            .map_err(|error| error.to_string())?;
        let root = project.to_str().ok_or("non-UTF-8 scratch path")?;
        let key = [13; 32];
        let cold = scan_project(root, key, &BTreeMap::new())?;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let warm = scan_project(root, key, &reusable)?;
        assert_eq!(warm.reused_compiler_files.len(), 1);
        fs::remove_file(project.join("kept.rs")).map_err(|error| error.to_string())?;
        symlink(&outside, project.join("kept.rs")).map_err(|error| error.to_string())?;
        let Err(error) = admit_compiler_sources_with_policy(
            project.as_path(),
            Vec::new(),
            warm.reused_compiler_files,
            warm.source_admission_policy,
        ) else {
            return Err("a symlinked reused file was admitted".to_owned());
        };
        assert!(error.contains("kept.rs"), "{error}");
        assert!(!error.contains("secret"), "{error}");
        Ok(())
    }

    fn scratch_dir(label: &str) -> Result<PathBuf, String> {
        let unique = format!(
            "backend-ingest-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        Ok(directory)
    }

    #[cfg(unix)]
    struct GitScratch(PathBuf);

    #[cfg(unix)]
    impl Drop for GitScratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[cfg(unix)]
    fn git_test_command(root: &Path, args: &[&str]) -> Result<Vec<u8>, String> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .map_err(|error| format!("could not run Git test helper: {error}"))?;
        if !output.status.success() {
            return Err(format!("Git test helper failed: git {}", args.join(" ")));
        }
        Ok(output.stdout)
    }

    #[cfg(unix)]
    fn git_test_repo(label: &str, files: &[(&str, &[u8])]) -> Result<Option<GitScratch>, String> {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return Ok(None);
        }
        let repository = GitScratch(scratch_dir(label)?);
        let root = &repository.0;
        git_test_command(root, &["init", "-q"])?;
        git_test_command(
            root,
            &["config", "user.email", "frontier-test@example.invalid"],
        )?;
        git_test_command(root, &["config", "user.name", "Source Frontier Test"])?;
        for (relative, bytes) in files {
            let path = root.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            fs::write(path, bytes).map_err(|error| error.to_string())?;
        }
        git_test_command(root, &["add", "-A"])?;
        git_test_command(root, &["commit", "-m", "initial source snapshot"])?;
        Ok(Some(repository))
    }

    #[cfg(unix)]
    fn git_test_commit(root: &Path, message: &str) -> Result<(), String> {
        git_test_command(root, &["add", "-A"])?;
        git_test_command(root, &["commit", "-m", message])?;
        Ok(())
    }

    #[cfg(unix)]
    fn git_project_key(root: &Path) -> [u8; 32] {
        *blake3::hash(root.as_os_str().as_encoded_bytes()).as_bytes()
    }

    fn reusable_rows(snapshot: &IndexSnapshot) -> BTreeMap<[u8; 32], ProductSourceRecord> {
        snapshot.files.iter().cloned().collect()
    }

    #[cfg(unix)]
    #[test]
    fn go_module_profiles_bind_fresh_reused_and_cold_source_frontiers() -> Result<(), String> {
        let repository = git_test_repo(
            "go-module-profiles",
            &[
                ("go.mod", b"module example.com/root\ngo 1.25.0\n"),
                ("main.go", b"package main\nfunc main() {}\n"),
                ("nested/go.mod", b"module example.com/nested\ngo 1.24.3\n"),
                ("nested/child.go", b"package nested\nfunc Child() {}\n"),
            ],
        )?
        .ok_or("Git is required for the actual frontier control")?;
        let root = &repository.0;
        let coordinate = root.to_str().ok_or("non-UTF-8 Go fixture")?;
        let project = git_project_key(root);
        let cold = scan_project(coordinate, project, &BTreeMap::new())?;
        let saved = reusable_rows(&cold);
        let check = |snapshot: &IndexSnapshot,
                     path: &str,
                     version: Option<GoVersion>|
         -> Result<(), String> {
            let record = snapshot
                .files
                .iter()
                .find(|(_, row)| row.label() == path)
                .ok_or("missing Go source row")?;
            let mut bytes = Vec::new();
            ProductSourceRelation::encode_value(&record.1, &mut bytes);
            let reopened =
                ProductSourceRelation::decode_value(&bytes).map_err(|_| "cold Go row decode")?;
            let fields = reopened.file_fields().ok_or("cold Go row fields")?;
            assert_eq!(fields.go_version, version, "{path}");
            assert_eq!(
                source_record_profile(fields)?,
                Some(LanguageProfile::Go(version.unwrap_or(GoVersion::Go125)))
            );
            let profiles = snapshot
                .compiler_sources
                .iter()
                .map(|source| (source.relative_path.as_str(), source.profile))
                .chain(
                    snapshot
                        .reused_compiler_files
                        .iter()
                        .map(|source| (source.relative_path.as_str(), source.profile)),
                )
                .collect::<BTreeMap<_, _>>();
            assert_eq!(
                profiles.get(path),
                Some(&LanguageProfile::Go(version.unwrap_or(GoVersion::Go125)))
            );
            Ok(())
        };
        check(&cold, "main.go", Some(GoVersion::Go125))?;
        check(&cold, "nested/child.go", Some(GoVersion::Go124))?;
        let warm = scan_project(coordinate, project, &saved)?;
        assert_eq!(
            warm.source_bytes_read, 0,
            "unchanged Git frontier avoids source reads"
        );
        check(&warm, "main.go", Some(GoVersion::Go125))?;
        check(&warm, "nested/child.go", Some(GoVersion::Go124))?;
        fs::write(root.join("go.mod"), b"module example.com/root\ngo 1.26.2\n")
            .map_err(|error| error.to_string())?;
        assert!(!compiler_revision_is_current(&cold.revision_fence)?);
        let changed = scan_project(coordinate, project, &saved)?;
        check(&changed, "main.go", Some(GoVersion::Go126))?;
        check(&changed, "nested/child.go", Some(GoVersion::Go124))?;
        assert_eq!(
            cold.files
                .iter()
                .find(|(_, row)| row.label() == "nested/child.go"),
            changed
                .files
                .iter()
                .find(|(_, row)| row.label() == "nested/child.go"),
            "another module's manifest edit preserves this file's exact row/analysis key",
        );
        assert_ne!(
            cold.source_version, changed.source_version,
            "manifest-only change renews source/analysis keys"
        );
        // Reopening old rows never consults the now-newer live manifest.
        check(&cold, "main.go", Some(GoVersion::Go125))?;
        fs::remove_file(root.join("nested/go.mod")).map_err(|error| error.to_string())?;
        let boundary_removed = scan_project(coordinate, project, &reusable_rows(&changed))?;
        check(&boundary_removed, "nested/child.go", Some(GoVersion::Go126))?;
        assert_eq!(
            changed
                .files
                .iter()
                .find(|(_, row)| row.label() == "main.go"),
            boundary_removed
                .files
                .iter()
                .find(|(_, row)| row.label() == "main.go"),
            "nested boundary deletion does not invalidate the parent's own file",
        );
        fs::remove_file(root.join("go.mod")).map_err(|error| error.to_string())?;
        let legacy = scan_project(coordinate, project, &reusable_rows(&boundary_removed))?;
        check(&legacy, "main.go", None)?;
        check(&legacy, "nested/child.go", None)?;
        let live = live_compiler_profiles(
            root,
            &legacy.compiler_sources,
            &legacy.reused_compiler_files,
        );
        assert_eq!(
            live,
            BTreeSet::from([LanguageProfile::Go(GoVersion::Go125)])
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn ignored_go_module_creation_is_admitted_and_fenced() -> Result<(), String> {
        let repository = git_test_repo(
            "go-hidden-module",
            &[
                (".gitignore", b"go.mod\n"),
                ("main.go", b"package main\nfunc main() {}\n"),
            ],
        )?
        .ok_or("Git is required for module absence control")?;
        let root = &repository.0;
        let coordinate = root.to_str().ok_or("Go fixture path")?;
        let project = git_project_key(root);
        let absent = scan_project(coordinate, project, &BTreeMap::new())?;
        assert_eq!(
            absent.files[0].1.file_fields().ok_or("Go row")?.go_version,
            None
        );
        fs::write(
            root.join("go.mod"),
            b"module example.com/hidden\ngo 1.26.2\n",
        )
        .map_err(|error| error.to_string())?;
        assert!(!compiler_revision_is_current(&absent.revision_fence)?);
        let present = scan_project(coordinate, project, &reusable_rows(&absent))?;
        assert_eq!(
            present.files[0].1.file_fields().ok_or("Go row")?.go_version,
            Some(GoVersion::Go126)
        );
        assert!(
            present
                .compiler_configuration
                .files
                .iter()
                .any(|file| file.relative_path == Path::new("go.mod"))
        );
        fs::remove_file(root.join("go.mod")).map_err(|error| error.to_string())?;
        assert!(!compiler_revision_is_current(&present.revision_fence)?);
        std::os::unix::fs::symlink(root.join(".gitignore"), root.join("go.mod"))
            .map_err(|error| error.to_string())?;
        assert!(
            scan_project(coordinate, project, &reusable_rows(&present)).is_err(),
            "a symlink module cannot acquire the legacy fallback"
        );
        Ok(())
    }

    #[test]
    fn go_future_module_is_refused_before_source_rows_or_frontiers() -> Result<(), String> {
        let root = crate::test_support::private_temp_dir("go-future-module")
            .map_err(|error| error.to_string())?;
        fs::write(
            root.path().join("go.mod"),
            b"module example.com/future\ngo 1.27.0\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            root.path().join("main.go"),
            b"package main\nfunc main() {}\n",
        )
        .map_err(|error| error.to_string())?;
        let result = scan_project(
            root.path().to_str().ok_or("Go fixture path")?,
            [31; 32],
            &BTreeMap::new(),
        );
        let Err(error) = result else {
            return Err("future Go module acquired a supported source profile".to_owned());
        };
        assert!(
            error.contains("go.mod")
                && error.contains("1.27.0")
                && error.contains("unsupported Go language"),
            "{error}"
        );
        Ok(())
    }

    fn scan_with_source_policy(
        root: &Path,
        project: [u8; 32],
        reusable: &BTreeMap<[u8; 32], ProductSourceRecord>,
        source_policy: SourceAdmissionPolicy,
    ) -> Result<IndexSnapshot, String> {
        let root = root.to_str().ok_or("non-UTF-8 Git path")?;
        scan_project_with_configuration_policy_attempt(
            root,
            project,
            reusable,
            source_selection_policy(),
            true,
            true,
            None,
            source_policy,
            None,
            None,
            None,
            NativeTsScanMode::Legacy,
        )
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_warm_scan_reads_no_source_bytes_with_ignored_build_output() -> Result<(), String>
    {
        let Some(repository) = git_test_repo(
            "frontier-ignored-build",
            &[
                (".gitignore", b"/target/\n"),
                ("lib.rs", b"pub fn stable() -> u8 { 7 }\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let project = git_project_key(root);
        git_test_command(root, &["config", "core.fsmonitor", "true"])?;
        let source_bytes = b"pub fn stable() -> u8 { 7 }\n";
        fs::create_dir_all(root.join("target")).map_err(|error| error.to_string())?;
        fs::write(root.join("target/generated.rs"), b"pub fn generated() {}\n")
            .map_err(|error| error.to_string())?;

        let cold = scan_project(
            root.to_str().ok_or("non-UTF-8 Git path")?,
            project,
            &BTreeMap::new(),
        )?;
        assert_eq!(cold.files.len(), 1);
        assert_eq!(cold.source_bytes_read, source_bytes.len());
        let warm = scan_project(
            root.to_str().ok_or("non-UTF-8 Git path")?,
            project,
            &reusable_rows(&cold),
        )?;
        assert_eq!(warm.source_bytes_read, 0);
        assert_eq!(warm.files.len(), 1);
        assert_eq!(cold.source_version, warm.source_version);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn changed_source_quota_invalidates_warm_frontier_before_file_admission() -> Result<(), String>
    {
        let Some(repository) = git_test_repo(
            "frontier-source-quota-identity",
            &[("lib.rs", b"pub fn stable() -> u8 { 7 }\n")],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let project = git_project_key(root);
        let default = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let cold = scan_with_source_policy(root, project, &BTreeMap::new(), default)?;
        let reusable = reusable_rows(&cold);
        assert_eq!(
            cold.source_bytes_read,
            b"pub fn stable() -> u8 { 7 }\n".len()
        );

        let mut changed_limits = SourceAdmissionLimits::default();
        changed_limits.max_file_source_bytes = 16;
        changed_limits.max_in_flight_source_bytes = 32;
        let changed =
            SourceAdmissionPolicy::new(changed_limits).map_err(|error| error.to_string())?;
        assert_ne!(default.identity(), changed.identity());
        let warm = scan_with_source_policy(root, project, &reusable, changed)?;
        let row = warm.files[0].1.file_fields().ok_or("expected source row")?;
        assert_eq!(warm.source_bytes_read, 0);
        assert!(warm.reused_compiler_files.is_empty());
        assert!(warm.compiler_sources.is_empty());
        assert_eq!(
            row.retention,
            backend_engine::DeclarationRetention::Unavailable(SourceUnavailableReason::TooLarge),
            "the tightened quota must refuse the cached file as too large"
        );
        Ok(())
    }

    #[test]
    fn complete_fact_pages_exceeding_the_project_quota_refuse_before_retention()
    -> Result<(), String> {
        let root = scratch_dir("source-fact-page-budget")?;
        let mut source = String::new();
        for index in 0..1_000 {
            source.push_str(&format!(
                "export function declaration_{index:04}(): number {{ return {index}; }}\n"
            ));
        }
        fs::write(root.join("a_many.ts"), source).map_err(|error| error.to_string())?;
        fs::write(root.join("z_small.ts"), "export const small = 1;\n")
            .map_err(|error| error.to_string())?;
        let default = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let admitted = scan_with_source_policy(&root, [29; 32], &BTreeMap::new(), default)?;
        assert_eq!(admitted.files.len(), 2);
        let fact_bytes = admitted
            .source_facts
            .iter()
            .map(ProductSourceFileFactsUpdate::encoded_bytes)
            .sum::<usize>();
        assert!(
            fact_bytes > ProductSourceRecord::ROW_VALUE_CAPACITY,
            "the real parser must produce multiple complete fact pages"
        );
        let mut compact_bytes = 0;
        for (_, row) in &admitted.files {
            let mut encoded = Vec::new();
            ProductSourceRelation::encode_value(row, &mut encoded);
            assert!(encoded.len() <= ProductSourceRecord::ROW_VALUE_CAPACITY);
            compact_bytes += encoded.len();
        }
        let mut limits = default.limits();
        limits.max_project_record_bytes = compact_bytes.max(limits.max_encoded_record_bytes);
        let tight = SourceAdmissionPolicy::new(limits).map_err(|error| error.to_string())?;
        let error = scan_with_source_policy(&root, [29; 32], &BTreeMap::new(), tight)
            .err()
            .ok_or("the complete fact pages escaped the project quota")?;
        assert!(error.contains("project record bytes"), "{error}");
        // The refusing scan drains its workers. A subsequent scan can admit
        // the same exact sources without a poisoned cancellation or channel.
        let retried = scan_with_source_policy(&root, [29; 32], &BTreeMap::new(), default)?;
        assert_eq!(retried.source_version, admitted.source_version);
        assert_eq!(retried.files, admitted.files);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn warm_frontier_charges_unchanged_fact_pages_after_another_file_grows() -> Result<(), String> {
        let repository = GitScratch(scratch_dir("warm-fact-page-budget")?);
        let root = &repository.0;
        git_test_command(root, &["init", "-q"])?;
        git_test_command(
            root,
            &["config", "user.email", "source-budget@example.invalid"],
        )?;
        git_test_command(root, &["config", "user.name", "Source Budget Test"])?;
        let declarations = |count: usize| {
            (0..count)
                .map(|index| {
                    format!(
                        "export function declaration_{index:04}(): number {{ return {index}; }}\n"
                    )
                })
                .collect::<String>()
        };
        fs::write(root.join("a_many.ts"), declarations(1_000))
            .map_err(|error| error.to_string())?;
        fs::write(root.join("b_changed.ts"), declarations(1)).map_err(|error| error.to_string())?;
        git_test_command(root, &["add", "-A"])?;
        git_test_command(root, &["commit", "-m", "fact page budget fixture"])?;
        let project = git_project_key(root);
        let default = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let cold = scan_with_source_policy(root, project, &BTreeMap::new(), default)?;
        let encoded_rows = |snapshot: &IndexSnapshot| {
            snapshot
                .files
                .iter()
                .map(|(_, row)| {
                    let mut bytes = Vec::new();
                    ProductSourceRelation::encode_value(row, &mut bytes);
                    bytes.len()
                })
                .sum::<usize>()
                + snapshot
                    .encoded_fact_bytes
                    .iter()
                    .map(|(_, bytes)| bytes)
                    .sum::<usize>()
        };
        let mut limits = default.limits();
        limits.max_project_record_bytes = encoded_rows(&cold) + 512;
        let tight = SourceAdmissionPolicy::new(limits).map_err(|error| error.to_string())?;
        let seeded = scan_with_source_policy(root, project, &reusable_rows(&cold), tight)?;
        let reusable = reusable_rows(&seeded);
        for _ in 0..2 {
            let warm = scan_with_source_policy(root, project, &reusable, tight)?;
            assert_eq!(
                warm.source_bytes_read, 0,
                "the unchanged frontier must still avoid source reads"
            );
            assert!(
                warm.source_facts.is_empty(),
                "the page charge must not force reconstruction"
            );
            assert_eq!(
                warm.encoded_fact_bytes, seeded.encoded_fact_bytes,
                "a warm frontier must carry full page charges into the next frontier"
            );
        }
        fs::write(root.join("b_changed.ts"), declarations(500))
            .map_err(|error| error.to_string())?;
        // A distinct cache coordinate keeps the tight-policy frontier intact.
        // The following refusal must exercise cached unchanged pages, rather
        // than a full fallback caused by switching this coordinate's policy.
        let grown = scan_with_source_policy(root, [0x73; 32], &BTreeMap::new(), default)?;
        assert!(encoded_rows(&grown) > tight.limits().max_project_record_bytes);
        // The changed file alone fits; only accounting the unchanged file's
        // complete pages makes the aggregate refusal correct.
        let changed_charge = grown
            .encoded_fact_bytes
            .iter()
            .find(|(key, _)| *key == product_source_file_key([0x73; 32], "b_changed.ts"))
            .ok_or("changed file charge")?
            .1;
        assert!(changed_charge < tight.limits().max_project_record_bytes);
        let error = scan_with_source_policy(root, project, &reusable, tight)
            .err()
            .ok_or("warm reuse erased the unchanged file's page charge")?;
        assert!(error.contains("project record bytes"), "{error}");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn many_small_files_85_mib_cold_and_warm_scans_obey_admission_and_memory_caps()
    -> Result<(), String> {
        const FILE_COUNT: usize = 10_880;
        const FILE_BYTES: usize = 8 * 1024;
        let repository = GitScratch(scratch_dir("source-budget-85-mib")?);
        let root = &repository.0;
        git_test_command(root, &["init", "-q"])?;
        git_test_command(
            root,
            &["config", "user.email", "source-budget@example.invalid"],
        )?;
        git_test_command(root, &["config", "user.name", "Source Budget Test"])?;
        for index in 0..FILE_COUNT {
            let prefix = format!("pub fn generated_{index}() -> usize {{ {index} }}\n");
            if prefix.len() >= FILE_BYTES {
                return Err("generated source prefix exceeded the fixture size".to_owned());
            }
            let mut bytes = vec![b' '; FILE_BYTES];
            bytes[..prefix.len()].copy_from_slice(prefix.as_bytes());
            fs::write(root.join(format!("generated_{index:05}.rs")), &bytes)
                .map_err(|error| error.to_string())?;
        }
        git_test_command(root, &["add", "-A"])?;
        git_test_command(root, &["commit", "-m", "large source budget fixture"])?;

        let project = git_project_key(root);
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let limits = policy.limits();
        let expected_bytes = FILE_COUNT * FILE_BYTES;
        assert!(expected_bytes > 84 * 1024 * 1024);
        assert!(FILE_COUNT > 10_000);
        assert!(expected_bytes < limits.max_project_source_bytes);
        assert!(expected_bytes < limits.max_retained_compiler_source_bytes);
        assert!(limits.max_in_flight_source_bytes < expected_bytes);

        let cold = scan_with_source_policy(root, project, &BTreeMap::new(), policy)?;
        let cold_handle_bytes = retained_compiler_handle_bytes(&cold);
        assert_eq!(cold.files.len(), FILE_COUNT);
        assert_eq!(cold.source_bytes_read, expected_bytes);
        assert!(cold_handle_bytes < expected_bytes / 20);
        assert!(
            policy.worker_count(usize::MAX, FILE_COUNT) * 2 * limits.max_file_source_bytes
                <= limits.max_in_flight_source_bytes,
            "active parse buffers plus queued results must fit the in-flight budget"
        );

        let compiler_sources = admit_compiler_sources_with_policy(
            root,
            cold.compiler_sources.clone(),
            cold.reused_compiler_files.clone(),
            policy,
        )?;
        let materialized_bytes = compiler_sources
            .iter()
            .map(|source| source.source.len())
            .sum::<usize>();
        assert_eq!(materialized_bytes, expected_bytes);
        assert!(materialized_bytes <= limits.max_retained_compiler_source_bytes);
        drop(compiler_sources);

        let warm = scan_with_source_policy(root, project, &reusable_rows(&cold), policy)?;
        assert_eq!(warm.source_version, cold.source_version);
        assert_eq!(warm.source_bytes_read, 0);
        assert!(warm.compiler_sources.is_empty());
        assert_eq!(warm.reused_compiler_files.len(), FILE_COUNT);
        Ok(())
    }

    #[test]
    fn one_valid_generated_tsx_over_512_kib_is_scanned_and_reopened() -> Result<(), String> {
        const GENERATED_BYTES: usize = 768 * 1024;
        let root = scratch_dir("large-generated-tsx")?;
        let generated = "x".repeat(GENERATED_BYTES);
        let source =
            format!("const Generated = <div>{generated}</div>;\nexport default Generated;\n");
        fs::write(root.join("Generated.tsx"), &source).map_err(|error| error.to_string())?;
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        assert!(source.len() > 512 * 1024);
        assert!(source.len() < policy.limits().max_file_source_bytes);

        let scan = scan_with_source_policy(&root, [21; 32], &BTreeMap::new(), policy)?;
        assert_eq!(scan.source_bytes_read, source.len());
        assert_eq!(scan.compiler_sources.len(), 1);
        assert_eq!(
            scan.compiler_sources[0].profile,
            LanguageProfile::TypeScript(TypeScriptSource::Tsx)
        );
        assert_eq!(
            scan.compiler_sources[0].content,
            typed_of::<InputContentSchema>(source.as_bytes()).to_bytes()
        );
        let admitted = admit_compiler_sources_with_policy(
            &root,
            scan.compiler_sources.clone(),
            Vec::new(),
            scan.source_admission_policy,
        )?;
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].content, scan.compiler_sources[0].content);
        assert_eq!(
            admitted[0].source_fact_identity,
            SourceFactIdentity::from_canonical_bytes(source.as_bytes())
        );
        assert_eq!(admitted[0].source.as_slice(), source.as_bytes());
        fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn compiler_handoff_rejects_retargeted_root_alias_with_identical_source() -> Result<(), String>
    {
        use std::os::unix::fs::symlink;

        let scratch = scratch_dir("root-alias-retarget")?;
        let admitted_root = scratch.join("admitted");
        let foreign_root = scratch.join("foreign");
        let alias = scratch.join("project");
        fs::create_dir_all(&admitted_root).map_err(|error| error.to_string())?;
        fs::create_dir_all(&foreign_root).map_err(|error| error.to_string())?;
        let source = b"pub fn same_bytes() {}\n";
        fs::write(admitted_root.join("lib.rs"), source).map_err(|error| error.to_string())?;
        fs::write(foreign_root.join("lib.rs"), source).map_err(|error| error.to_string())?;
        symlink(&admitted_root, &alias).map_err(|error| error.to_string())?;

        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let scan = scan_with_source_policy(&alias, [31; 32], &BTreeMap::new(), policy)?;
        fs::remove_file(&alias).map_err(|error| error.to_string())?;
        symlink(&foreign_root, &alias).map_err(|error| error.to_string())?;

        let error = match admit_compiler_sources_for_scan(
            &alias,
            &scan.revision_fence,
            scan.compiler_sources,
            scan.reused_compiler_files,
            scan.source_admission_policy,
            None,
        ) {
            Err(error) => error,
            Ok(_) => {
                return Err("same-content foreign root crossed the admitted-root fence".to_owned());
            }
        };
        assert!(
            error.contains("alias now resolves to a different directory"),
            "{error}"
        );
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn compiler_workspace_rejects_retargeted_ancestor_alias_with_identical_files()
    -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let scratch = scratch_dir("workspace-root-alias-retarget")?;
        let admitted_parent = scratch.join("admitted-parent");
        let foreign_parent = scratch.join("foreign-parent");
        let alias = scratch.join("workspace-alias");
        let requested_root = alias.join("project");
        fs::create_dir_all(requested_root.as_path()).map_err(|error| error.to_string())?;
        fs::create_dir_all(&admitted_parent).map_err(|error| error.to_string())?;
        fs::create_dir_all(foreign_parent.join("project")).map_err(|error| error.to_string())?;
        fs::write(requested_root.join("lib.rs"), b"pub fn same() {}\n")
            .map_err(|error| error.to_string())?;
        fs::write(foreign_parent.join("project/lib.rs"), b"pub fn same() {}\n")
            .map_err(|error| error.to_string())?;
        fs::rename(alias.join("project"), admitted_parent.join("project"))
            .map_err(|error| error.to_string())?;
        fs::remove_dir(&alias).map_err(|error| error.to_string())?;
        symlink(&admitted_parent, &alias).map_err(|error| error.to_string())?;

        let snapshot = CompilerWorkspaceSnapshot::open(&requested_root)?;
        fs::remove_file(&alias).map_err(|error| error.to_string())?;
        symlink(&foreign_parent, &alias).map_err(|error| error.to_string())?;
        assert!(!snapshot.revalidate()?);
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[test]
    fn compiler_handoff_rejects_root_replacement_with_identical_source() -> Result<(), String> {
        let scratch = scratch_dir("root-replacement")?;
        let root = scratch.join("project");
        let moved_root = scratch.join("moved-project");
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let source = b"pub fn same_bytes() {}\n";
        fs::write(root.join("lib.rs"), source).map_err(|error| error.to_string())?;
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let scan = scan_with_source_policy(&root, [32; 32], &BTreeMap::new(), policy)?;

        fs::rename(&root, &moved_root).map_err(|error| error.to_string())?;
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        fs::write(root.join("lib.rs"), source).map_err(|error| error.to_string())?;
        let error = match admit_compiler_sources_for_scan(
            &root,
            &scan.revision_fence,
            scan.compiler_sources,
            scan.reused_compiler_files,
            scan.source_admission_policy,
            None,
        ) {
            Err(error) => error,
            Ok(_) => return Err("replacement root crossed the admitted-root fence".to_owned()),
        };
        assert!(error.contains("project root directory changed"), "{error}");
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn unchanged_tmp_alias_resolves_to_its_admitted_private_tmp_root() -> Result<(), String> {
        let root = PathBuf::from("/tmp").join(format!(
            "backend-tmp-alias-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        ));
        fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let source = b"pub fn temporary_alias() {}\n";
        fs::write(root.join("lib.rs"), source).map_err(|error| error.to_string())?;
        let canonical = root.canonicalize().map_err(|error| error.to_string())?;
        if canonical == root || !canonical.starts_with("/private/tmp") {
            let _ = fs::remove_dir_all(&root);
            return Err(format!(
                "expected /tmp to resolve through /private/tmp: {canonical:?}"
            ));
        }
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let scan = scan_with_source_policy(&root, [33; 32], &BTreeMap::new(), policy)?;
        let admitted = admit_compiler_sources_for_scan(
            &root,
            &scan.revision_fence,
            scan.compiler_sources,
            scan.reused_compiler_files,
            scan.source_admission_policy,
            None,
        )?;
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].source.as_slice(), source);
        let snapshot = CompilerWorkspaceSnapshot::open(&root)?;
        assert!(snapshot.revalidate()?);
        fs::remove_dir_all(&root).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn compiler_materialization_cancellation_stops_before_the_next_source_read()
    -> Result<(), String> {
        let root = scratch_dir("handoff-cancel")?;
        let first = b"pub fn first() {}\n";
        let second = b"pub fn second() {}\n";
        fs::write(root.join("a.rs"), first).map_err(|error| error.to_string())?;
        fs::write(root.join("b.rs"), second).map_err(|error| error.to_string())?;
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let scan = scan_with_source_policy(&root, [34; 32], &BTreeMap::new(), policy)?;
        fs::write(root.join("b.rs"), b"changed after scan\n").map_err(|error| error.to_string())?;
        let capability = scan.revision_fence.open_bound_root(&root)?;
        let cancellation = AtomicBool::new(false);
        let mut completed = 0;
        let error = match materialize_compiler_sources(
            &capability,
            scan.compiler_sources,
            scan.reused_compiler_files,
            scan.source_admission_policy,
            Some(&cancellation),
            |count| {
                completed = count;
                if count == 1 {
                    cancellation.store(true, Ordering::Release);
                }
            },
        ) {
            Err(error) => error,
            Ok(_) => {
                return Err(
                    "cancellation after the first handle admitted the remaining source".to_owned(),
                );
            }
        };
        assert_eq!(completed, 1);
        assert_eq!(error, INDEX_SCAN_CANCELLED);
        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_reads_only_an_edited_file_and_handles_rename_delete() -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-edit-rename",
            &[
                ("lib.rs", b"pub fn kept() {}\n"),
                ("old.rs", b"pub fn moved() {}\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);
        let edited = b"pub fn kept() { let _ = 1; }\n";
        fs::write(root.join("lib.rs"), edited).map_err(|error| error.to_string())?;
        let delta = scan_project(root_text, project, &reusable)?;
        assert_eq!(delta.source_bytes_read, edited.len());
        assert_eq!(delta.reused_compiler_files.len(), 1);

        git_test_command(root, &["mv", "old.rs", "new.rs"])?;
        let renamed = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            renamed.source_bytes_read,
            edited.len() + b"pub fn moved() {}\n".len()
        );
        assert!(renamed.files.iter().all(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path != "old.rs")
        }));
        assert!(renamed.files.iter().any(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path == "new.rs")
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_falls_back_for_new_untracked_source_and_ignore_policy_change()
    -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-untracked-policy",
            &[
                (".gitignore", b"/hidden_sources/\n"),
                ("lib.rs", b"pub fn stable() {}\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);

        let extra = b"pub fn untracked() {}\n";
        fs::write(root.join("extra.rs"), extra).map_err(|error| error.to_string())?;
        let with_untracked = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            with_untracked.source_bytes_read,
            b"pub fn stable() {}\n".len() + extra.len()
        );
        assert!(with_untracked.files.iter().any(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path == "extra.rs")
        }));

        fs::remove_file(root.join("extra.rs")).map_err(|error| error.to_string())?;
        fs::create_dir_all(root.join("hidden_sources")).map_err(|error| error.to_string())?;
        let newly_selected = b"pub fn now_visible() {}\n";
        fs::write(root.join("hidden_sources/generated.rs"), newly_selected)
            .map_err(|error| error.to_string())?;
        let policy_change = b"# generated sources are now visible\n";
        fs::write(root.join(".gitignore"), policy_change).map_err(|error| error.to_string())?;
        let changed_policy = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            changed_policy.source_bytes_read,
            b"pub fn stable() {}\n".len() + newly_selected.len()
        );
        assert!(changed_policy.files.iter().any(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path == "hidden_sources/generated.rs")
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_witnesses_a_local_ignore_file_hidden_by_gitignore() -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-hidden-ignore-policy",
            &[
                (".gitignore", b"/.ignore\n"),
                ("lib.rs", b"pub fn stable() {}\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        fs::create_dir_all(root.join("hidden_sources")).map_err(|error| error.to_string())?;
        let hidden = b"pub fn became_visible() {}\n";
        fs::write(root.join("hidden_sources/generated.rs"), hidden)
            .map_err(|error| error.to_string())?;
        fs::write(root.join(".ignore"), b"hidden_sources/\n").map_err(|error| error.to_string())?;

        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        assert_eq!(cold.files.len(), 1);
        let reusable = reusable_rows(&cold);
        fs::write(root.join(".ignore"), b"# source selection changed\n")
            .map_err(|error| error.to_string())?;

        let changed_policy = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            changed_policy.source_bytes_read,
            b"pub fn stable() {}\n".len() + hidden.len()
        );
        assert!(changed_policy.files.iter().any(|(_, record)| {
            record
                .file_fields()
                .is_some_and(|fields| fields.path == "hidden_sources/generated.rs")
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_falls_back_when_a_selected_path_is_a_symlink() -> Result<(), String> {
        use std::os::unix::fs::symlink;

        let Some(repository) =
            git_test_repo("frontier-symlink", &[("lib.rs", b"pub fn stable() {}\n")])?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);
        let outside = root
            .parent()
            .ok_or("missing scratch parent")?
            .join("outside.rs");
        fs::write(&outside, b"pub fn outside() {}\n").map_err(|error| error.to_string())?;
        symlink(&outside, root.join("linked.rs")).map_err(|error| error.to_string())?;

        let fallback = scan_project(root_text, project, &reusable)?;
        assert_eq!(fallback.source_bytes_read, b"pub fn stable() {}\n".len());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_detects_mutation_after_a_zero_read_reuse_attempt() -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-race",
            &[
                ("lib.rs", b"pub fn old_value() {}\n"),
                ("kept.rs", b"pub fn kept() {}\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);
        let changed = b"pub fn new_value() {}\n";
        source_frontier::install_source_frontier_race(
            root.clone(),
            root.join("lib.rs"),
            changed.to_vec(),
        );

        let raced = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            raced.source_bytes_read,
            changed.len() + b"pub fn kept() {}\n".len()
        );
        assert!(raced.compiler_sources.iter().any(|source| {
            source.relative_path == "lib.rs"
                && source.content == typed_of::<InputContentSchema>(changed).to_bytes()
        }));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn git_frontier_full_retry_releases_fresh_rows_and_preserves_complete_facts()
    -> Result<(), String> {
        let kept = b"pub fn kept_before_race() {}\n";
        let initial = b"pub fn before_edit() {}\n";
        let repository = git_test_repo(
            "frontier-release-fresh-rows",
            &[("a_fresh.rs", initial), ("b_raced.rs", kept)],
        )?
        .ok_or("Git is required for the physical frontier retry control")?;
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let initial_scan = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&initial_scan);
        let mut fresh = String::new();
        for index in 0..180 {
            fresh.push_str(&format!(
                "/// {}\npub fn retained_fact_{index}() -> u64 {{ {index} }}\n",
                "complete documentation and source evidence ".repeat(24)
            ));
        }
        fs::write(root.join("a_fresh.rs"), fresh.as_bytes()).map_err(|error| error.to_string())?;
        let raced_bytes = b"pub fn after_race(value: u64) -> u64 { value + 1 }\n";
        source_frontier::install_source_frontier_race(
            root.clone(),
            root.join("b_raced.rs"),
            raced_bytes.to_vec(),
        );

        // The delta attempt freshly parses a_fresh.rs and reuses b_raced.rs.
        // The actual race forces the full retry. Its cfg(test) Weak check at
        // the retry boundary rejects retaining the fresh first-attempt rows.
        let raced = scan_project(root_text, project, &reusable)?;
        assert_eq!(
            raced.source_bytes_read,
            fresh.len() + fresh.len() + raced_bytes.len(),
            "retain the first attempt read charge and every full-retry read"
        );
        let fresh_facts = raced
            .source_facts
            .iter()
            .find(|update| update.manifest().path() == "a_fresh.rs")
            .ok_or("full fresh source facts missing")?;
        assert_eq!(fresh_facts.declaration_count(), 181);
        assert!(
            !fresh_facts.pages().is_empty(),
            "exercise complete paged facts"
        );
        assert!(raced.compiler_sources.iter().any(|source| {
            source.relative_path == "b_raced.rs"
                && source.content == typed_of::<InputContentSchema>(raced_bytes).to_bytes()
        }));
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        assert_eq!(cold.source_bytes_read, fresh.len() + raced_bytes.len());
        assert_eq!(raced.source_version, cold.source_version);
        assert_eq!(raced.files, cold.files, "same exact compact source rows");
        assert_eq!(
            raced.source_facts, cold.source_facts,
            "same complete fact trees"
        );
        assert_eq!(raced.encoded_fact_bytes, cold.encoded_fact_bytes);
        assert_eq!(raced.compiler_sources.len(), cold.compiler_sources.len());
        for (actual, expected) in raced.compiler_sources.iter().zip(&cold.compiler_sources) {
            assert_eq!(actual.relative_path, expected.relative_path);
            assert_eq!(actual.content, expected.content);
            assert_eq!(actual.source_fact_identity, expected.source_fact_identity);
            assert_eq!(actual.profile, expected.profile);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn cold_frontier_reopen_uses_the_full_source_read() -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-cold-reopen",
            &[("lib.rs", b"pub fn stable() {}\n")],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);
        source_frontier::clear_source_frontier_for(root, project);

        let reopened = scan_project(root_text, project, &reusable)?;
        assert_eq!(reopened.source_bytes_read, b"pub fn stable() {}\n".len());
        assert_eq!(reopened.source_version, cold.source_version);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn raw_crlf_lf_change_with_the_same_git_blob_is_reread() -> Result<(), String> {
        let Some(repository) = git_test_repo(
            "frontier-line-endings",
            &[
                (".gitattributes", b"*.rs text\n"),
                ("lib.rs", b"pub fn first() {}\npub fn second() {}\r\n"),
            ],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        git_test_command(root, &["config", "core.autocrlf", "true"])?;
        git_test_command(root, &["add", "--renormalize", "."])?;
        git_test_command(root, &["commit", "--amend", "--no-edit"])?;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let original = fs::read(root.join("lib.rs")).map_err(|error| error.to_string())?;
        let blob_before = git_test_command(root, &["ls-files", "--stage", "--", "lib.rs"])?;
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let reusable = reusable_rows(&cold);

        let swapped = b"pub fn first() {}\r\npub fn second() {}\n";
        assert_eq!(original.len(), swapped.len());
        fs::write(root.join("lib.rs"), swapped).map_err(|error| error.to_string())?;
        git_test_command(root, &["add", "--", "lib.rs"])?;
        let blob_after = git_test_command(root, &["ls-files", "--stage", "--", "lib.rs"])?;
        assert_eq!(blob_before, blob_after);
        assert!(git_test_command(root, &["status", "--porcelain"])?.is_empty());

        let changed = scan_project(root_text, project, &reusable)?;
        assert_eq!(changed.source_bytes_read, swapped.len());
        let old = cold.files[0]
            .1
            .file_fields()
            .ok_or("expected original source record")?
            .content_version;
        let new = changed.files[0]
            .1
            .file_fields()
            .ok_or("expected changed source record")?
            .content_version;
        assert_ne!(
            old, new,
            "raw line endings are part of the admitted source bytes"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    #[ignore = "manual large-fixture measurement; reports source bytes and wall time"]
    fn benchmark_git_frontier_reads_large_fixture_once_then_reuses_all_rows() -> Result<(), String>
    {
        use std::time::Instant;

        let Some(repository) = git_test_repo(
            "frontier-large-benchmark",
            &[("src/lib.rs", b"pub fn root() {}\n")],
        )?
        else {
            return Ok(());
        };
        let root = &repository.0;
        fs::create_dir_all(root.join("src/generated")).map_err(|error| error.to_string())?;
        for index in 0..1024 {
            let text = format!(
                "pub fn item_{index}() -> usize {{ {index} }}\n// {}\n",
                "x".repeat(4_000)
            );
            fs::write(root.join(format!("src/generated/item_{index}.rs")), text)
                .map_err(|error| error.to_string())?;
        }
        git_test_commit(root, "large source fixture")?;
        let root_text = root.to_str().ok_or("non-UTF-8 Git path")?;
        let project = git_project_key(root);
        let cold_start = Instant::now();
        let cold = scan_project(root_text, project, &BTreeMap::new())?;
        let cold_elapsed = cold_start.elapsed();
        let reusable = reusable_rows(&cold);
        let warm_start = Instant::now();
        let warm = scan_project(root_text, project, &reusable)?;
        let warm_elapsed = warm_start.elapsed();
        assert_eq!(cold.files.len(), 1025);
        assert_eq!(warm.source_bytes_read, 0);
        eprintln!(
            "source_frontier_benchmark files={} cold_bytes={} warm_bytes={} cold_ms={} warm_ms={}",
            cold.files.len(),
            cold.source_bytes_read,
            warm.source_bytes_read,
            cold_elapsed.as_millis(),
            warm_elapsed.as_millis()
        );
        Ok(())
    }

    #[test]
    fn a_changed_language_does_not_reread_an_unchanged_one() -> Result<(), String> {
        let scratch = scratch_dir("language-delta")?;
        let root = scratch.to_str().ok_or("non-UTF-8 scratch path")?;
        let rust_v1 = "pub fn ferris() {}";
        let rust_v2 = "pub fn ferris() { let _ = 1; }";
        let python = "def monty():\n    pass\n";
        fs::write(scratch.join("lib.rs"), rust_v1).map_err(|error| error.to_string())?;
        fs::write(scratch.join("app.py"), python).map_err(|error| error.to_string())?;
        let project = [14; 32];
        let cold = scan_project(root, project, &BTreeMap::new())?;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        fs::write(scratch.join("lib.rs"), rust_v2).map_err(|error| error.to_string())?;
        let delta = scan_project(root, project, &reusable)?;
        fs::write(scratch.join("app.py"), b"def monty():\n    return 9\n")
            .map_err(|error| error.to_string())?;
        let present = present_compiler_paths(&delta.compiler_sources, &delta.reused_compiler_files);
        let lost = lost_compiler_profiles(scratch.as_path(), &reusable, &present)?;
        assert!(lost.is_empty(), "{lost:?}");
        let skipped = delta.reused_compiler_files.len();
        let (fresh, reused) = select_compiler_inputs(
            scratch.as_path(),
            delta.compiler_sources,
            delta.reused_compiler_files,
            &lost,
        );
        assert_eq!(reused.len(), 0);
        let admitted = admit_compiler_sources_with_policy(
            scratch.as_path(),
            fresh,
            reused,
            delta.source_admission_policy,
        )?;
        assert_eq!(
            admitted
                .iter()
                .map(|source| (
                    source.relative_path.as_str(),
                    std::str::from_utf8(&source.source).expect("UTF-8 fixture")
                ))
                .collect::<Vec<_>>(),
            [("lib.rs", rust_v2)]
        );
        eprintln!(
            "profile_delta languages=2 changed=rust selected_files=1 skipped_reused={skipped}"
        );
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[test]
    fn a_deleted_language_does_not_reread_the_languages_that_remain() -> Result<(), String> {
        let scratch = scratch_dir("language-delete")?;
        let root = scratch.to_str().ok_or("non-UTF-8 scratch path")?;
        fs::write(scratch.join("lib.rs"), b"pub fn ferris() {}")
            .map_err(|error| error.to_string())?;
        fs::write(scratch.join("app.py"), b"def monty():\n    pass\n")
            .map_err(|error| error.to_string())?;
        let project = [15; 32];
        let cold = scan_project(root, project, &BTreeMap::new())?;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        fs::remove_file(scratch.join("app.py")).map_err(|error| error.to_string())?;
        let delta = scan_project(root, project, &reusable)?;
        fs::write(scratch.join("lib.rs"), b"pub fn torn() {}")
            .map_err(|error| error.to_string())?;
        let present = present_compiler_paths(&delta.compiler_sources, &delta.reused_compiler_files);
        let lost = lost_compiler_profiles(scratch.as_path(), &reusable, &present)?;
        assert!(
            lost.iter()
                .any(|profile| matches!(profile, LanguageProfile::Python(_)))
        );
        assert!(
            !lost
                .iter()
                .any(|profile| matches!(profile, LanguageProfile::Rust(_)))
        );
        let (fresh, reused) = select_compiler_inputs(
            scratch.as_path(),
            delta.compiler_sources,
            delta.reused_compiler_files,
            &lost,
        );
        assert!(fresh.is_empty());
        assert!(reused.is_empty());
        let admitted = admit_compiler_sources_with_policy(
            scratch.as_path(),
            fresh,
            reused,
            delta.source_admission_policy,
        )?;
        assert!(admitted.is_empty());
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    #[test]
    fn one_rust_edition_does_not_reread_another_crate() -> Result<(), String> {
        let scratch = scratch_dir("edition-delta")?;
        let root = scratch.to_str().ok_or("non-UTF-8 scratch path")?;
        fs::create_dir_all(scratch.join("old/src")).map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.join("new/src")).map_err(|error| error.to_string())?;
        fs::write(
            scratch.join("old/Cargo.toml"),
            "[package]\nname = \"old\"\nedition = \"2018\"\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.join("new/Cargo.toml"),
            "[package]\nname = \"new\"\nedition = \"2021\"\n",
        )
        .map_err(|error| error.to_string())?;
        let old_v1 = "pub fn old_crate() {}";
        let old_v2 = "pub fn old_crate() { let _ = 1; }";
        let new_source = "pub fn new_crate() {}";
        fs::write(scratch.join("old/src/lib.rs"), old_v1).map_err(|error| error.to_string())?;
        fs::write(scratch.join("new/src/lib.rs"), new_source).map_err(|error| error.to_string())?;
        let project = [16; 32];
        let cold = scan_project(root, project, &BTreeMap::new())?;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        fs::write(scratch.join("old/src/lib.rs"), old_v2).map_err(|error| error.to_string())?;
        let delta = scan_project(root, project, &reusable)?;
        fs::write(scratch.join("new/src/lib.rs"), b"pub fn torn() {}")
            .map_err(|error| error.to_string())?;
        let present = present_compiler_paths(&delta.compiler_sources, &delta.reused_compiler_files);
        let lost = lost_compiler_profiles(scratch.as_path(), &reusable, &present)?;
        assert!(lost.is_empty(), "{lost:?}");
        let (fresh, reused) = select_compiler_inputs(
            scratch.as_path(),
            delta.compiler_sources,
            delta.reused_compiler_files,
            &lost,
        );
        assert!(reused.is_empty());
        let admitted = admit_compiler_sources_with_policy(
            scratch.as_path(),
            fresh,
            reused,
            delta.source_admission_policy,
        )?;
        assert_eq!(admitted.len(), 1);
        assert_eq!(admitted[0].relative_path, "old/src/lib.rs");
        assert_eq!(admitted[0].source.as_slice(), old_v2.as_bytes());
        assert_eq!(
            compilation_profile(
                scratch.as_path(),
                &admitted[0].relative_path,
                admitted[0].profile
            ),
            LanguageProfile::Rust(RustEdition::Rust2018)
        );
        assert_eq!(
            compilation_profile(
                scratch.as_path(),
                "new/src/lib.rs",
                LanguageProfile::Rust(RustEdition::Rust2024)
            ),
            LanguageProfile::Rust(RustEdition::Rust2021)
        );
        eprintln!("edition_delta crates=2 selected_files=1 skipped_crate=2021");
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }

    fn write_mixed_edition_profile_count_fixture(
        root: &Path,
    ) -> Result<BTreeMap<LanguageProfile, usize>, String> {
        let profiles = [
            ("2015", RustEdition::Rust2015),
            ("2018", RustEdition::Rust2018),
            ("2021", RustEdition::Rust2021),
            ("2024", RustEdition::Rust2024),
        ];
        for (edition, _) in profiles {
            let directory = root.join(format!("crate_{edition}"));
            fs::create_dir_all(directory.join("src")).map_err(|error| error.to_string())?;
            fs::write(
                directory.join("Cargo.toml"),
                format!("[package]\nname = \"crate_{edition}\"\nedition = \"{edition}\"\n"),
            )
            .map_err(|error| error.to_string())?;
            fs::write(directory.join("src/lib.rs"), "pub fn original() {}\n")
                .map_err(|error| error.to_string())?;
        }
        Ok(profiles
            .into_iter()
            .map(|(_, edition)| (LanguageProfile::Rust(edition), 1))
            .collect::<BTreeMap<_, _>>())
    }

    #[test]
    fn compiler_profile_counts_follow_cargo_editions_for_fresh_reused_and_new_sources()
    -> Result<(), String> {
        let scratch = scratch_dir("profile-source-counts")?;
        let root = scratch.to_str().ok_or("non-UTF-8 scratch path")?;
        let expected = write_mixed_edition_profile_count_fixture(&scratch)?;
        let project = [73; 32];
        let cold = scan_project(root, project, &BTreeMap::new())?;
        assert_eq!(cold.compiler_sources.len(), 4);
        assert!(cold.reused_compiler_files.is_empty());
        assert!(
            cold.compiler_sources
                .iter()
                .all(|source| { source.profile == LanguageProfile::Rust(RustEdition::Rust2024) }),
            "extension discovery defaults must not be confused with Cargo edition"
        );
        assert_eq!(
            compiler_profile_source_counts(
                &scratch,
                &cold.compiler_sources,
                &cold.reused_compiler_files,
            ),
            expected,
        );
        let previous = cold.files.into_iter().collect::<BTreeMap<_, _>>();
        let warm = scan_project(root, project, &previous)?;
        assert!(warm.compiler_sources.is_empty());
        assert_eq!(warm.reused_compiler_files.len(), 4);
        assert_eq!(
            compiler_profile_source_counts(
                &scratch,
                &warm.compiler_sources,
                &warm.reused_compiler_files,
            ),
            expected,
        );
        fs::write(
            scratch.join("crate_2018/src/lib.rs"),
            "pub fn edited() {}\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.join("crate_2021/src/extra.rs"),
            "pub fn added() {}\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.join("crate_2021/src/lib.rs"),
            "mod extra;\npub fn original() {}\n",
        )
        .map_err(|error| error.to_string())?;
        let changed = scan_project(root, project, &previous)?;
        assert_eq!(changed.compiler_sources.len(), 3);
        assert_eq!(changed.reused_compiler_files.len(), 2);
        let mut expected = expected;
        expected.insert(LanguageProfile::Rust(RustEdition::Rust2021), 2);
        assert_eq!(
            compiler_profile_source_counts(
                &scratch,
                &changed.compiler_sources,
                &changed.reused_compiler_files,
            ),
            expected,
            "fresh additions and retained files count together under their actual editions",
        );
        let previous = changed.files.into_iter().collect::<BTreeMap<_, _>>();
        fs::remove_file(scratch.join("crate_2015/src/lib.rs"))
            .map_err(|error| error.to_string())?;
        let removed = scan_project(root, project, &previous)?;
        let counts = compiler_profile_source_counts(
            &scratch,
            &removed.compiler_sources,
            &removed.reused_compiler_files,
        );
        assert!(!counts.contains_key(&LanguageProfile::Rust(RustEdition::Rust2015)));
        assert_eq!(
            counts
                .get(&LanguageProfile::Rust(RustEdition::Rust2015))
                .copied()
                .unwrap_or(0),
            0
        );
        assert_eq!(
            live_compiler_profiles(
                &scratch,
                &removed.compiler_sources,
                &removed.reused_compiler_files
            ),
            counts.keys().copied().collect::<BTreeSet<_>>(),
        );
        let present =
            present_compiler_paths(&removed.compiler_sources, &removed.reused_compiler_files);
        assert!(
            lost_compiler_profiles(&scratch, &previous, &present)?
                .contains(&LanguageProfile::Rust(RustEdition::Rust2015))
        );
        fs::remove_dir_all(&scratch).map_err(|error| error.to_string())?;
        Ok(())
    }

    #[test]
    fn a_deleted_crate_retires_its_edition_after_the_manifest_is_gone() -> Result<(), String> {
        let scratch = scratch_dir("retire-edition")?;
        let root = scratch.to_str().ok_or("non-UTF-8 scratch path")?;
        fs::create_dir_all(scratch.join("old/src")).map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.join("new/src")).map_err(|error| error.to_string())?;
        fs::write(
            scratch.join("old/Cargo.toml"),
            "[package]\nname = \"old\"\nedition = \"2018\"\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(
            scratch.join("new/Cargo.toml"),
            "[package]\nname = \"new\"\nedition = \"2021\"\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(scratch.join("old/src/lib.rs"), b"pub fn old_crate() {}")
            .map_err(|error| error.to_string())?;
        fs::write(scratch.join("new/src/lib.rs"), b"pub fn new_crate() {}")
            .map_err(|error| error.to_string())?;
        fs::write(scratch.join("app.py"), b"def monty():\n    pass\n")
            .map_err(|error| error.to_string())?;
        let project = [17; 32];
        let cold = scan_project(root, project, &BTreeMap::new())?;
        let cold_live = live_compiler_profiles(
            scratch.as_path(),
            &cold.compiler_sources,
            &cold.reused_compiler_files,
        );
        let rust_2018 = LanguageProfile::Rust(RustEdition::Rust2018);
        let rust_2021 = LanguageProfile::Rust(RustEdition::Rust2021);
        assert!(cold_live.contains(&rust_2018));
        assert!(cold_live.contains(&rust_2021));
        let python = *cold_live
            .iter()
            .find(|profile| matches!(profile, LanguageProfile::Python(_)))
            .ok_or("python profile missing from the cold scan")?;
        let reusable = cold.files.iter().cloned().collect::<BTreeMap<_, _>>();
        fs::remove_dir_all(scratch.join("old")).map_err(|error| error.to_string())?;
        let delta = scan_project(root, project, &reusable)?;
        let live = live_compiler_profiles(
            scratch.as_path(),
            &delta.compiler_sources,
            &delta.reused_compiler_files,
        );
        assert!(!live.contains(&rust_2018));
        assert!(live.contains(&rust_2021));
        assert!(live.contains(&python));
        let selected = [rust_2018, rust_2021, python];
        let retired = retired_selected_profiles(&selected, &live);
        assert_eq!(retired.into_iter().collect::<Vec<_>>(), vec![rust_2018]);
        let present = present_compiler_paths(&delta.compiler_sources, &delta.reused_compiler_files);
        let lost = lost_compiler_profiles(scratch.as_path(), &reusable, &present)?;
        let (fresh, reused) = select_compiler_inputs(
            scratch.as_path(),
            delta.compiler_sources,
            delta.reused_compiler_files,
            &lost,
        );
        let compiled = live_compiler_profiles(scratch.as_path(), &fresh, &reused);
        assert!(compiled.is_subset(&live));
        assert!(!compiled.contains(&rust_2018));
        eprintln!(
            "retire_edition live={} retired=1 compiled={}",
            live.len(),
            compiled.len()
        );
        let _ = fs::remove_dir_all(&scratch);
        Ok(())
    }
}

/// Regression laws for the canonical single-row capacity of source records.
///
/// A real source file routinely extracts more declaration detail than one
/// canonical relation row can hold. Before `file_within_row_capacity` existed,
/// ingest admitted such a record and the whole project failed later, when the
/// relation delta was prepared, with an opaque
/// `workspace relation node was rejected`. `memchr 2.8.3` reproduces it with a
/// single file: `src/arch/x86_64/avx2/memchr.rs` extracts 125 declarations
/// carrying 49 452 bytes of excerpt, for a 65 686 byte row against a 65 464
/// byte capacity.
///
/// These tests assert the rendered content of the resulting record, not row
/// counts: a scan that silently dropped every declaration would keep the same
/// file count while erasing everything a surface can show.
#[cfg(test)]
mod row_capacity_tests {
    use super::*;
    use backend_engine::{DeclarationRetention, ProductSourceRecord};
    use std::fmt::Write as _;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Result<Scratch, String> {
        let unique = format!(
            "backend-row-capacity-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        Ok(Scratch(directory))
    }

    /// Writes a Rust file whose extracted declarations exceed one row.
    ///
    /// Each function carries a documentation block and a body wide enough to
    /// produce a substantial excerpt, so the full record passes the canonical
    /// capacity while every individual declaration stays small.
    fn dense_source(functions: usize) -> String {
        let mut source = String::new();
        for index in 0..functions {
            let _ = writeln!(
                source,
                "/// Declaration {index} exists to widen the extracted record \
                 beyond one canonical relation row.\n\
                 pub fn declaration_{index}(argument: u64) -> u64 {{"
            );
            for step in 0..24 {
                let _ = writeln!(
                    source,
                    "    let intermediate_value_{step} = \
                     argument.wrapping_mul({step}).wrapping_add({index});"
                );
            }
            source.push_str("    argument\n}\n\n");
        }
        source
    }

    fn encoded_bytes(record: &ProductSourceRecord) -> usize {
        let mut bytes = Vec::new();
        ProductSourceRelation::encode_value(record, &mut bytes);
        bytes.len()
    }

    #[test]
    fn a_file_denser_than_one_row_is_indexed_rather_than_rejected() -> Result<(), String> {
        let scratch = scratch("dense")?;
        let source = dense_source(140);
        fs::write(scratch.0.join("dense.rs"), source.as_bytes())
            .map_err(|error| error.to_string())?;
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;

        let scan = scan_project(root, [3; 32], &BTreeMap::new())?;

        let (_, record) = scan.files.first().ok_or("dense file was not scanned")?;
        let fields = record.file_fields().ok_or("expected a file record")?;

        let encoded = encoded_bytes(record);
        assert!(
            encoded <= ProductSourceRecord::ROW_VALUE_CAPACITY,
            "a scanned record must fit one canonical row: {encoded} bytes against a \
             {} byte capacity",
            ProductSourceRecord::ROW_VALUE_CAPACITY
        );
        assert_ne!(
            fields.retention,
            DeclarationRetention::Complete,
            "a record that had to shed detail must not claim complete retention"
        );
        assert_eq!(
            fields.retention,
            DeclarationRetention::ExcerptsElided,
            "shedding excerpts alone must be enough for a dense but ordinary file"
        );

        // Content, not counts: every extracted name must still be renderable.
        let names = fields
            .declarations
            .iter()
            .map(|declaration| declaration.name().to_owned())
            .collect::<Vec<_>>();
        assert!(
            names.iter().any(|name| name == "declaration_0"),
            "the first declaration name must survive shedding, got {names:?}"
        );
        assert!(
            names.iter().any(|name| name == "declaration_139"),
            "the last declaration name must survive shedding, got {names:?}"
        );
        assert!(
            fields
                .declarations
                .iter()
                .all(|declaration| !declaration.signature().is_empty()),
            "shedding excerpts must not also erase signatures"
        );
        Ok(())
    }

    #[test]
    fn a_file_within_one_row_keeps_complete_retention_and_its_excerpts() -> Result<(), String> {
        let scratch = scratch("small")?;
        fs::write(
            scratch.0.join("small.rs"),
            b"/// One small declaration.\npub fn ferris(value: u64) -> u64 { value }\n",
        )
        .map_err(|error| error.to_string())?;
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;

        let scan = scan_project(root, [4; 32], &BTreeMap::new())?;
        let (_, record) = scan.files.first().ok_or("small file was not scanned")?;
        let fields = record.file_fields().ok_or("expected a file record")?;

        assert_eq!(
            fields.retention,
            DeclarationRetention::Complete,
            "a file that fits must not be reported as reduced"
        );
        let declaration = fields
            .declarations
            .iter()
            .find(|declaration| declaration.name() == "ferris")
            .ok_or_else(|| {
                format!(
                    "expected a `ferris` declaration, got {:?}",
                    fields
                        .declarations
                        .iter()
                        .map(backend_compile::SourceDeclaration::name)
                        .collect::<Vec<_>>()
                )
            })?;
        assert!(
            matches!(
                declaration.source_excerpt(),
                backend_compile::SourceExcerpt::Captured { .. }
            ),
            "a file that fits must keep its captured excerpt"
        );
        Ok(())
    }

    #[test]
    fn shedding_is_a_pure_function_of_the_source() -> Result<(), String> {
        let scratch = scratch("stable")?;
        fs::write(scratch.0.join("dense.rs"), dense_source(140).as_bytes())
            .map_err(|error| error.to_string())?;
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;

        let first = scan_project(root, [5; 32], &BTreeMap::new())?;
        let second = scan_project(root, [5; 32], &BTreeMap::new())?;

        assert_eq!(
            first.source_version, second.source_version,
            "an unchanged project must produce an unchanged source version"
        );
        assert_eq!(
            first.files, second.files,
            "an unchanged project must produce byte-identical rows, \
             otherwise re-indexing churns the content-addressed relation"
        );
        Ok(())
    }
}

/// Robustness laws: every failure a real project can cause is typed and
/// survivable.
///
/// A checkout in the wild contains files that cannot be read, files that are
/// not text, files that are too large, symlink cycles, and paths outside
/// ASCII. Before these laws, each of those aborted the entire scan with a
/// single string, so one bad file made a whole project permanently
/// unindexable. Each case now yields a typed per-file reason while every
/// other file still indexes.
///
/// The assertions are on rendered content - the surviving file's declaration
/// names and the failing file's typed reason - because a scan that dropped
/// every declaration would keep the same file count.
#[cfg(all(test, unix))]
mod robustness_tests {
    use super::*;
    use backend_engine::{DeclarationRetention, SourceUnavailableReason};

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::set_permissions(
                self.0.join("broken.rs"),
                <fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o644),
            );
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Result<Scratch, String> {
        let unique = format!(
            "backend-robust-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        Ok(Scratch(directory))
    }

    fn good_source() -> &'static [u8] {
        b"/// A readable declaration.\npub fn ferris(value: u64) -> u64 { value }\n"
    }

    /// Returns the retention claimed for one project-relative path.
    fn retention_of(scan: &IndexSnapshot, path: &str) -> Option<DeclarationRetention> {
        scan.files.iter().find_map(|(_, record)| {
            let fields = record.file_fields()?;
            (fields.path == path).then_some(fields.retention)
        })
    }

    fn names_of(scan: &IndexSnapshot, path: &str) -> Vec<String> {
        scan.files
            .iter()
            .find_map(|(_, record)| {
                let fields = record.file_fields()?;
                (fields.path == path).then(|| {
                    fields
                        .declarations
                        .iter()
                        .map(|declaration| declaration.name().to_owned())
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default()
    }

    fn scan(scratch: &Scratch) -> Result<IndexSnapshot, String> {
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        scan_project(root, [8; 32], &BTreeMap::new())
    }

    #[test]
    fn python_unavailable_resources_keep_baseline_rows_and_exact_compiler_frontier()
    -> Result<(), String> {
        let scratch = scratch("python-raw-frontier")?;
        let files: [(&str, &[u8]); 3] = [
            ("good.py", b"def answer():\n    return 42\n"),
            ("invalid.pyi", b"x y\n"),
            ("latin.py", b"# coding: latin-1\nname = 'caf\xe9'\n"),
        ];
        for (path, bytes) in files {
            fs::write(scratch.0.join(path), bytes).map_err(|e| e.to_string())?;
        }
        let snapshot = scan(&scratch)?;
        assert_eq!(
            retention_of(&snapshot, "invalid.pyi"),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::Unparsed
            ))
        );
        assert_eq!(
            retention_of(&snapshot, "latin.py"),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::NotText
            ))
        );
        assert!(
            names_of(&snapshot, "good.py")
                .iter()
                .any(|name| name == "answer")
        );
        assert_eq!(snapshot.compiler_sources.len(), files.len());
        for handle in &snapshot.compiler_sources {
            let bytes = files
                .iter()
                .find(|(path, _)| *path == handle.relative_path)
                .ok_or("compiler handle does not match the retained raw fixture")?
                .1;
            assert_eq!(
                handle.content,
                typed_of::<InputContentSchema>(bytes).to_bytes()
            );
            assert_eq!(
                handle.source_fact_identity,
                SourceFactIdentity::from_canonical_bytes(bytes)
            );
        }
        let admitted = admit_compiler_sources_for_scan(
            &scratch.0,
            &snapshot.revision_fence,
            snapshot.compiler_sources,
            snapshot.reused_compiler_files,
            snapshot.source_admission_policy,
            None,
        )?;
        assert_eq!(admitted.len(), files.len());
        for source in &admitted {
            let bytes = files
                .iter()
                .find(|(path, _)| *path == source.relative_path)
                .ok_or("compiler source does not match the retained raw fixture")?
                .1;
            assert_eq!(source.source, bytes);
        }
        Ok(())
    }

    /// An export macro between `class` and its name is valid after native
    /// preprocessing, but the structural grammar rejects it. The real pugixml
    /// header was dropped here before libclang could observe its declarations.
    #[test]
    fn cpp_declaration_macros_use_exact_native_candidate_frontier() -> Result<(), String> {
        let scratch = scratch("cpp-macro-frontier")?;
        let source: &[u8] = b"#define PUGIXML_CLASS\nnamespace pugi {\nclass PUGIXML_CLASS xml_document {};\nstruct PUGIXML_CLASS xml_parse_result { int status; };\n}\n";
        let path = "pugixml.hpp";
        fs::write(scratch.0.join(path), source).map_err(|e| e.to_string())?;
        let frontend = backend_frontend_clang::syntax_frontend().map_err(|e| e.to_string())?;
        assert!(matches!(
            frontend.analyze(Path::new(path), source),
            Err(SyntaxError::MalformedSource(SourceLanguage::Clang))
        ));
        let first = scan(&scratch)?;
        assert_eq!(
            retention_of(&first, path),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::Unparsed
            ))
        );
        assert!(names_of(&first, path).is_empty());
        // A repeat scan of the unavailable browsing row must still retain the
        // input, rather than reusing that row as an empty semantic program.
        let reusable = first.files.iter().cloned().collect::<BTreeMap<_, _>>();
        let second = scan_project(
            scratch.0.to_str().ok_or("non-UTF-8 scratch path")?,
            [8; 32],
            &reusable,
        )?;
        for snapshot in [first, second] {
            assert!(
                snapshot.compiler_sources.is_empty(),
                "a Clang malformed candidate must not fall through the ordinary compiler-source lane"
            );
            assert!(snapshot.reused_compiler_files.is_empty());
            assert_eq!(snapshot.native_clang_candidates.len(), 1);
            let handle = &snapshot.native_clang_candidates[0];
            assert_eq!(handle.relative_path, path);
            assert!(matches!(handle.profile, LanguageProfile::Cxx(_)));
            assert_eq!(
                handle.content,
                typed_of::<InputContentSchema>(source).to_bytes()
            );
            assert_eq!(
                handle.source_fact_identity,
                SourceFactIdentity::from_canonical_bytes(source)
            );
            assert_eq!(handle.source_bytes.as_deref(), Some(source));
            let expected_byte_length =
                u32::try_from(source.len()).map_err(|error| error.to_string())?;
            let expected_chunk_count =
                u32::try_from(source.len().div_ceil(NATIVE_SOURCE_CHUNK_BYTES))
                    .map_err(|error| error.to_string())?;
            assert!(
                snapshot.native_frontier.iter().any(|record| matches!(
                    record,
                    ProductNativeFrontierRecord::Path {
                        project,
                        profile: candidate_profile,
                        path: candidate_path,
                        disposition: NativePathDisposition::ClangMalformed {
                            content,
                            source_identity,
                            byte_length,
                            chunk_count,
                        },
                    } if *project == [8; 32]
                        && candidate_profile == &handle.profile
                        && candidate_path.as_str() == path
                        && content == &handle.content
                        && source_identity == &handle.source_fact_identity
                        && *byte_length == expected_byte_length
                        && *chunk_count == expected_chunk_count
                )),
                "the frontier must commit this exact path, profile, content, source identity, and chunk geometry"
            );
        }
        Ok(())
    }

    #[test]
    fn an_unreadable_file_is_reported_without_failing_the_project() -> Result<(), String> {
        use std::os::unix::fs::PermissionsExt as _;

        let scratch = scratch("unreadable")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let broken = scratch.0.join("broken.rs");
        fs::write(&broken, good_source()).map_err(|e| e.to_string())?;
        fs::set_permissions(&broken, fs::Permissions::from_mode(0o000))
            .map_err(|e| e.to_string())?;
        if fs::read(&broken).is_ok() {
            // Running as a user that bypasses the mode bits; the law is not
            // observable here and a pass would be fabricated.
            return Ok(());
        }

        let scan = scan(&scratch)?;

        assert_eq!(
            retention_of(&scan, "broken.rs"),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::Unreadable
            )),
            "an unreadable file must keep its place with a typed reason"
        );
        assert_eq!(
            retention_of(&scan, "good.rs"),
            Some(DeclarationRetention::Complete),
            "an unreadable neighbour must not degrade a readable file"
        );
        assert!(
            names_of(&scan, "good.rs").iter().any(|n| n == "ferris"),
            "the readable file must still contribute its declarations"
        );
        Ok(())
    }

    #[test]
    fn a_binary_file_is_reported_as_not_text_without_failing_the_project() -> Result<(), String> {
        let scratch = scratch("binary")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let invalid = [0xffu8, 0xfe, 0x00, 0x80, 0x81];
        fs::write(scratch.0.join("blob.rs"), invalid).map_err(|e| e.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(
            scan.source_bytes_read,
            good_source().len() + invalid.len(),
            "invalid UTF-8 remains charged by its exact original byte length"
        );
        assert_eq!(
            retention_of(&scan, "blob.rs"),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::NotText
            )),
            "non-UTF-8 bytes are a fact about one file, not about the project"
        );
        assert!(names_of(&scan, "good.rs").iter().any(|n| n == "ferris"));
        Ok(())
    }

    #[test]
    fn valid_non_ascii_source_keeps_exact_bytes_and_content_identity() -> Result<(), String> {
        let scratch = scratch("non-ascii")?;
        let source = "pub fn greeting() -> &'static str { \"café\" }\n".as_bytes();
        fs::write(scratch.0.join("greeting.rs"), source).map_err(|error| error.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(scan.source_bytes_read, source.len());
        assert_eq!(scan.compiler_sources.len(), 1);
        assert_eq!(
            scan.compiler_sources[0].content,
            typed_of::<InputContentSchema>(source).to_bytes()
        );
        let fields = scan.files[0]
            .1
            .file_fields()
            .ok_or("expected a source file row")?;
        assert_eq!(
            fields.content_version,
            typed_of::<InputContentSchema>(source).to_bytes(),
            "source identity remains based on the exact UTF-8 bytes"
        );
        assert!(
            names_of(&scan, "greeting.rs")
                .iter()
                .any(|name| name == "greeting")
        );
        Ok(())
    }

    #[test]
    fn an_oversized_file_is_reported_as_too_large_without_failing_the_project() -> Result<(), String>
    {
        let scratch = scratch("oversized")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let huge = vec![
            b'\n';
            SourceAdmissionLimits::default()
                .max_file_source_bytes
                .saturating_add(1)
        ];
        fs::write(scratch.0.join("huge.rs"), &huge).map_err(|e| e.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(
            retention_of(&scan, "huge.rs"),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::TooLarge
            )),
            "a file past the per-file limit must be named, not silently dropped"
        );
        assert!(names_of(&scan, "good.rs").iter().any(|n| n == "ferris"));
        Ok(())
    }

    #[test]
    fn gitignored_sources_are_excluded_while_negated_sources_are_admitted() -> Result<(), String> {
        let scratch = scratch("gitignore")?;
        fs::write(
            scratch.0.join(".gitignore"),
            b"ignored.rs\nsrc/*\n!src/keep.rs\n",
        )
        .map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("ignored.rs"), good_source())
            .map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join("src")).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("src/drop.rs"), good_source())
            .map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("src/keep.rs"), good_source())
            .map_err(|error| error.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(retention_of(&scan, "ignored.rs"), None);
        assert_eq!(retention_of(&scan, "src/drop.rs"), None);
        assert_eq!(
            retention_of(&scan, "src/keep.rs"),
            Some(DeclarationRetention::Complete)
        );
        assert_eq!(
            retention_of(&scan, "good.rs"),
            Some(DeclarationRetention::Complete)
        );
        Ok(())
    }

    #[test]
    fn a_symlink_cycle_is_skipped_and_the_project_still_indexes() -> Result<(), String> {
        let scratch = scratch("symlink")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let loop_directory = scratch.0.join("loop");
        std::os::unix::fs::symlink(&scratch.0, &loop_directory).map_err(|e| e.to_string())?;
        std::os::unix::fs::symlink(scratch.0.join("good.rs"), scratch.0.join("alias.rs"))
            .map_err(|e| e.to_string())?;

        let scan = scan(&scratch)?;

        assert!(
            scan.files.iter().all(|(_, record)| record
                .file_fields()
                .is_none_or(|fields| !fields.path.contains("loop"))),
            "a symlinked directory must never be descended into"
        );
        assert_eq!(
            retention_of(&scan, "alias.rs"),
            None,
            "a symlinked file must not be admitted as a second copy of its target"
        );
        assert!(names_of(&scan, "good.rs").iter().any(|n| n == "ferris"));
        Ok(())
    }

    #[test]
    fn a_unicode_path_is_indexed_under_its_exact_coordinate() -> Result<(), String> {
        let scratch = scratch("unicode")?;
        let directory = scratch.0.join("café-日本語");
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        fs::write(directory.join("ünïcode.rs"), good_source()).map_err(|e| e.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(
            retention_of(&scan, "café-日本語/ünïcode.rs"),
            Some(DeclarationRetention::Complete),
            "a non-ASCII path must round-trip exactly, not be lossily replaced"
        );
        assert!(
            names_of(&scan, "café-日本語/ünïcode.rs")
                .iter()
                .any(|n| n == "ferris")
        );
        Ok(())
    }

    #[test]
    fn a_file_removed_during_the_scan_leaves_the_project_indexable() -> Result<(), String> {
        let scratch = scratch("vanish")?;
        fs::write(scratch.0.join("good.rs"), good_source()).map_err(|e| e.to_string())?;
        let vanishing = scratch.0.join("gone.rs");
        fs::write(&vanishing, good_source()).map_err(|e| e.to_string())?;

        // Discovery lists the file; the read must then see it removed.
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        let frontends = frontends()?;
        let paths = supported_paths(Path::new(root), frontends)?;
        assert!(
            paths.iter().any(|path| path.ends_with("gone.rs")),
            "the fixture must be discovered before it is removed"
        );
        fs::remove_file(&vanishing).map_err(|e| e.to_string())?;

        let capability = ProjectRoot::open(Path::new(root))?;
        let source_policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .map_err(|error| error.to_string())?;
        let scanned = scan_file(
            Path::new(root),
            &capability,
            &vanishing,
            [8; 32],
            &BTreeMap::new(),
            frontends,
            PythonVersion::Python314,
            &GoProjectCapture::default(),
            source_policy,
        )?;
        assert!(
            scanned.is_none(),
            "a file removed between discovery and read is no longer part of the project"
        );

        let scan = scan(&scratch)?;
        assert!(names_of(&scan, "good.rs").iter().any(|n| n == "ferris"));
        Ok(())
    }

    #[test]
    fn an_unavailable_file_becomes_available_again_when_it_can_be_read() -> Result<(), String> {
        // The unavailable row carries a zero content version, so a later
        // successful scan must produce a different row rather than reusing the
        // reported failure forever.
        let scratch = scratch("recover")?;
        fs::write(scratch.0.join("blob.rs"), [0xffu8, 0xfe]).map_err(|e| e.to_string())?;
        let broken = scan(&scratch)?;
        assert_eq!(
            retention_of(&broken, "blob.rs"),
            Some(DeclarationRetention::Unavailable(
                SourceUnavailableReason::NotText
            ))
        );

        fs::write(scratch.0.join("blob.rs"), good_source()).map_err(|e| e.to_string())?;
        let repaired = scan(&scratch)?;
        assert_eq!(
            retention_of(&repaired, "blob.rs"),
            Some(DeclarationRetention::Complete),
            "a repaired file must stop being reported as unavailable"
        );
        assert_ne!(
            broken.source_version, repaired.source_version,
            "repairing a file must change the project source version"
        );
        assert!(names_of(&repaired, "blob.rs").iter().any(|n| n == "ferris"));
        Ok(())
    }
}

/// Discovery and semantic-profile agreement laws.
///
/// Discovery admits a file because a frontend claims its extension, and the
/// file then needs a semantic profile.  When those two tables were written
/// separately they disagreed: the TypeScript frontend claimed `js`, `jsx`,
/// `mjs` and `cjs`, the profile table knew none of them, and the mismatch was
/// classified as fatal — so any checkout containing a single `postcss.config.js`
/// could not be indexed at all.
///
/// The assertions are on rendered content — the profile each extension
/// resolves to, the declaration names a scanned config file contributes, and
/// the typed reason an unprofiled file records — because a scan that admitted
/// the files and dropped every declaration inside them would keep the same
/// file count.
#[cfg(test)]
mod profile_tests {
    use super::*;

    #[test]
    fn sql_explicit_settings_and_hosts_are_confined_and_revalidated() -> Result<(), String> {
        let mut scratch = scratch("sql-explicit")?;
        scratch.0 = scratch.0.canonicalize().map_err(|e| e.to_string())?;
        fs::create_dir_all(scratch.0.join(".nudox")).map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("query.sql"), b"SELECT id FROM t").map_err(|e| e.to_string())?;
        let initial_scan = scan(&scratch)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        // Without a settings file the same walk selects every `.sql` file, each dialect inferred.
        let implicit = capture_sql_project(&initial_scan.sql_walk, &initial_scan.revision_fence, &scratch.0, &cancelled)
            .map_err(|e| format!("{e:?}"))?;
        assert!(implicit.implicit());
        assert_eq!(implicit.hosts().len(), 1);
        assert!(implicit.hosts()[0].inferred() && implicit.hosts()[0].oracle(), "no token decides the dialect of `SELECT id FROM t`");
        cancelled.store(true, Ordering::Release);
        assert!(matches!(capture_sql_project(&initial_scan.sql_walk, &initial_scan.revision_fence, &scratch.0, &cancelled), Err(SqlCaptureFailure::Cancelled)));
        cancelled.store(false, Ordering::Release);
        let config = br#"{"version":1,"sources":[{"path":"query.sql","dialect":"sqlite","context":{"kind":"sqlite_proposed","epoch":1,"tables":[{"name":"t","columns":["id"]}]}}]}"#;
        fs::write(scratch.0.join(".nudox/sql.json"), config).map_err(|e| e.to_string())?;
        let scan = scan(&scratch)?;
        let captured = std::sync::Arc::new(capture_sql_project(&scan.sql_walk, &scan.revision_fence, &scratch.0, &cancelled).map_err(|e| format!("{e:?}"))?);
        assert_eq!(captured.hosts().len(), 1);
        assert_eq!(captured.hosts()[0].bytes(), b"SELECT id FROM t");
        let witness = captured.witness();
        assert!(witness.revalidate(&scan.sql_walk, &scan.revision_fence, &scratch.0, &cancelled).is_ok());
        let other = Arc::new(AtomicBool::new(false));
        assert!(matches!(witness.revalidate(&scan.sql_walk, &scan.revision_fence, &scratch.0, &other), Err(SqlCaptureFailure::Invalid)));
        assert!(matches!(captured.revalidate(&scan.sql_walk, &scan.revision_fence, &scratch.0, &other), Err(SqlCaptureFailure::Invalid)));
        assert!(captured.revalidate(&scan.sql_walk, &scan.revision_fence, &scratch.0, &cancelled).is_ok());
        cancelled.store(true, Ordering::Release);
        assert!(matches!(captured.revalidate(&scan.sql_walk, &scan.revision_fence, &scratch.0, &cancelled), Err(SqlCaptureFailure::Cancelled)));
        cancelled.store(false, Ordering::Release);
        let projection = super::super::sql_capture::project_sql_project(std::sync::Arc::clone(&captured),
            scan.sql_walk.clone(), scan.revision_fence.clone(), scratch.0.clone(), None, None, &cancelled)
            .map_err(|e| format!("{e:?}"))?;
        assert!(projection.rows().iter().any(|(_, row)| matches!(row,
            backend_engine::builtin::SqlAnalysisRecord::Host { terminal: backend_engine::builtin::SqlHostTerminal::Unavailable(
                backend_engine::builtin::SqlUnavailable::EngineNotAdmitted), .. })));
        assert!(!projection.rows().iter().any(|(_, row)| matches!(row, backend_engine::builtin::SqlAnalysisRecord::Fact { .. })));
        fs::write(scratch.0.join("query.sql"), b"SELECT ix FROM t").map_err(|e| e.to_string())?;
        assert!(matches!(witness.revalidate(&scan.sql_walk, &scan.revision_fence, &scratch.0, &cancelled), Err(SqlCaptureFailure::Stale)));
        assert!(matches!(captured.revalidate(&scan.sql_walk, &scan.revision_fence, &scratch.0, &cancelled), Err(SqlCaptureFailure::Stale)));
        fs::write(scratch.0.join("query.sql"), b"SELECT id FROM t").map_err(|e| e.to_string())?;
        fs::write(scratch.0.join(".nudox/sql.json"), b"{} ").map_err(|e| e.to_string())?;
        assert!(matches!(captured.revalidate(&scan.sql_walk, &scan.revision_fence, &scratch.0, &cancelled), Err(SqlCaptureFailure::Stale)));
        Ok(())
    }

    #[test]
    fn sql_project_queue_proof_drops_host_buffers_and_rechecks_them() -> Result<(), String> {
        let mut scratch = scratch("sql-compact")?;
        scratch.0 = scratch.0.canonicalize().map_err(|e| e.to_string())?;
        fs::create_dir_all(scratch.0.join(".nudox")).map_err(|e| e.to_string())?;
        let sql = vec![b' '; 400 * 1024];
        fs::write(scratch.0.join("query.sql"), &sql).map_err(|e| e.to_string())?;
        let config = br#"{"version":1,"sources":[{"path":"query.sql","dialect":"sqlite","context":{"kind":"unavailable"}}]}"#;
        fs::write(scratch.0.join(".nudox/sql.json"), config).map_err(|e| e.to_string())?;
        let label = scratch.0.to_str().ok_or("invalid test root")?;
        let scan = scan_project(label, backend_engine::PackageKey::from_value(label).to_bytes(), &BTreeMap::new())?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let captured = Arc::new(capture_sql_project(&scan.sql_walk, &scan.revision_fence, &scratch.0, &cancelled).map_err(|e| format!("{e:?}"))?);
        let projection = super::super::sql_capture::project_sql_project(captured, scan.sql_walk.clone(), scan.revision_fence.clone(), scratch.0.clone(), None, None, &cancelled).map_err(|e| format!("{e:?}"))?;
        assert!(projection.checked_retained_bytes().ok_or("unaccounted retained proof")? < sql.len());
        assert!(projection.revalidate(&cancelled).is_ok());
        let other = Arc::new(AtomicBool::new(false));
        assert!(!projection.matches_cancellation(&other));
        assert!(matches!(projection.revalidate(&other), Err(SqlCaptureFailure::Invalid)));
        let digest = projection.checked_capture_digest(&cancelled).map_err(|e| format!("{e:?}"))?;
        assert_eq!(digest, projection.checked_capture_digest(&cancelled).map_err(|e| format!("{e:?}"))?);
        assert!(projection.checked_capture_digest(&other).is_err());
        let selected_digest = projection.project_rows_digest(&cancelled).map_err(|e| format!("{e:?}"))?;
        let mut retained_rows = projection.rows().to_vec();
        let attempt = retained_rows.iter_mut().find_map(|(_, row)| match row {
            backend_engine::builtin::SqlAnalysisRecord::Attempt { outcome, .. } => Some(outcome), _ => None,
        }).ok_or("missing SQL attempt")?;
        *attempt = backend_engine::builtin::SqlAttemptOutcome::Unavailable(backend_engine::builtin::SqlUnavailable::Read);
        assert_eq!(selected_digest, super::super::sql_capture::sql_project_rows_digest(projection.project(), &retained_rows, cancelled.as_ref()).map_err(|e| format!("{e:?}"))?);
        retained_rows.retain(|(_, row)| !matches!(row, backend_engine::builtin::SqlAnalysisRecord::Host { .. }));
        assert!(super::super::sql_capture::sql_project_rows_digest(projection.project(), &retained_rows, cancelled.as_ref()).is_err());
        cancelled.store(true, Ordering::Release);
        assert!(matches!(projection.checked_capture_digest(&cancelled), Err(backend_engine::builtin::SqlUnavailable::Cancelled)));
        cancelled.store(false, Ordering::Release);
        let project = ProductSourceRecord::project_with_membership_pages_and_source_selection(
            label, scan.source_version, scan.files.iter().map(|(key, _)| *key).collect::<Vec<_>>(),
            None, Some(scan.source_selection.clone()),
        )?.project_record().clone();
        let projection = Arc::new(projection);
        assert!(super::super::sql_admission::SqlLiveProof::new(Arc::clone(&projection), &project, Arc::clone(&other)).is_err());
        assert!(super::super::sql_admission::SqlLiveProof::new(Arc::clone(&projection), &project, Arc::clone(&cancelled)).is_ok());
        let mut edited = sql; edited[0] = b'x';
        fs::write(scratch.0.join("query.sql"), edited).map_err(|e| e.to_string())?;
        assert!(matches!(projection.revalidate(&cancelled), Err(SqlCaptureFailure::Stale)));
        Ok(())
    }

    #[test]
    #[ignore = "requires an independently built PostgreSQL worker and its actual producer basis"]
    fn postgres_project_capture_observes_native_column_source() -> Result<(), String> {
        let mut scratch = scratch("sql-postgres")?;
        scratch.0 = scratch.0.canonicalize().map_err(|e| e.to_string())?;
        fs::create_dir_all(scratch.0.join(".nudox")).map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("query.sql"), b"SELECT id, id FROM t;").map_err(|e| e.to_string())?;
        let config = br#"{"version":1,"sources":[{"path":"query.sql","dialect":"postgres","context":{"kind":"postgres_proposed","epoch":1,"search_path":["s1"],"schemas":[{"name":"s1","tables":[{"name":"t","columns":["id"]}]}]}}]}"#;
        fs::write(scratch.0.join(".nudox/sql.json"), config).map_err(|e| e.to_string())?;
        let scan = scan(&scratch)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let captured = std::sync::Arc::new(capture_sql_project(&scan.sql_walk, &scan.revision_fence, &scratch.0, &cancelled).map_err(|e| format!("{e:?}"))?);
        let worker = std::path::PathBuf::from(std::env::var_os("NUDOX_TEST_POSTGRES_WORKER").ok_or("worker path missing")?);
        let basis: backend_sql_contract::ProducerBasis = serde_json::from_str(&std::env::var("NUDOX_TEST_POSTGRES_BASIS").map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        let artifact = backend_compile::ToolchainArtifact::from_path(&worker, vec![]).map_err(|e| format!("{e:?}"))?;
        let permit = super::super::sql_capture::PostgresWorkerPermit::from_admitted(worker, artifact, basis).map_err(str::to_owned)?;
        let projected = super::super::sql_capture::project_sql_project(captured, scan.sql_walk.clone(), scan.revision_fence.clone(), scratch.0.clone(), None, Some(&permit), &cancelled).map_err(|e| format!("{e:?}"))?;
        let reads = projected.rows().iter().filter_map(|(_, row)| match row {
            backend_engine::builtin::SqlAnalysisRecord::Fact { fact: backend_semantic::sql_analysis::SqlFact::PostgresColumnRead { extent, .. }, .. } => Some(*extent),
            _ => None,
        }).collect::<Vec<_>>();
        assert_eq!(reads.len(), 2);
        assert_ne!(reads[0].start, reads[1].start);
        for read in reads {
            assert_eq!(b"SELECT id, id FROM t;".get(read.start as usize..read.end as usize), Some(&b"id"[..]));
        }
        Ok(())
    }

    #[test]
    fn vue_discovery_exclusions_are_counted_without_claiming_ts_authority_or_compiler_membership()
    -> Result<(), String> {
        let scratch = scratch("unsupported-vue")?;
        fs::create_dir_all(scratch.0.join("src")).map_err(|e| e.to_string())?;
        fs::create_dir_all(scratch.0.join("node_modules/hidden")).map_err(|e| e.to_string())?;
        fs::write(scratch.0.join(".gitignore"), "ignored.vue\nignored.css\n")
            .map_err(|e| e.to_string())?;
        for path in [
            "src/App.vue",
            "src/Other.VUE",
            "src/styles.CSS",
            "src/query.sql",
            "ignored.vue",
            "ignored.css",
            "node_modules/hidden/hidden.sql",
            "node_modules/hidden/Hidden.vue",
        ] {
            fs::write(
                scratch.0.join(path),
                "<template><h1>unsupported Vue</h1></template>",
            )
            .map_err(|e| e.to_string())?;
        }
        fs::write(scratch.0.join("src/App.vue"), [0xff, 0xfe]).map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("src/main.ts"), "export const typed = 1;")
            .map_err(|e| e.to_string())?;
        fs::write(
            scratch.0.join("src/helpers.js"),
            "export const scripted = 2;",
        )
        .map_err(|e| e.to_string())?;
        let first = scan(&scratch)?;
        assert!(first.stylo_walk.inventory_complete());
        assert_eq!(
            first.stylo_walk.inventory().map(|witness| witness.0),
            Some(1)
        );
        assert_eq!(
            first.stylo_walk.files()[0].relative_path,
            PathBuf::from("src/styles.CSS")
        );
        assert_eq!(
            first.source_selection,
            backend_library::SourceSelectionObservation::project_extensions(1, 1, 2)
        );
        let mut supported = paths_of(&first);
        supported.sort();
        assert_eq!(supported, ["src/helpers.js", "src/main.ts"]);
        assert_eq!(first.compiler_sources.len(), 2);
        assert_eq!(
            first.source_bytes_read,
            "export const typed = 1;".len() + "export const scripted = 2;".len(),
            "unsupported container bytes are not admitted or charged as compiler source"
        );
        assert!(
            !first.revision_fence.files.iter().any(|file| file
                .relative_path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("vue"))),
            "directory fences protect discovery counts without per-container metadata expansion"
        );

        assert!(
            first
                .compiler_sources
                .iter()
                .all(|source| !source.relative_path.ends_with(".vue"))
        );
        assert!(
            source_profile(Path::new("src/App.vue"))?.is_none(),
            "no Vue authority is fabricated"
        );
        let unchanged = scan(&scratch)?;
        assert_eq!(unchanged.source_selection, first.source_selection);
        assert_eq!(unchanged.source_version, first.source_version);
        let source_version = first.source_version;
        let before = first.source_selection;
        fs::write(
            scratch.0.join("src/New.vue"),
            "<script>not submitted to TSZ</script>",
        )
        .map_err(|e| e.to_string())?;
        let second = scan(&scratch)?;
        assert_eq!(
            second.source_selection,
            backend_library::SourceSelectionObservation::project_extensions(1, 1, 3)
        );
        assert_ne!(second.source_selection, before);
        assert_eq!(
            second.source_version, source_version,
            "supported source identities are independent of excluded containers"
        );
        assert_eq!(second.compiler_sources.len(), 2);
        Ok(())
    }

    #[test]
    fn stylo_worker_attempt_budget_counts_failures_and_stops_at_sixteen() {
        let mut budget = StyloWorkerBudget::new();
        let mut launches = 0;
        for index in 0..16 {
            let result = budget.run(|| -> Result<(), &str> {
                launches += 1;
                if index == 0 {
                    Err("cancelled")
                } else {
                    Err("worker failed")
                }
            });
            assert!(matches!(result, Some(Err(_))));
        }
        assert!(budget.exhausted());
        assert!(
            budget
                .run(|| {
                    launches += 1;
                    Ok::<(), &str>(())
                })
                .is_none(),
            "attempt 17 must never launch"
        );
        assert_eq!(launches, 16);
    }

    #[test]
    fn stylo_selection_deadline_is_recorded_as_unknown_capability() -> Result<(), String> {
        use backend_engine::builtin::{
            StyloAnalysisRecord, StyloAttemptArtifact, StyloAttemptOutcome, StyloUnavailable,
        };
        let scratch = scratch("stylo-selection-deadline")?;
        fs::write(scratch.0.join("site.css"), ".a {}").map_err(|error| error.to_string())?;
        let label = scratch.0.to_str().ok_or("non-UTF-8 test root")?;
        let package = backend_engine::PackageKey::from_value(label);
        let scan = scan_project(label, package.to_bytes(), &BTreeMap::new())?;
        let files = scan.files.iter().map(|(key, _)| *key).collect::<Vec<_>>();
        let project = ProductSourceRecord::project_with_membership_pages_and_source_selection(
            label,
            scan.source_version,
            files,
            None,
            Some(scan.source_selection.clone()),
        )?
        .project_record()
        .clone();
        let attempt = capture_stylo_attempt(
            &scan,
            project,
            None,
            Some(StyloUnavailable::Deadline),
            None,
            None,
            &AtomicBool::new(false),
            std::time::Duration::from_secs(30),
            None,
        )
        .map_err(|error| format!("attempt: {error:?}"))?;
        assert!(matches!(
            attempt.row(),
            StyloAnalysisRecord::Attempt {
                worker_artifact: StyloAttemptArtifact::Unknown,
                outcome: StyloAttemptOutcome::Unavailable(StyloUnavailable::Deadline),
                served_selection: None,
                ..
            }
        ));
        Ok(())
    }

    #[test]
    fn stylo_mandatory_attempt_capacity_refuses_before_publication() -> Result<(), String> {
        let scratch = scratch("stylo-attempt-capacity")?;
        fs::write(scratch.0.join("site.css"), ".a {}").map_err(|error| error.to_string())?;
        let label = scratch.0.to_str().ok_or("non-UTF-8 test root")?;
        let package = backend_engine::PackageKey::from_value(label);
        let mut scan = scan_project(label, package.to_bytes(), &BTreeMap::new())?;
        let files = scan.files.iter().map(|(key, _)| *key).collect::<Vec<_>>();
        let project = ProductSourceRecord::project_with_membership_pages_and_source_selection(
            label,
            scan.source_version,
            files,
            None,
            Some(scan.source_selection.clone()),
        )?
        .project_record()
        .clone();
        let limits = scan.source_admission_policy.limits();
        scan.stylo_budget_baseline.records = limits.max_project_records;
        let cancelled = AtomicBool::new(false);
        assert!(matches!(
            capture_stylo_attempt(
                &scan,
                project.clone(),
                None,
                None,
                None,
                None,
                &cancelled,
                std::time::Duration::from_secs(30),
                None
            ),
            Err(StyloProjectCaptureFailure::AttemptCapacity)
        ));
        scan.stylo_budget_baseline.records = 0;
        scan.stylo_budget_baseline.record_bytes = limits.max_project_record_bytes;
        assert!(matches!(
            capture_stylo_attempt(
                &scan,
                project,
                None,
                None,
                None,
                None,
                &cancelled,
                std::time::Duration::from_secs(30),
                None
            ),
            Err(StyloProjectCaptureFailure::AttemptCapacity)
        ));
        Ok(())
    }

    #[test]
    fn stylo_growth_respects_remaining_read_quota_and_invalidates_fence() -> Result<(), String> {
        let scratch = scratch("stylo-growth")?;
        let css = scratch.0.join("site.css");
        fs::write(&css, b"x").map_err(|error| error.to_string())?;
        let snapshot = scan(&scratch)?;
        let fence = snapshot.stylo_walk.bind_fence(&snapshot.revision_fence)?;
        let root = fence.open_bound_root(&fence.requested_root)?;
        fs::write(&css, b"grown").map_err(|error| error.to_string())?;
        // Three bytes remain: reserve one for read_bounded's overflow probe.
        let mut remaining = 3;
        let mut observed = 0;
        assert!(matches!(
            read_stylo_bounded(
                &root,
                Path::new("site.css"),
                100,
                &mut remaining,
                &mut observed
            ),
            Err(SourceFault::Unavailable(SourceUnavailableReason::TooLarge))
                | Err(SourceFault::UnavailableRead(
                    SourceUnavailableReason::TooLarge,
                    0..=3
                ))
        ));
        assert_eq!((remaining, observed), (3, 0));
        assert!(!compiler_revision_is_current_with_cancellation(
            &fence, None
        )?);
        // A file exactly at the allowance is deliberately refused to reserve
        // the probe, even if no growth occurs.
        fs::write(&css, b"abc").map_err(|error| error.to_string())?;
        assert!(matches!(
            read_stylo_bounded(
                &root,
                Path::new("site.css"),
                100,
                &mut remaining,
                &mut observed
            ),
            Err(SourceFault::Unavailable(SourceUnavailableReason::TooLarge))
        ));
        assert_eq!((remaining, observed), (3, 0));
        fs::write(&css, b"ab").map_err(|error| error.to_string())?;
        assert_eq!(
            read_stylo_bounded(
                &root,
                Path::new("site.css"),
                100,
                &mut remaining,
                &mut observed
            )
            .map_err(|error| format!("{error:?}"))?,
            b"ab"
        );
        assert_eq!((remaining, observed), (1, 2));
        Ok(())
    }

    #[test]
    #[ignore = "requires NUDOX_TEST_STYLO_WORKER pointing to a separately built fixture artifact"]
    fn stylo_worker_cap_limits_seventeenth_host_and_preserves_repeatable_facts()
    -> Result<(), String> {
        let scratch = scratch("stylo-worker-cap")?;
        for index in 0..17 {
            fs::write(
                scratch.0.join(format!("site{index:02}.css")),
                format!(".c{index} {{ --ink: red; }}"),
            )
            .map_err(|error| error.to_string())?;
        }
        let worker = PathBuf::from(
            std::env::var_os("NUDOX_TEST_STYLO_WORKER").ok_or("worker fixture missing")?,
        );
        let artifact = backend_compile::ToolchainArtifact::from_path(&worker, Vec::new())
            .map_err(|error| error.to_string())?;
        let permit = super::super::stylo_worker::StyloWorkerPermit::from_admitted(
            worker,
            artifact,
            backend_frontend_stylo::support_fingerprint(),
        )
        .map_err(|error| format!("worker permit: {error:?}"))?;
        let cancellation = AtomicBool::new(false);
        let capture = || {
            let scan = scan(&scratch)?;
            capture_stylo_project(
                &scan,
                &permit,
                &cancellation,
                std::time::Duration::from_secs(30),
                None,
            )
            .map_err(|error| format!("native capture: {error:?}"))
        };
        let first = capture()?;
        let second = capture()?;
        assert_eq!(first.worker_attempts(), 16);
        assert_eq!(second.worker_attempts(), 16);
        assert_eq!(
            first.rows(),
            second.rows(),
            "same source and policy must retain the same rows"
        );
        let mut limited = 0;
        let mut parsed = 0;
        for (_, row) in first.rows() {
            if let backend_engine::builtin::StyloAnalysisRecord::Host {
                path,
                content,
                terminal,
                ..
            } = row
            {
                match terminal {
                    backend_engine::builtin::StyloTerminal::Unavailable(
                        backend_engine::builtin::StyloUnavailable::Limit,
                    ) => {
                        assert_eq!(path, "site16.css");
                        assert!(content.is_none(), "capped host bytes were not read");
                        limited += 1;
                    }
                    backend_engine::builtin::StyloTerminal::Parsed(_) => parsed += 1,
                    other => return Err(format!("unexpected host terminal: {other:?}")),
                }
            }
        }
        assert_eq!((parsed, limited), (16, 1));
        Ok(())
    }

    #[test]
    #[ignore = "requires NUDOX_TEST_STYLO_WORKER pointing to a separately built fixture artifact"]
    fn same_walk_stylo_project_capture_binds_worker_facts_and_source_fence() -> Result<(), String> {
        let scratch = scratch("stylo-project")?;
        let css = scratch.0.join("site.css");
        fs::write(&css, ".button { --ink: red; }").map_err(|error| error.to_string())?;
        let worker = PathBuf::from(
            std::env::var_os("NUDOX_TEST_STYLO_WORKER").ok_or("worker fixture missing")?,
        );
        let artifact = backend_compile::ToolchainArtifact::from_path(&worker, Vec::new())
            .map_err(|error| error.to_string())?;
        let permit = super::super::stylo_worker::StyloWorkerPermit::from_admitted(
            worker,
            artifact,
            backend_frontend_stylo::support_fingerprint(),
        )
        .map_err(|error| format!("fixture permit: {error:?}"))?;
        let cancellation = AtomicBool::new(false);
        let first_scan = scan(&scratch)?;
        let first = capture_stylo_project(
            &first_scan,
            &permit,
            &cancellation,
            std::time::Duration::from_secs(30),
            None,
        )
        .map_err(|error| format!("first native capture: {error:?}"))?;
        first
            .verify(&cancellation)
            .map_err(|error| format!("native proof: {error:?}"))?;
        assert!(first.rows().iter().any(|(_, row)| matches!(row, backend_engine::builtin::StyloAnalysisRecord::Fact { fact: backend_engine::builtin::StyloProjectedRow::SourceOccurrence(site), .. } if site.raw == "button")));
        let second_scan = scan(&scratch)?;
        let second = capture_stylo_project(
            &second_scan,
            &permit,
            &cancellation,
            std::time::Duration::from_secs(30),
            None,
        )
        .map_err(|error| format!("repeat native capture: {error:?}"))?;
        assert_eq!(first.rows(), second.rows());
        assert_eq!(first.source_bytes_read(), ".button { --ink: red; }".len());
        fs::write(&css, ".buttOn { --ink: red; }").map_err(|error| error.to_string())?;
        assert!(matches!(
            first.verify(&cancellation),
            Err(StyloProjectCaptureFailure::Stale)
        ));
        // Failure terminals retain the attempted parser and process policy.
        fs::write(&css, [0xff]).map_err(|error| error.to_string())?;
        let failed_scan = scan(&scratch)?;
        let failed = |seconds| {
            capture_stylo_project(
                &failed_scan,
                &permit,
                &cancellation,
                std::time::Duration::from_secs(seconds),
                None,
            )
            .map_err(|error| format!("failed attempt capture: {error:?}"))
        };
        let a = failed(20)?;
        let b = failed(30)?;
        let host = |capture: &StyloProjectCapture| {
            capture
                .rows()
                .into_iter()
                .find_map(|(_, row)| {
                    if let backend_engine::builtin::StyloAnalysisRecord::Host {
                        config,
                        worker_policy,
                        terminal,
                        ..
                    } = row
                    {
                        Some((config.clone(), worker_policy.clone(), terminal.clone()))
                    } else {
                        None
                    }
                })
                .expect("failed host")
        };
        let (config_a, policy_a, terminal_a) = host(&a);
        let (config_b, policy_b, terminal_b) = host(&b);
        assert!(config_a.is_some() && policy_a.is_some());
        assert_eq!(config_a, config_b);
        assert_eq!(terminal_a, terminal_b);
        assert_ne!(policy_a, policy_b);
        Ok(())
    }

    use backend_engine::DeclarationRetention;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(label: &str) -> Result<Scratch, String> {
        let unique = format!(
            "backend-profile-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|error| error.to_string())?
                .as_nanos()
        );
        let directory = std::env::temp_dir().join(unique);
        fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
        Ok(Scratch(directory))
    }

    fn scan(scratch: &Scratch) -> Result<IndexSnapshot, String> {
        let root = scratch.0.to_str().ok_or("non-UTF-8 scratch path")?;
        scan_project(root, [11; 32], &BTreeMap::new())
    }

    fn retention_of(scan: &IndexSnapshot, path: &str) -> Option<DeclarationRetention> {
        scan.files.iter().find_map(|(_, record)| {
            let fields = record.file_fields()?;
            (fields.path == path).then_some(fields.retention)
        })
    }

    fn names_of(scan: &IndexSnapshot, path: &str) -> Vec<String> {
        scan.files
            .iter()
            .find_map(|(_, record)| {
                let fields = record.file_fields()?;
                (fields.path == path).then(|| {
                    fields
                        .declarations
                        .iter()
                        .map(|declaration| declaration.name().to_owned())
                        .collect::<Vec<_>>()
                })
            })
            .unwrap_or_default()
    }

    fn paths_of(scan: &IndexSnapshot) -> Vec<String> {
        scan.files
            .iter()
            .filter_map(|(_, record)| record.file_fields().map(|fields| fields.path.to_owned()))
            .collect()
    }

    /// Every extension any frontend claims resolves to a profile of that
    /// frontend's own language.
    ///
    /// This is the law the two tables broke.  It fails the moment a frontend
    /// gains an extension whose dialect nobody chose, which is exactly how
    /// `js` arrived without a profile.
    #[test]
    fn every_claimed_extension_resolves_to_its_own_language_profile() -> Result<(), String> {
        let frontends = FrontendSet::build()?;
        let mut checked = 0_usize;
        for frontend in &frontends.values {
            let language = frontend.language();
            for extension in frontend.baseline.extensions() {
                let path = PathBuf::from(format!("claimed.{extension}"));
                let profile = profile_of(&frontends, &path).ok_or_else(|| {
                    format!(
                        "{language:?} claims .{extension} but no semantic profile \
                         covers it, so every project containing one fails"
                    )
                })?;
                if profile.language() != language {
                    return Err(format!(
                        ".{extension} is claimed by {language:?} but resolves to \
                         {:?}",
                        profile.language()
                    ));
                }
                checked = checked.checked_add(1).ok_or("extension count overflow")?;
            }
        }
        if checked < 20 {
            return Err(format!(
                "only {checked} claimed extensions were inspected; the frontend \
                 inventory cannot have shrunk this far"
            ));
        }
        Ok(())
    }

    /// The extensions the QA report named resolve to the dialect they are.
    #[test]
    fn javascript_and_python_stub_extensions_carry_their_dialect() -> Result<(), String> {
        let frontends = FrontendSet::build()?;
        let expected = [
            (
                "app.js",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.mjs",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.cjs",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.mts",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.cts",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.ts",
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            ),
            (
                "app.jsx",
                LanguageProfile::TypeScript(TypeScriptSource::Tsx),
            ),
            (
                "app.tsx",
                LanguageProfile::TypeScript(TypeScriptSource::Tsx),
            ),
            (
                "stub.pyi",
                LanguageProfile::Python(PythonVersion::Python314),
            ),
            ("app.pyw", LanguageProfile::Python(PythonVersion::Python314)),
            ("lib.m", LanguageProfile::C(CStandard::C23)),
            ("lib.mm", LanguageProfile::Cxx(CxxStandard::Cxx23)),
        ];
        for (name, want) in expected {
            let got = profile_of(&frontends, Path::new(name))
                .ok_or_else(|| format!("{name} has no semantic profile"))?;
            if got != want {
                return Err(format!("{name} resolved to {got:?}, expected {want:?}"));
            }
        }
        Ok(())
    }

    /// An extension with no profile is one unavailable file, never a fatal
    /// scan.
    ///
    /// The distinction is the whole defect: `Fatal` propagates out of
    /// `scan_file` and fails `scan_project`, so the project cannot be indexed,
    /// while `Unavailable` becomes a row and the rest of the project survives.
    #[test]
    fn an_unprofiled_extension_is_unavailable_and_not_fatal() -> Result<(), String> {
        let frontends = FrontendSet::build()?;
        let fault = profile_fault(&frontends, Path::new("weird.qq")).err();
        if fault != Some(SourceFault::Unavailable(SourceUnavailableReason::Unparsed)) {
            return Err(format!(
                "an unprofiled extension produced {fault:?}; anything fatal here \
                 sinks the whole project"
            ));
        }
        profile_fault(&frontends, Path::new("postcss.config.js"))
            .map_err(|fault| format!("a claimed .js file still faults: {fault:?}"))?;
        Ok(())
    }

    /// A front-end checkout with a root config script and a framework cache
    /// indexes: the cache is never walked, the config file is scanned.
    #[test]
    fn a_javascript_project_indexes_its_config_and_skips_framework_caches() -> Result<(), String> {
        let scratch = scratch("javascript")?;
        fs::write(
            scratch.0.join("postcss.config.js"),
            b"function tailwindPlugin(options) { return options; }\n",
        )
        .map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join("src")).map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join("src/app.ts"),
            b"export function renderShell(depth: number): number { return depth; }\n",
        )
        .map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join(".angular/cache")).map_err(|e| e.to_string())?;
        fs::write(
            scratch.0.join(".angular/cache/x.js"),
            vec![
                b'x';
                SourceAdmissionLimits::default()
                    .max_file_source_bytes
                    .saturating_add(1)
            ],
        )
        .map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join(".next/static")).map_err(|error| error.to_string())?;
        fs::write(
            scratch.0.join(".next/static/chunk.js"),
            vec![
                b'x';
                SourceAdmissionLimits::default()
                    .max_file_source_bytes
                    .saturating_add(1)
            ],
        )
        .map_err(|error| error.to_string())?;

        let scan = scan(&scratch)?;

        assert_eq!(
            retention_of(&scan, "postcss.config.js"),
            Some(DeclarationRetention::Complete),
            "a root config script must be scanned, not refused"
        );
        assert!(
            names_of(&scan, "postcss.config.js")
                .iter()
                .any(|name| name == "tailwindPlugin"),
            "the config file must contribute its declarations, got {:?}",
            names_of(&scan, "postcss.config.js")
        );
        assert!(
            names_of(&scan, "src/app.ts")
                .iter()
                .any(|name| name == "renderShell"),
            "the TypeScript source must still index alongside it, got {:?}",
            names_of(&scan, "src/app.ts")
        );
        let paths = paths_of(&scan);
        assert!(
            !paths.iter().any(|path| path.starts_with(".angular")),
            "a framework cache must never be walked, found {paths:?}"
        );
        assert!(
            !paths.iter().any(|path| path.starts_with(".next")),
            "a framework cache must never be walked, found {paths:?}"
        );
        Ok(())
    }

    /// Every build-output directory the C# oracle refuses to load is also
    /// refused by discovery, so no `.cs` file waits for semantics that the
    /// oracle will never produce.
    ///
    /// Mirrors `frontends/csharp/helper/SourceLoader.cs:73`.
    #[test]
    fn discovery_skips_every_directory_the_csharp_oracle_skips() {
        for name in ["obj", ".git", ".vs", "node_modules"] {
            assert!(
                backend_library::is_hard_ignored_directory(std::ffi::OsStr::new(name)),
                "{name} is excluded by the C# source loader but still walked here"
            );
        }
        assert!(
            !backend_library::is_hard_ignored_directory(std::ffi::OsStr::new("bin")),
            "bin is a source directory for Python and npm; C# files are filtered by the shared language-specific discovery policy"
        );
    }

    /// The tool and cache directories a real checkout carries are not walked.
    #[test]
    fn discovery_skips_framework_and_tool_caches() {
        for name in [
            ".angular",
            ".next",
            ".nuxt",
            ".svelte-kit",
            ".turbo",
            ".cache",
            ".vite",
            ".parcel-cache",
            ".webpack",
            ".rollup.cache",
            ".nx",
            "out",
            "coverage",
            "storybook-static",
            ".nyc_output",
            "Library",
            ".gradle",
            ".mypy_cache",
            ".pytest_cache",
            "__pycache__",
            ".tox",
            ".ruff_cache",
            "jspm_packages",
            "vendor",
        ] {
            assert!(
                backend_library::is_hard_ignored_directory(std::ffi::OsStr::new(name)),
                "{name} is still walked"
            );
        }
        assert!(
            !backend_library::is_hard_ignored_directory(std::ffi::OsStr::new("src")),
            "discovery must not start guessing which directories are the project"
        );
    }
}

#[cfg(test)]
mod compiler_workspace_snapshot_tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn scratch() -> Result<Scratch, String> {
        // Keep the full path below macOS's SUN_LEN limit for socket fixtures.
        let serial = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("bcw-{}-{serial}", std::process::id()));
        fs::create_dir_all(&path).map_err(|error| error.to_string())?;
        Ok(Scratch(path))
    }

    #[test]
    fn confined_configuration_capture_includes_ignored_files_and_rechecks_absence()
    -> Result<(), String> {
        let scratch = scratch()?;
        fs::write(scratch.0.join(".gitignore"), "Directory.Build.*\n")
            .map_err(|e| e.to_string())?;
        fs::write(
            scratch.0.join("Directory.Build.props"),
            "<Project/>
",
        )
        .map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("App.csproj"), "<Project/>\n").map_err(|e| e.to_string())?;
        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        let paths = vec![
            "App.csproj".to_owned(),
            "Directory.Build.props".to_owned(),
            "Directory.Build.targets".to_owned(),
        ];
        let capture = snapshot.capture_configuration_paths(&paths, 1024)?;
        let observed = capture.probes().collect::<Vec<_>>();
        assert_eq!(observed, vec![
            ("App.csproj", Some(b"<Project/>\n".as_slice())),
            ("Directory.Build.props", Some(b"<Project/>\n".as_slice())),
            ("Directory.Build.targets", None),
        ]);
        assert!(capture.revalidate(&snapshot)?);
        let other = self::scratch()?;
        fs::write(other.0.join(".gitignore"), "Directory.Build.*\n").map_err(|e| e.to_string())?;
        fs::write(other.0.join("App.csproj"), "<Project/>\n").map_err(|e| e.to_string())?;
        fs::write(other.0.join("Directory.Build.props"), "<Project/>\n").map_err(|e| e.to_string())?;
        let other_snapshot = CompilerWorkspaceSnapshot::open(&other.0)?;
        assert!(!capture.revalidate(&other_snapshot)?);
        let make_fence = |snapshot: &CompilerWorkspaceSnapshot| CompilerRevisionFence {
            requested_root: snapshot.requested_root.clone(),
            root: snapshot.root.clone(),
            directories: vec![(PathBuf::new(), Some(snapshot.entries[0].revision))],
            files: Vec::new(),
            typescript_reuse: Vec::new(),
        };
        let fence = make_fence(&snapshot);
        let other_fence = make_fence(&other_snapshot);
        assert!(capture.revalidate_revision_fence(&fence, None)?);
        assert!(!capture.revalidate_revision_fence(&other_fence, None)?);
        let project = snapshot.capture_confined_file("App.csproj", 1024)?;
        assert!(project.revalidate_revision_fence(&fence, None)?);
        assert!(!project.revalidate_revision_fence(&other_fence, None)?);
        fs::write(
            scratch.0.join("Directory.Build.targets"),
            "<Project/>
",
        )
        .map_err(|e| e.to_string())?;
        assert!(!capture.revalidate(&snapshot)?);
        assert!(!capture.revalidate_revision_fence(&fence, None)?);
        Ok(())
    }

    #[test]
    fn csharp_rematerialization_reopens_exact_selected_source_bytes() -> Result<(), String> {
        let scratch = scratch()?;
        fs::write(scratch.0.join("App.cs"), "class A {}\n").map_err(|e| e.to_string())?;
        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        let fence = CompilerRevisionFence {
            requested_root: snapshot.requested_root.clone(), root: snapshot.root.clone(),
            directories: vec![(PathBuf::new(), Some(snapshot.entries[0].revision))],
            files: Vec::new(), typescript_reuse: Vec::new(),
        };
        let bytes = b"class A {}\n".to_vec();
        let profile = LanguageProfile::CSharp(backend_semantic::vocabulary::CSharpVersion::CSharp14);
        let mut sources = vec![CompilerSource {
            relative_path: "App.cs".to_owned(), profile,
            content: typed_of::<InputContentSchema>(&bytes).to_bytes(),
            source_fact_identity: SourceFactIdentity::from_canonical_bytes(&bytes), source: bytes,
        }];
        let cancellation = AtomicBool::new(false);
        assert!(rematerialize_csharp_source_buffers(&fence, &mut sources, "App.csproj", profile, &cancellation)?);
        fs::write(scratch.0.join("App.cs"), "class B {}\n").map_err(|e| e.to_string())?;
        assert!(!rematerialize_csharp_source_buffers(&fence, &mut sources, "App.csproj", profile, &cancellation)?);
        cancellation.store(true, Ordering::Release);
        assert!(rematerialize_csharp_source_buffers(&fence, &mut sources, "App.csproj", profile, &cancellation).is_err());
        Ok(())
    }

    #[test]
    fn csharp_global_json_probes_are_ordered_and_recheck_ignored_absence() -> Result<(), String> {
        let scratch = scratch()?;
        fs::create_dir(scratch.0.join("src")).map_err(|e| e.to_string())?;
        fs::write(scratch.0.join(".gitignore"), "global.json\n").map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("src/App.csproj"), "<Project/>\n").map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("global.json"), "{\"sdk\":{}}\n").map_err(|e| e.to_string())?;
        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        let captured = snapshot.capture_csharp_global_json_paths("src/App.csproj")?;
        assert_eq!(captured.probes().collect::<Vec<_>>(), vec![
            ("src/global.json", None),
            ("global.json", Some(b"{\"sdk\":{}}\n".as_slice())),
        ]);
        let fence = CompilerRevisionFence {
            requested_root: snapshot.requested_root.clone(), root: snapshot.root.clone(),
            directories: vec![(PathBuf::new(), Some(snapshot.entries[0].revision))],
            files: Vec::new(), typescript_reuse: Vec::new(),
        };
        assert!(captured.revalidate_revision_fence(&fence, None)?);
        fs::write(scratch.0.join("src/global.json"), "{}\n").map_err(|e| e.to_string())?;
        assert!(!captured.revalidate_revision_fence(&fence, None)?);
        assert!(snapshot.capture_csharp_global_json_paths(&format!("{}/App.csproj", vec!["x"; 8].join("/"))).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn csharp_global_json_probe_rejects_ignored_symlink() -> Result<(), String> {
        use std::os::unix::fs::symlink;
        let scratch = scratch()?;
        fs::write(scratch.0.join(".gitignore"), "global.json\n").map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("App.csproj"), "<Project/>\n").map_err(|e| e.to_string())?;
        symlink("App.csproj", scratch.0.join("global.json")).map_err(|e| e.to_string())?;
        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        assert!(snapshot.capture_csharp_global_json_paths("App.csproj").is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn confined_configuration_rejects_ignored_symlink() -> Result<(), String> {
        use std::os::unix::fs::symlink;
        let scratch = scratch()?;
        fs::write(scratch.0.join(".gitignore"), "Directory.Build.*\n").map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("App.csproj"), "<Project/>\n").map_err(|e| e.to_string())?;
        symlink("App.csproj", scratch.0.join("Directory.Build.props")).map_err(|e| e.to_string())?;
        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        let paths = vec!["App.csproj".to_owned(), "Directory.Build.props".to_owned(), "Directory.Build.targets".to_owned()];
        assert!(snapshot.capture_configuration_paths(&paths, 1024).is_err());
        Ok(())
    }

    #[test]
    fn snapshot_captures_python_build_package_bytes_and_fences_package_marker_changes()
    -> Result<(), String> {
        let scratch = scratch()?;
        for (path, bytes) in [
            ("src/pkg/__init__.py", b"".as_slice()),
            ("src/pkg/build/__init__.py", b"".as_slice()),
            (
                "src/pkg/build/build_tracker.py",
                b"class BuildTracker: pass\n".as_slice(),
            ),
            ("src/pkg/build/data.bin", b"raw-package-data".as_slice()),
            ("build/__init__.py", b"".as_slice()),
            ("build/generated.py", b"x = 1\n".as_slice()),
        ] {
            let path = scratch.0.join(path);
            fs::create_dir_all(path.parent().ok_or("parent")?)
                .map_err(|error| error.to_string())?;
            fs::write(path, bytes).map_err(|error| error.to_string())?;
        }
        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        assert_eq!(
            snapshot.read_file("src/pkg/build/build_tracker.py", 1024)?,
            b"class BuildTracker: pass\n"
        );
        assert_eq!(
            snapshot.read_file("src/pkg/build/data.bin", 1024)?,
            b"raw-package-data"
        );
        assert!(
            !snapshot
                .entries()
                .iter()
                .any(|entry| entry.path == "build" || entry.path.starts_with("build/"))
        );
        assert!(snapshot.revalidate()?);
        fs::remove_file(scratch.0.join("src/pkg/build/__init__.py"))
            .map_err(|error| error.to_string())?;
        assert!(
            !snapshot.revalidate()?,
            "changing package topology changes the admitted source frontier"
        );
        Ok(())
    }

    #[test]
    fn snapshot_inventory_includes_root_config_zero_byte_files_and_fences_new_paths()
    -> Result<(), String> {
        let scratch = scratch()?;
        fs::create_dir_all(scratch.0.join("src")).map_err(|error| error.to_string())?;
        fs::create_dir_all(scratch.0.join("target")).map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("Cargo.lock"), b"lock").map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("src/main.rs"), b"fn main() {}")
            .map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("empty.bin"), b"").map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("target/generated.rs"), b"ignored")
            .map_err(|error| error.to_string())?;

        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        let original_fence = snapshot.fence_digest();
        assert_eq!(
            snapshot.entries().first().map(|entry| entry.path.as_str()),
            Some("")
        );
        assert_eq!(
            snapshot.policy_identity(),
            COMPILER_WORKSPACE_POLICY_IDENTITY
        );
        assert!(snapshot.entries().iter().any(|entry| {
            entry.path == "Cargo.lock"
                && entry.kind == CompilerWorkspaceEntryKind::File
                && entry.byte_length == Some(4)
        }));
        assert!(snapshot.entries().iter().any(|entry| {
            entry.path == "empty.bin"
                && entry.kind == CompilerWorkspaceEntryKind::File
                && entry.byte_length == Some(0)
        }));
        assert!(
            !snapshot
                .entries()
                .iter()
                .any(|entry| entry.path.starts_with("target/"))
        );
        assert_eq!(snapshot.read_file("empty.bin", 0)?, b"");
        assert_eq!(snapshot.read_file("Cargo.lock", 4)?, b"lock");
        let mut streamed = Vec::new();
        assert_eq!(
            snapshot.stream_file("Cargo.lock", 4, 2, |chunk| {
                streamed.extend_from_slice(chunk);
                Ok(())
            })?,
            4
        );
        assert_eq!(streamed, b"lock");
        assert!(snapshot.revalidate()?);

        fs::write(scratch.0.join("new.config"), b"new").map_err(|error| error.to_string())?;
        assert!(!snapshot.revalidate()?);
        assert_ne!(
            CompilerWorkspaceSnapshot::open(&scratch.0)?.fence_digest(),
            original_fence
        );
        Ok(())
    }

    #[test]
    fn snapshot_fence_detects_content_and_metadata_changes() -> Result<(), String> {
        let scratch = scratch()?;
        fs::write(scratch.0.join("lockfile"), b"before").map_err(|error| error.to_string())?;
        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        fs::write(scratch.0.join("lockfile"), b"after!").map_err(|error| error.to_string())?;
        assert!(!snapshot.revalidate()?);
        assert!(snapshot.read_file("lockfile", 16).is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_fence_detects_same_length_edit_after_last_write_is_restored() -> Result<(), String>
    {
        use std::fs::{FileTimes, OpenOptions};
        use std::time::Duration;

        let scratch = scratch()?;
        let path = scratch.0.join("Cargo.lock");
        fs::write(&path, b"before!!").map_err(|error| error.to_string())?;
        let original_modified = fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .modified()
            .map_err(|error| error.to_string())?;
        let snapshot = CompilerWorkspaceSnapshot::open(&scratch.0)?;
        let captured = snapshot
            .entries()
            .iter()
            .find(|entry| entry.path == "Cargo.lock")
            .ok_or_else(|| "captured file is missing".to_owned())?
            .revision
            .windows;

        // Give even filesystems with coarse change-time granularity a new
        // timestamp bucket before restoring the user-settable last-write time.
        std::thread::sleep(Duration::from_millis(2_100));
        fs::write(&path, b"after!!!").map_err(|error| error.to_string())?;
        OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(|error| error.to_string())?
            .set_times(FileTimes::new().set_modified(original_modified))
            .map_err(|error| error.to_string())?;
        let restored_modified = fs::metadata(&path)
            .map_err(|error| error.to_string())?
            .modified()
            .map_err(|error| error.to_string())?;
        assert_eq!(restored_modified, original_modified);

        let current = snapshot
            .root_capability
            .revision_relative(Path::new("Cargo.lock"))?
            .windows;
        assert_eq!(current.length, captured.length);
        assert_eq!(current.file_id, captured.file_id);
        assert_eq!(current.last_write_time, captured.last_write_time);
        assert_ne!(current.change_time, captured.change_time);
        assert!(!snapshot.revalidate()?);
        assert!(snapshot.read_file("Cargo.lock", 16).is_err());
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn snapshot_rejects_junctions_and_hardlink_aliases() -> Result<(), String> {
        use std::process::Command;

        let scratch = scratch()?;
        let outside = scratch.0.join("outside-target");
        fs::create_dir(&outside).map_err(|error| error.to_string())?;
        fs::write(outside.join("outside.rs"), b"fn outside() {}\n")
            .map_err(|error| error.to_string())?;
        let junction = scratch.0.join("junction");
        let output = Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(&outside)
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(format!("mklink /J failed: {output:?}"));
        }
        assert!(CompilerWorkspaceSnapshot::open(&scratch.0).is_err());
        fs::remove_dir(&junction).map_err(|error| error.to_string())?;

        let outside_file = outside.join("linked.rs");
        fs::write(&outside_file, b"fn shared() {}\n").map_err(|error| error.to_string())?;
        fs::hard_link(&outside_file, scratch.0.join("linked.rs"))
            .map_err(|error| error.to_string())?;
        assert!(CompilerWorkspaceSnapshot::open(&scratch.0).is_err());
        Ok(())
    }

    #[test]
    fn workspace_paths_must_be_utf8_nfc_and_portable() {
        assert!(normalized_workspace_path(Path::new("src/main.rs")).is_ok());
        // A backslash inside a component is a non-portable name on Unix. On
        // Windows it is the separator, so the same text is the path
        // `src/main.rs`, and must normalize to exactly that.
        #[cfg(unix)]
        assert!(normalized_workspace_path(Path::new("src\\main.rs")).is_err());
        #[cfg(windows)]
        assert_eq!(
            normalized_workspace_path(Path::new("src\\main.rs")).as_deref(),
            Ok("src/main.rs")
        );
        assert!(normalized_workspace_path(Path::new("src/e\u{301}.txt")).is_err());
    }

    #[test]
    fn case_colliding_names_fail_portable_workspace_admission() -> Result<(), String> {
        let scratch = scratch()?;
        fs::write(scratch.0.join("Foo"), b"upper").map_err(|error| error.to_string())?;
        fs::write(scratch.0.join("foo"), b"lower").map_err(|error| error.to_string())?;
        if fs::read(scratch.0.join("Foo")).map_err(|error| error.to_string())? != b"upper" {
            // The host filesystem aliases case variants, so it cannot
            // represent the collision this test exercises.
            return Ok(());
        }
        assert!(CompilerWorkspaceSnapshot::open(&scratch.0).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_symlinks_instead_of_silently_omitting_them() -> Result<(), String> {
        let scratch = scratch()?;
        let outside = scratch.0.with_extension("outside");
        fs::write(&outside, b"external").map_err(|error| error.to_string())?;
        std::os::unix::fs::symlink(&outside, scratch.0.join("alias"))
            .map_err(|error| error.to_string())?;
        let result = CompilerWorkspaceSnapshot::open(&scratch.0);
        let _ = fs::remove_file(outside);
        assert!(result.is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_rejects_special_files_instead_of_admitting_them() -> Result<(), String> {
        use std::os::unix::net::UnixListener;

        let scratch = scratch()?;
        let _socket = UnixListener::bind(scratch.0.join("control.sock"))
            .map_err(|error| error.to_string())?;
        assert!(CompilerWorkspaceSnapshot::open(&scratch.0).is_err());
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "ingest/path_identity_tests.rs"]
mod path_identity_tests;

#[cfg(all(test, unix))]
mod work_observation_tests {
    use super::*;
    use crate::builtin::index_work_observation::IndexWorkObservation;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    fn ticket(id: u64) -> backend_library::IndexJobTicket {
        backend_library::IndexJobTicket::new(
            std::num::NonZeroU64::new(id).expect("nonzero"),
            [7; 16],
            backend_library::PackageReference::parse("/observed-project".to_owned())
                .expect("package"),
        )
    }

    #[test]
    fn held_source_is_observed_before_cancelled_scan_joins_and_exact_retry_finishes()
    -> Result<(), Box<dyn std::error::Error>> {
        let project = crate::test_support::private_temp_dir("observed-real-source")?;
        let workspace = crate::test_support::private_temp_dir("observed-real-owner")?;
        fs::write(
            project.path().join("lib.rs"),
            b"pub fn ready() -> u8 { 7 }\n",
        )?;
        let root = project.path().to_path_buf();
        let cancelled = Arc::new(AtomicBool::new(false));
        let observed =
            IndexWorkObservation::for_test(workspace.path(), ticket(1), Arc::clone(&cancelled));
        let (entered, entering) = mpsc::sync_channel(1);
        let (release, held) = mpsc::sync_channel(1);
        let held = Mutex::new(held);
        observed.hold_source_for_test(Arc::new(move || {
            entered.send(()).expect("announce source entry");
            held.lock()
                .expect("held source")
                .recv()
                .expect("release source");
        }));
        let worker_observed = Arc::clone(&observed);
        let worker_cancelled = Arc::clone(&cancelled);
        let first = thread::spawn(move || {
            scan_project_for_unproven_authorities_cancellable_with_observation(
                root.to_str().expect("UTF-8 test root"),
                [8; 32],
                &BTreeMap::new(),
                &worker_cancelled,
                None,
                Some(&worker_observed),
            )
        });
        entering.recv_timeout(Duration::from_secs(3))?;
        let path = workspace.path().join("index-work-observation-v1.json");
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            let value = fs::read(&path)
                .ok()
                .and_then(|raw| serde_json::from_slice::<serde_json::Value>(&raw).ok());
            if value.as_ref().is_some_and(|v| {
                v["phase"] == "structural-source-analysis" && v["workers"][0]["source_ordinal"] == 1
            }) {
                let value = value.expect("observed");
                assert_eq!(value["source_results_finished"], 0);
                assert_eq!(value["source_results_acknowledged"], 0);
                assert_eq!(value["terminal_receipt"], false);
                break;
            }
            assert!(
                Instant::now() < until,
                "existing scan worker must publish while one parser entry is held"
            );
            thread::sleep(Duration::from_millis(5));
        }
        cancelled.store(true, Ordering::Release);
        assert!(
            !first.is_finished(),
            "cancel terminal cannot precede this held source worker"
        );
        release.send(())?;
        let first = first
            .join()
            .map_err(|_| std::io::Error::other("scan panicked"))?;
        match first {
            Err(error) => assert!(error.contains("cancel")),
            Ok(_) => panic!("cancelled source was admitted"),
        }
        let second_cancelled = Arc::new(AtomicBool::new(false));
        let second_observed = IndexWorkObservation::for_test(
            workspace.path(),
            ticket(2),
            Arc::clone(&second_cancelled),
        );
        let second = scan_project_for_unproven_authorities_cancellable_with_observation(
            project.path().to_str().expect("UTF-8 test root"),
            [8; 32],
            &BTreeMap::new(),
            &second_cancelled,
            None,
            Some(&second_observed),
        )
        .map_err(std::io::Error::other)?;
        assert_eq!(second.files.len(), 1);
        assert_eq!(
            second.files[0].1.file_fields().expect("source file").path,
            "lib.rs"
        );
        assert_eq!(
            second.source_bytes_read,
            b"pub fn ready() -> u8 { 7 }\n".len()
        );
        let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
        assert_eq!(value["ticket"], serde_json::to_value(ticket(2))?);
        assert_eq!(value["source_results_acknowledged"], 1);
        assert_eq!(value["cancelled"], false);
        Ok(())
    }
}

/// Fixed per-project supervisor-attempt cap. Every claimed slot is charged
/// before invocation, including failures and cancellations; remaining hosts
/// retain explicit Limit terminals without reading their bytes.
const MAX_STYLO_WORKER_ATTEMPTS: u32 = 16;
struct StyloWorkerBudget {
    remaining: u32,
}
impl StyloWorkerBudget {
    fn new() -> Self {
        Self {
            remaining: MAX_STYLO_WORKER_ATTEMPTS,
        }
    }
    fn exhausted(&self) -> bool {
        self.remaining == 0
    }
    fn claim(&mut self) -> bool {
        if self.remaining == 0 {
            false
        } else {
            self.remaining -= 1;
            true
        }
    }
    fn run<T>(&mut self, invoke: impl FnOnce() -> T) -> Option<T> {
        self.claim().then(invoke)
    }
}

/// A selected native CSS host. Captured rows retain a private supervised
/// witness; unavailable terminals never claim empty native facts.
enum StyloProjectHost {
    Captured(super::stylo_capture::StyloHostCapture),
    Unavailable {
        key: [u8; 32],
        row: backend_engine::builtin::StyloAnalysisRecord,
    },
}
impl StyloProjectHost {
    fn key(&self) -> [u8; 32] {
        match self {
            Self::Captured(value) => value.host_key(),
            Self::Unavailable { key, .. } => *key,
        }
    }
    fn row(&self) -> &backend_engine::builtin::StyloAnalysisRecord {
        match self {
            Self::Captured(value) => value.host(),
            Self::Unavailable { row, .. } => row,
        }
    }
}
/// Private project selection produced from one source walk and supervised
/// workers. It is not a durable workspace admission or a query capability.
pub(super) struct StyloProjectCapture {
    selection_key: [u8; 32],
    selection: backend_engine::builtin::StyloAnalysisRecord,
    attempt: Option<StyloAttemptCapture>,
    hosts: Vec<StyloProjectHost>,
    fence: CompilerRevisionFence,
    source_bytes_read: usize,
    worker_attempts: u32,
}
#[derive(Debug)]
pub(super) enum StyloProjectCaptureFailure {
    Cancelled,
    Stale,
    Unavailable,
    /// The mandatory latest-attempt row cannot fit the shared admission budget.
    AttemptCapacity,
    Invalid(String),
}
impl StyloProjectCapture {
    pub(super) fn revision_fence(&self) -> &CompilerRevisionFence { &self.fence }
    pub(super) fn selection_key(&self) -> [u8; 32] {
        self.selection_key
    }
    pub(super) fn selection(&self) -> &backend_engine::builtin::StyloAnalysisRecord {
        &self.selection
    }
    pub(super) fn source_bytes_read(&self) -> usize {
        self.source_bytes_read
    }
    #[cfg(test)]
    fn worker_attempts(&self) -> u32 {
        self.worker_attempts
    }
    pub(super) fn bind_attempt(&mut self, attempt: StyloAttemptCapture) {
        self.attempt = Some(attempt);
    }
    pub(super) fn attempt_project(&self) -> Option<&backend_engine::ProductSourceRecord> {
        self.attempt.as_ref().map(StyloAttemptCapture::project)
    }
    pub(super) fn rows(&self) -> Vec<([u8; 32], &backend_engine::builtin::StyloAnalysisRecord)> {
        let mut rows = vec![(self.selection_key, &self.selection)];
        if let Some(attempt) = &self.attempt {
            rows.push((attempt.key, &attempt.row));
        }
        for host in &self.hosts {
            rows.push((host.key(), host.row()));
            if let StyloProjectHost::Captured(value) = host {
                rows.extend(value.facts().iter().map(|(key, row)| (*key, row)));
            }
        }
        rows.sort_by_key(|entry| entry.0);
        rows
    }
    /// Rechecks private host witnesses, full selection consistency and the
    /// original confined discovery/file fence immediately before publication.
    pub(super) fn verify(
        &self,
        cancellation: &AtomicBool,
    ) -> Result<(), StyloProjectCaptureFailure> {
        for host in &self.hosts {
            if let StyloProjectHost::Captured(value) = host {
                value.verify().map_err(|error| {
                    StyloProjectCaptureFailure::Invalid(format!("Stylo host witness: {error:?}"))
                })?;
            }
        }
        if let Some(attempt) = &self.attempt {
            attempt.verify(cancellation)?;
        }
        backend_engine::builtin::admit_stylo_relation(&self.rows())
            .map_err(StyloProjectCaptureFailure::Invalid)?;
        match compiler_revision_is_current_with_cancellation(&self.fence, Some(cancellation)) {
            Ok(true) => Ok(()),
            Ok(false) => Err(StyloProjectCaptureFailure::Stale),
            Err(error) if cancellation.load(Ordering::Acquire) => {
                Err(StyloProjectCaptureFailure::Cancelled)
            }
            Err(error) => Err(StyloProjectCaptureFailure::Invalid(error)),
        }
    }
}

/// Owner-private same-walk proof for a current or unavailable native attempt.
pub(super) struct StyloAttemptCapture {
    key: [u8; 32],
    row: backend_engine::builtin::StyloAnalysisRecord,
    project: backend_engine::ProductSourceRecord,
    fence: CompilerRevisionFence,
}
impl StyloAttemptCapture {
    pub(super) fn revision_fence(&self) -> &CompilerRevisionFence { &self.fence }
    pub(super) fn row(&self) -> &backend_engine::builtin::StyloAnalysisRecord {
        &self.row
    }
    pub(super) fn key(&self) -> [u8; 32] {
        self.key
    }
    pub(super) fn project(&self) -> &backend_engine::ProductSourceRecord {
        &self.project
    }
    pub(super) fn verify(
        &self,
        cancellation: &AtomicBool,
    ) -> Result<(), StyloProjectCaptureFailure> {
        self.row
            .validate()
            .map_err(StyloProjectCaptureFailure::Invalid)?;
        match compiler_revision_is_current_with_cancellation(&self.fence, Some(cancellation)) {
            Ok(true) => Ok(()),
            Ok(false) => Err(StyloProjectCaptureFailure::Stale),
            Err(_) if cancellation.load(Ordering::Acquire) => {
                Err(StyloProjectCaptureFailure::Cancelled)
            }
            Err(error) => Err(StyloProjectCaptureFailure::Invalid(error)),
        }
    }
}

/// Bind attempt status to the same source scan and exact Project after-image.
/// An unavailable witness never asserts host content or a complete inventory.
pub(super) fn capture_stylo_attempt(
    scan: &IndexSnapshot,
    project: backend_engine::ProductSourceRecord,
    permit: Option<&super::stylo_worker::StyloWorkerPermit>,
    selection_failure: Option<backend_engine::builtin::StyloUnavailable>,
    observed: Option<&StyloProjectCapture>,
    retained: Option<&backend_engine::builtin::StyloAnalysisRecord>,
    cancellation: &AtomicBool,
    wall_time: std::time::Duration,
    memory_limit: Option<usize>,
) -> Result<StyloAttemptCapture, StyloProjectCaptureFailure> {
    use backend_engine::builtin::{
        StyloAnalysisProfile, StyloAnalysisRecord, StyloAttemptArtifact, StyloAttemptOutcome,
        StyloInventory, StyloUnavailable, stylo_attempt_key, stylo_rows_digest,
        stylo_selection_key,
    };
    if selection_failure.is_some() && (permit.is_some() || observed.is_some()) {
        return Err(StyloProjectCaptureFailure::Invalid(
            "conflicting Stylo selection outcome".into(),
        ));
    }
    let walk = &scan.stylo_walk;
    let fields = project.project_fields().ok_or_else(|| {
        StyloProjectCaptureFailure::Invalid("attempt source is not a Project".into())
    })?;
    if backend_engine::PackageKey::from_value(fields.label).to_bytes() != walk.project
        || fields.source_version != scan.source_version
    {
        return Err(StyloProjectCaptureFailure::Invalid(
            "attempt Project differs from source scan".into(),
        ));
    }
    let fence = walk
        .bind_fence(&scan.revision_fence)
        .map_err(StyloProjectCaptureFailure::Invalid)?;
    if !compiler_revision_is_current_with_cancellation(&fence, Some(cancellation))
        .map_err(StyloProjectCaptureFailure::Invalid)?
    {
        return Err(StyloProjectCaptureFailure::Stale);
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-attempt-scope-policy.v1\0");
    hash.update(&(walk.discovery_options.len() as u64).to_be_bytes());
    hash.update(&walk.discovery_options);
    hash.update(&walk.source_policy);
    hash.update(&(16u64 * 1024 * 1024).to_be_bytes());
    hash.update(&(ProductSourceRecord::MAX_PROJECT_FILES as u64).to_be_bytes());
    hash.update(&(backend_engine::builtin::MAX_STYLO_RELATION_ROW_BYTES as u64).to_be_bytes());
    hash.update(&2u64.to_be_bytes()); // reserved selection + latest-attempt rows
    hash.update(&MAX_STYLO_WORKER_ATTEMPTS.to_be_bytes());
    let scope_policy = *hash.finalize().as_bytes();
    let source_root = walk
        .root
        .to_str()
        .ok_or(StyloProjectCaptureFailure::Unavailable)?
        .to_owned();
    let (discovery, inventory, inventory_count, inventory_digest, served_selection, outcome) =
        if let Some(capture) = observed {
            let StyloAnalysisRecord::Selection {
                project,
                source_root: selected_root,
                discovery,
                inventory,
                inventory_count,
                inventory_digest,
                ..
            } = capture.selection()
            else {
                return Err(StyloProjectCaptureFailure::Invalid(
                    "captured Stylo selection missing".into(),
                ));
            };
            if *project != walk.project || selected_root != &source_root {
                return Err(StyloProjectCaptureFailure::Invalid(
                    "captured Stylo selection differs from attempt scope".into(),
                ));
            }
            let digest = stylo_rows_digest(&[(stylo_selection_key(*project), capture.selection())])
                .map_err(StyloProjectCaptureFailure::Invalid)?;
            (
                Some(*discovery),
                Some(*inventory),
                Some(*inventory_count),
                Some(*inventory_digest),
                Some(digest),
                StyloAttemptOutcome::Observed,
            )
        } else {
            let root = fence
                .open_bound_root(&fence.requested_root)
                .map_err(StyloProjectCaptureFailure::Invalid)?;
            let discovery = match stylo_discovery_witness(walk, &root, cancellation) {
                Ok(value) => Some(value),
                Err(StyloProjectCaptureFailure::Unavailable) => None,
                Err(error) => return Err(error),
            };
            let (inventory, count, digest) = match walk.inventory {
                Some((count, digest)) => (
                    Some(if walk.inventory_complete {
                        StyloInventory::Complete
                    } else {
                        StyloInventory::Incomplete
                    }),
                    Some(count),
                    Some(digest),
                ),
                None => (None, None, None),
            };
            let served = retained
                .map(|selection| {
                    stylo_rows_digest(&[(stylo_selection_key(walk.project), selection)])
                })
                .transpose()
                .map_err(StyloProjectCaptureFailure::Invalid)?;
            let reason = if let Some(reason) = selection_failure {
                reason
            } else if permit.is_some() {
                StyloUnavailable::SelectionUnavailable
            } else {
                StyloUnavailable::ParserUnavailable
            };
            (
                discovery,
                inventory,
                count,
                digest,
                served,
                StyloAttemptOutcome::Unavailable(reason),
            )
        };
    let worker_artifact = if selection_failure.is_some() {
        StyloAttemptArtifact::Unknown
    } else {
        permit.map_or(StyloAttemptArtifact::NoPermit, |permit| {
            StyloAttemptArtifact::Admitted(permit.artifact_identity())
        })
    };
    let requested_policy = super::stylo_worker::stylo_worker_policy(wall_time, memory_limit)
        .map_err(|_| StyloProjectCaptureFailure::Unavailable)?;
    let row = StyloAnalysisRecord::Attempt {
        project: walk.project,
        source_root,
        profile: StyloAnalysisProfile::StyloStandaloneV1,
        scope_policy,
        discovery,
        policy_complete: false,
        max_worker_attempts: Some(MAX_STYLO_WORKER_ATTEMPTS),
        inventory,
        inventory_digest,
        inventory_count,
        expected_producer: Some(backend_frontend_stylo::support_fingerprint()),
        expected_recipe: Some(backend_frontend_stylo::RECIPE.into()),
        adapter: super::stylo_admission::stylo_adapter_fingerprint(),
        worker_artifact,
        requested_policy,
        outcome,
        served_selection,
    };
    let limits = scan.source_admission_policy.limits();
    if limits
        .max_project_records
        .saturating_sub(scan.stylo_budget_baseline.records)
        == 0
    {
        return Err(StyloProjectCaptureFailure::AttemptCapacity);
    }
    let mut encoded = Vec::new();
    <backend_engine::builtin::StyloAnalysisRelation as backend_version::Relation>::encode_value(
        &row,
        &mut encoded,
    );
    if encoded.len()
        > limits
            .max_project_record_bytes
            .saturating_sub(scan.stylo_budget_baseline.record_bytes)
    {
        return Err(StyloProjectCaptureFailure::AttemptCapacity);
    }
    let result = StyloAttemptCapture {
        key: stylo_attempt_key(walk.project),
        row,
        project,
        fence,
    };
    result.verify(cancellation)?;
    Ok(result)
}

fn stylo_discovery_witness(
    walk: &StyloWalkEvidence,
    root: &ProjectRoot,
    cancellation: &AtomicBool,
) -> Result<[u8; 32], StyloProjectCaptureFailure> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-discovery-v1\0");
    hash.update(&(walk.discovery_options.len() as u64).to_be_bytes());
    hash.update(&walk.discovery_options);
    hash.update(&walk.source_policy);
    hash.update(&(16u64 * 1024 * 1024).to_be_bytes());
    hash.update(&(ProductSourceRecord::MAX_PROJECT_FILES as u64).to_be_bytes());
    hash.update(&(backend_engine::builtin::MAX_STYLO_RELATION_ROW_BYTES as u64).to_be_bytes());
    hash.update(&2u64.to_be_bytes()); // reserved selection + latest-attempt rows
    hash.update(&MAX_STYLO_WORKER_ATTEMPTS.to_be_bytes());
    let mut bytes = 0usize;
    for path in &walk.local_policy_paths {
        if cancellation.load(Ordering::Acquire) {
            return Err(StyloProjectCaptureFailure::Cancelled);
        }
        let relative = path.strip_prefix(&walk.root).map_err(|_| {
            StyloProjectCaptureFailure::Invalid("policy escaped selected root".into())
        })?;
        let name = relative
            .to_str()
            .ok_or(StyloProjectCaptureFailure::Unavailable)?
            .replace(std::path::MAIN_SEPARATOR, "/");
        hash.update(&(name.len() as u64).to_be_bytes());
        hash.update(name.as_bytes());
        let remaining = (16 * 1024 * 1024usize).saturating_sub(bytes);
        if remaining == 0 {
            return Err(StyloProjectCaptureFailure::Unavailable);
        }
        match root.read(relative, remaining.min(1024 * 1024)) {
            Ok(value) => {
                bytes = bytes
                    .checked_add(value.len())
                    .ok_or(StyloProjectCaptureFailure::Unavailable)?;
                hash.update(&(value.len() as u64).to_be_bytes());
                hash.update(&value);
            }
            Err(SourceFault::Vanished) => return Err(StyloProjectCaptureFailure::Stale),
            Err(SourceFault::Fatal(error)) => {
                return Err(StyloProjectCaptureFailure::Invalid(error));
            }
            Err(_) => return Err(StyloProjectCaptureFailure::Unavailable),
        }
    }
    Ok(*hash.finalize().as_bytes())
}

/// Captures selected stylesheets once from the existing walk using an admitted
/// supervised worker. Failure is scoped to native CSS; this function does not
/// alter the shared source index or publish any workspace transition.
/// Reads within the remaining project quota, including the overflow probe.
fn read_stylo_bounded(
    root: &ProjectRoot,
    path: &Path,
    max_file: usize,
    remaining: &mut usize,
    observed: &mut usize,
) -> Result<Vec<u8>, SourceFault> {
    if *remaining == 0 {
        return Err(SourceFault::Unavailable(SourceUnavailableReason::TooLarge));
    }
    let result = root.read(path, max_file.min(remaining.saturating_sub(1)));
    let charged = match &result {
        Ok(bytes) => bytes.len(),
        Err(SourceFault::UnavailableRead(_, bytes)) => *bytes,
        _ => 0,
    };
    *remaining = remaining
        .checked_sub(charged)
        .ok_or_else(|| SourceFault::Fatal("Stylo source quota exceeded".into()))?;
    *observed = observed
        .checked_add(charged)
        .ok_or_else(|| SourceFault::Fatal("Stylo source work overflow".into()))?;
    result
}

pub(super) fn capture_stylo_project(
    scan: &IndexSnapshot,
    permit: &super::stylo_worker::StyloWorkerPermit,
    cancellation: &AtomicBool,
    wall_time: std::time::Duration,
    memory_limit: Option<usize>,
) -> Result<StyloProjectCapture, StyloProjectCaptureFailure> {
    use super::stylo_capture::{StyloCaptureFailure, capture_stylo_host};
    use super::stylo_worker::StyloWorkerFailure;
    use backend_engine::builtin::{
        StyloAnalysisProfile, StyloAnalysisRecord, StyloAnalysisRelation, StyloInventory,
        StyloTerminal, StyloUnavailable, stylo_host_key, stylo_rows_digest, stylo_selection_key,
    };
    use backend_semantic::stylo_analysis::{StyloLimits, StyloParseConfig, StyloSourceKind};
    use backend_version::Relation;
    if cancellation.load(Ordering::Acquire) {
        return Err(StyloProjectCaptureFailure::Cancelled);
    }
    if permit.support() != backend_frontend_stylo::support_fingerprint() {
        return Err(StyloProjectCaptureFailure::Unavailable);
    }
    let walk = &scan.stylo_walk;
    let (inventory_count, inventory_digest) = walk
        .inventory
        .ok_or(StyloProjectCaptureFailure::Unavailable)?;
    let fence = walk
        .bind_fence(&scan.revision_fence)
        .map_err(StyloProjectCaptureFailure::Invalid)?;
    if !compiler_revision_is_current_with_cancellation(&fence, Some(cancellation))
        .map_err(StyloProjectCaptureFailure::Invalid)?
    {
        return Err(StyloProjectCaptureFailure::Stale);
    }
    let root = fence
        .open_bound_root(&fence.requested_root)
        .map_err(StyloProjectCaptureFailure::Invalid)?;
    let discovery = stylo_discovery_witness(walk, &root, cancellation)?;
    let source_root = walk
        .root
        .to_str()
        .ok_or(StyloProjectCaptureFailure::Unavailable)?
        .to_owned();
    let limits = scan.source_admission_policy.limits();
    let mut remaining_source = limits
        .max_project_source_bytes
        .saturating_sub(scan.stylo_budget_baseline.source_bytes);
    let mut remaining_bytes = limits
        .max_project_record_bytes
        .saturating_sub(scan.stylo_budget_baseline.record_bytes)
        .min(64 * 1024 * 1024);
    let mut remaining_records = limits
        .max_project_records
        .saturating_sub(scan.stylo_budget_baseline.records)
        .min(262_144);
    if remaining_records < 2 {
        return Err(StyloProjectCaptureFailure::Unavailable);
    }
    remaining_records -= 2; // project selection and latest-attempt rows
    // The attempt is attached after the same-scan Project is constructed.
    // Reserve its largest admissible row before retaining host facts.
    remaining_bytes = remaining_bytes
        .checked_sub(backend_engine::builtin::MAX_STYLO_RELATION_ROW_BYTES)
        .ok_or(StyloProjectCaptureFailure::Unavailable)?;
    let max_file = limits
        .max_file_source_bytes
        .min(limits.max_in_flight_source_bytes)
        .min(backend_frontend_stylo::worker_protocol::MAX_SOURCE);
    if max_file == 0 {
        return Err(StyloProjectCaptureFailure::Unavailable);
    }
    let mut hosts = Vec::new();
    let mut worker_budget = StyloWorkerBudget::new();
    let mut source_bytes_read = 0usize;
    let mut complete = walk.inventory_complete;
    for file in &walk.files {
        if cancellation.load(Ordering::Acquire) {
            return Err(StyloProjectCaptureFailure::Cancelled);
        }
        if remaining_records == 0 || remaining_bytes == 0 {
            complete = false;
            break;
        }
        let name = file
            .relative_path
            .to_str()
            .ok_or(StyloProjectCaptureFailure::Unavailable)?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let key =
            stylo_host_key(walk.project, &name).map_err(StyloProjectCaptureFailure::Invalid)?;
        let size = file
            .metadata
            .and_then(|value| usize::try_from(value.length).ok());
        let mut failure = if size.is_none() {
            Some(StyloUnavailable::ReadUnavailable)
        } else if remaining_source == 0
            || size.is_some_and(|size| size > max_file || size >= remaining_source)
            || worker_budget.exhausted()
        {
            Some(StyloUnavailable::Limit)
        } else {
            None
        };
        let mut captured = None;
        let mut content = None;
        let mut source = None;
        let mut byte_length = None;
        let mut attempted_config = None;
        let mut attempted_policy = None;
        if failure.is_none() {
            match read_stylo_bounded(
                &root,
                &file.relative_path,
                max_file,
                &mut remaining_source,
                &mut source_bytes_read,
            ) {
                Ok(bytes) => {
                    content = Some(typed_of::<InputContentSchema>(&bytes).to_bytes());
                    source = Some(*backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(&bytes).as_ref());
                    byte_length = Some(bytes.len() as u64);
                    let base_url = url::Url::from_file_path(walk.root.join(&file.relative_path))
                        .map_err(|()| StyloProjectCaptureFailure::Unavailable)?
                        .to_string();
                    // Reserve disjoint native row quotas before launching the
                    // non-interruptible parser inside its supervised process.
                    let quota = remaining_records.saturating_sub(1);
                    let rule_quota = (quota / 4).min(100_000);
                    let record_quota = (quota / 2).min(200_000);
                    let diagnostic_quota =
                        quota.saturating_sub(rule_quota + record_quota).min(2_000);
                    let config = StyloParseConfig {
                        source_kind: StyloSourceKind::StandaloneUtf8,
                        base_url,
                        author_origin: true,
                        no_quirks: true,
                        allow_imports: false,
                        limits: StyloLimits {
                            input_bytes: max_file as u64,
                            rules: u32::try_from(rule_quota)
                                .map_err(|_| StyloProjectCaptureFailure::Unavailable)?,
                            depth: 64,
                            records: u32::try_from(record_quota)
                                .map_err(|_| StyloProjectCaptureFailure::Unavailable)?,
                            diagnostics: u32::try_from(diagnostic_quota)
                                .map_err(|_| StyloProjectCaptureFailure::Unavailable)?,
                            text_bytes: remaining_bytes.min(8 * 1024 * 1024) as u64,
                        },
                    };
                    attempted_policy = Some(
                        super::stylo_worker::stylo_worker_policy(wall_time, memory_limit)
                            .map_err(|_| StyloProjectCaptureFailure::Unavailable)?,
                    );
                    attempted_config = Some(config.clone());
                    let outcome = worker_budget
                        .run(|| {
                            capture_stylo_host(
                                permit,
                                walk.root.clone(),
                                walk.project,
                                &name,
                                &bytes,
                                &config,
                                wall_time,
                                memory_limit,
                                remaining_bytes,
                                remaining_records,
                                cancellation,
                            )
                        })
                        .ok_or_else(|| {
                            StyloProjectCaptureFailure::Invalid(
                                "Stylo worker budget was exhausted before invocation".into(),
                            )
                        })?;
                    match outcome {
                        Ok(value) => captured = Some(value),
                        Err(StyloCaptureFailure::Limit)
                        | Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Limit)) => {
                            failure = Some(StyloUnavailable::Limit)
                        }
                        Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Process(
                            backend_compile::ProcessError::Cancelled,
                        )))
                        | Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Terminal(
                            backend_compile::ProcessTerminal::Cancelled,
                        )))
                        | Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Native(
                            backend_frontend_stylo::StyloError::Cancelled,
                        ))) => return Err(StyloProjectCaptureFailure::Cancelled),
                        Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Process(
                            backend_compile::ProcessError::Deadline,
                        )))
                        | Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Terminal(
                            backend_compile::ProcessTerminal::Deadline,
                        ))) => failure = Some(StyloUnavailable::Deadline),
                        Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Process(
                            backend_compile::ProcessError::OutputLimit,
                        )))
                        | Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Terminal(
                            backend_compile::ProcessTerminal::OutputLimit,
                        ))) => failure = Some(StyloUnavailable::Limit),
                        Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Process(
                            backend_compile::ProcessError::UnsupportedLimit(_),
                        ))) => failure = Some(StyloUnavailable::UnsupportedHardLimit),
                        Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Native(
                            backend_frontend_stylo::StyloError::InvalidUtf8,
                        ))) => failure = Some(StyloUnavailable::InvalidUtf8),
                        Err(StyloCaptureFailure::Worker(StyloWorkerFailure::Native(
                            backend_frontend_stylo::StyloError::PreferencesChanged,
                        ))) => failure = Some(StyloUnavailable::PreferencesChanged),
                        Err(_) => failure = Some(StyloUnavailable::WorkerFailure),
                    }
                }
                Err(SourceFault::Vanished) => return Err(StyloProjectCaptureFailure::Stale),
                Err(SourceFault::Fatal(error)) => {
                    return Err(StyloProjectCaptureFailure::Invalid(error));
                }
                Err(SourceFault::UnavailableRead(reason, bytes_read)) => {
                    let _ = bytes_read; // already charged by read_stylo_bounded
                    failure = Some(if matches!(reason, SourceUnavailableReason::TooLarge) {
                        StyloUnavailable::Limit
                    } else {
                        StyloUnavailable::ReadUnavailable
                    });
                }
                Err(SourceFault::Unavailable(SourceUnavailableReason::TooLarge)) => {
                    failure = Some(StyloUnavailable::Limit)
                }
                Err(_) => failure = Some(StyloUnavailable::ReadUnavailable),
            }
        }
        let host = if let Some(value) = captured {
            remaining_records = remaining_records.saturating_sub(value.facts().len() + 1);
            remaining_bytes = remaining_bytes.saturating_sub(value.encoded_bytes());
            StyloProjectHost::Captured(value)
        } else {
            let row = StyloAnalysisRecord::Host {
                project: walk.project,
                path: name,
                content,
                source,
                byte_length,
                utf8_bom_removed: None,
                configuration: attempted_config
                    .as_ref()
                    .map(|config| {
                        backend_engine::builtin::stylo_parser_configuration_id(
                            backend_frontend_stylo::RECIPE,
                            config,
                        )
                    })
                    .transpose()
                    .map_err(StyloProjectCaptureFailure::Invalid)?,
                config: attempted_config,
                preferences: None,
                preference_stability: None,
                analysis_basis: None,
                worker_policy: attempted_policy,
                source_coverage: None,
                source_gaps: vec![],
                source_observer_recipe: None,
                terminal: StyloTerminal::Unavailable(
                    failure.ok_or(StyloProjectCaptureFailure::Unavailable)?,
                ),
                fact_count: 0,
                fact_digest: stylo_rows_digest(&[]).map_err(StyloProjectCaptureFailure::Invalid)?,
            };
            row.validate()
                .map_err(StyloProjectCaptureFailure::Invalid)?;
            let mut encoded = Vec::new();
            StyloAnalysisRelation::encode_value(&row, &mut encoded);
            if encoded.len() > remaining_bytes {
                complete = false;
                break;
            }
            remaining_bytes -= encoded.len();
            remaining_records -= 1;
            StyloProjectHost::Unavailable { key, row }
        };
        hosts.push(host);
    }
    hosts.sort_by_key(StyloProjectHost::key);
    let host_refs: Vec<_> = hosts.iter().map(|host| (host.key(), host.row())).collect();
    let host_digest = stylo_rows_digest(&host_refs).map_err(StyloProjectCaptureFailure::Invalid)?;
    let selection = StyloAnalysisRecord::Selection {
        project: walk.project,
        source_root,
        profile: StyloAnalysisProfile::StyloStandaloneV1,
        recipe: backend_frontend_stylo::RECIPE.into(),
        producer: permit.support(),
        worker_artifact: permit.artifact_identity(),
        adapter: super::stylo_admission::stylo_adapter_fingerprint(),
        discovery,
        policy_complete: false,
        max_worker_attempts: Some(MAX_STYLO_WORKER_ATTEMPTS),
        inventory: if complete && hosts.len() as u64 == inventory_count {
            StyloInventory::Complete
        } else {
            StyloInventory::Incomplete
        },
        inventory_digest,
        inventory_count,
        host_count: u32::try_from(hosts.len())
            .map_err(|_| StyloProjectCaptureFailure::Unavailable)?,
        host_digest,
    };
    selection
        .validate()
        .map_err(StyloProjectCaptureFailure::Invalid)?;
    let mut encoded = Vec::new();
    StyloAnalysisRelation::encode_value(&selection, &mut encoded);
    if encoded.len() > remaining_bytes {
        return Err(StyloProjectCaptureFailure::Unavailable);
    }
    let result = StyloProjectCapture {
        selection_key: stylo_selection_key(walk.project),
        selection,
        attempt: None,
        hosts,
        fence,
        source_bytes_read,
        worker_attempts: MAX_STYLO_WORKER_ATTEMPTS - worker_budget.remaining,
    };
    result.verify(cancellation)?;
    Ok(result)
}

/// Private source/configuration capture; it is not an admitted SQL root.
/// Publication must bind this witness to the selected project/snapshot and
/// revalidate it at the owner barrier before making any facts visible.
pub(super) struct SqlProjectCapture {
    project: [u8; 32],
    settings_bytes: Vec<u8>,
    settings: backend_library::sql_workspace::ParsedSqlProjectSettings,
    hosts: Vec<SqlCapturedHost>,
    cancellation: Arc<AtomicBool>,
    /// True when no `.nudox/sql.json` exists and the selection was derived from
    /// the same bounded walk (every `.sql` file, dialect inferred per file).
    implicit: bool,
}
pub(super) struct SqlCapturedHost {
    selected: backend_library::sql_workspace::SqlProjectSource,
    /// Dialect was inferred, not declared. `oracle` means the inference found no
    /// signal, so the owner may try the other dialect when the first rejects it.
    inferred: bool,
    oracle: bool,
    /// The host was selected but could not be read in full; no bytes were captured.
    fault: Option<backend_engine::builtin::SqlUnavailable>,
    bytes: Vec<u8>,
    input: backend_compile::InputContentVersion,
    source: backend_version::ContentId<backend_version::SourceFactDomain>,
}
/// Private replay witness after worker projection. It retains identities and
/// paths, not source/configuration byte buffers or catalog authority.
pub(super) struct SqlProjectWitness {
    implicit: bool,
    project: [u8; 32],
    config_input: backend_compile::InputContentVersion,
    config_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    config_len: usize,
    hosts: Vec<SqlHostWitness>,
    cancellation: Arc<AtomicBool>,
}
fn sql_charge_allocation(total: &mut usize, capacity: usize) -> Option<()> {
    if capacity != 0 { *total = total.checked_add(capacity)?.checked_add(2 * std::mem::size_of::<usize>())?; }
    Some(())
}
struct SqlHostWitness {
    path: String,
    input: backend_compile::InputContentVersion,
    source: backend_version::ContentId<backend_version::SourceFactDomain>,
    length: usize,
}
#[derive(Debug)]
pub(super) enum SqlCaptureFailure {
    NoConfiguration,
    Unavailable,
    Invalid,
    Limit,
    Stale,
    Cancelled,
}
impl SqlProjectCapture {
    /// Actual checked settings, still requiring owner selection admission.
    pub(super) fn settings(&self) -> &backend_library::sql_workspace::ParsedSqlProjectSettings { &self.settings }
    /// SQL hosts actually captured under the same confined root.
    pub(super) fn hosts(&self) -> &[SqlCapturedHost] { &self.hosts }
    /// Project claim carried by the source walk.
    pub(super) fn project(&self) -> [u8; 32] { self.project }
    pub(super) fn matches_cancellation(&self, cancellation: &Arc<AtomicBool>) -> bool { Arc::ptr_eq(&self.cancellation, cancellation) }
    pub(super) fn settings_len(&self) -> usize { self.settings_bytes.len() }
    /// Whether the selection was derived from the walk because no settings file exists.
    pub(super) fn implicit(&self) -> bool { self.implicit }
    pub(super) fn witness(&self) -> SqlProjectWitness {
        SqlProjectWitness { implicit: self.implicit, project: self.project, config_input: self.settings.input(),
            config_source: self.settings.source(), config_len: self.settings_bytes.len(),
            hosts: self.hosts.iter().filter(|host| host.fault.is_none()).map(|host| SqlHostWitness {
                path: host.selected.path.clone(), input: host.input, source: host.source, length: host.bytes.len(),
            }).collect(), cancellation: Arc::clone(&self.cancellation) }
    }
    /// Re-read actual configuration and every host before owner publication.
    pub(super) fn revalidate(&self, walk: &SqlWalkEvidence, fence: &CompilerRevisionFence,
        requested_root: &Path, cancellation: &Arc<AtomicBool>) -> Result<(), SqlCaptureFailure> {
        if !self.matches_cancellation(cancellation) { return Err(SqlCaptureFailure::Invalid); }
        if self.project != walk.project || walk.root != fence.root { return Err(SqlCaptureFailure::Stale); }
        if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
        let current = compiler_revision_is_current_with_cancellation(fence, Some(cancellation.as_ref()))
            .map_err(|_| if cancellation.load(Ordering::Acquire) { SqlCaptureFailure::Cancelled } else { SqlCaptureFailure::Unavailable })?;
        if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
        if !current {
            return Err(SqlCaptureFailure::Stale);
        }
        let root_result = fence.open_bound_root(requested_root);
        if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
        let root = root_result.map_err(|_| SqlCaptureFailure::Stale)?;
        if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
        let config_result = root.read(Path::new(backend_library::sql_workspace::SQL_PROJECT_SETTINGS_PATH),
            backend_library::sql_workspace::MAX_SQL_PROJECT_SETTINGS_BYTES);
        if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
        if self.implicit {
            // An implicit selection stays valid only while no settings file exists.
            match config_result {
                Err(SourceFault::Vanished) => {}
                Ok(_) => return Err(SqlCaptureFailure::Stale),
                Err(error) => return Err(sql_capture_fault(error)),
            }
        } else {
            let config = config_result.map_err(sql_capture_fault)?;
            if config != self.settings_bytes { return Err(SqlCaptureFailure::Stale); }
        }
        for host in self.hosts.iter().filter(|host| host.fault.is_none()) {
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            let result = root.read(Path::new(&host.selected.path), 512 * 1024);
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            let bytes = result.map_err(sql_capture_fault)?;
            let verified = backend_compile::VerifiedSourceText::admit(&bytes, host.input, host.source)
                .map_err(|_| SqlCaptureFailure::Stale)?;
            let extent = verified.extent(0..bytes.len()).map_err(|_| SqlCaptureFailure::Stale)?;
            if extent.text().as_bytes() != host.bytes { return Err(SqlCaptureFailure::Stale); }
        }
        let current = compiler_revision_is_current_with_cancellation(fence, Some(cancellation.as_ref()))
            .map_err(|_| if cancellation.load(Ordering::Acquire) { SqlCaptureFailure::Cancelled } else { SqlCaptureFailure::Unavailable })?;
        if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
        if !current {
            return Err(SqlCaptureFailure::Stale);
        }
        Ok(())
    }
}
impl SqlProjectWitness {
    pub(super) fn project(&self) -> [u8; 32] { self.project }
    pub(super) fn matches_cancellation(&self, cancellation: &Arc<AtomicBool>) -> bool { Arc::ptr_eq(&self.cancellation, cancellation) }
    pub(super) fn checked_retained_bytes(&self) -> Option<usize> {
        let mut total = std::mem::size_of::<Self>();
        sql_charge_allocation(&mut total, self.hosts.capacity().checked_mul(std::mem::size_of::<SqlHostWitness>())?)?;
        for host in &self.hosts { sql_charge_allocation(&mut total, host.path.capacity())?; }
        total.checked_add(std::mem::size_of::<AtomicBool>())?.checked_add(2 * std::mem::size_of::<usize>())
    }
    /// Recheck both typed content domains from the same confined bytes.
    pub(super) fn revalidate(&self, walk: &SqlWalkEvidence, fence: &CompilerRevisionFence,
        requested_root: &Path, cancellation: &Arc<AtomicBool>) -> Result<(), SqlCaptureFailure> {
        if !self.matches_cancellation(cancellation) { return Err(SqlCaptureFailure::Invalid); }
        if self.project != walk.project || walk.root != fence.root { return Err(SqlCaptureFailure::Stale); }
        let check_fence = || -> Result<(), SqlCaptureFailure> {
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            let current = compiler_revision_is_current_with_cancellation(fence, Some(cancellation.as_ref()))
                .map_err(|_| if cancellation.load(Ordering::Acquire) { SqlCaptureFailure::Cancelled } else { SqlCaptureFailure::Unavailable })?;
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            if !current { return Err(SqlCaptureFailure::Stale); }
            Ok(())
        };
        check_fence()?;
        let root_result = fence.open_bound_root(requested_root);
        if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
        let root = root_result.map_err(|_| SqlCaptureFailure::Stale)?;
        let check_bytes = |relative: &Path, limit: usize, length: usize,
                           input: backend_compile::InputContentVersion,
                           source: backend_version::ContentId<backend_version::SourceFactDomain>| -> Result<(), SqlCaptureFailure> {
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            let result = root.read(relative, limit);
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            let bytes = result.map_err(sql_capture_fault)?;
            if bytes.len() != length { return Err(SqlCaptureFailure::Stale); }
            backend_compile::VerifiedSourceText::admit(&bytes, input, source).map_err(|_| SqlCaptureFailure::Stale)?;
            Ok(())
        };
        if self.implicit {
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            let result = root.read(Path::new(backend_library::sql_workspace::SQL_PROJECT_SETTINGS_PATH),
                backend_library::sql_workspace::MAX_SQL_PROJECT_SETTINGS_BYTES);
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            match result {
                Err(SourceFault::Vanished) => {}
                Ok(_) => return Err(SqlCaptureFailure::Stale),
                Err(error) => return Err(sql_capture_fault(error)),
            }
        } else {
            check_bytes(Path::new(backend_library::sql_workspace::SQL_PROJECT_SETTINGS_PATH),
                backend_library::sql_workspace::MAX_SQL_PROJECT_SETTINGS_BYTES, self.config_len, self.config_input, self.config_source)?;
        }
        for host in &self.hosts { check_bytes(Path::new(&host.path), 512 * 1024, host.length, host.input, host.source)?; }
        check_fence()
    }
}
impl SqlCapturedHost {
    /// Exact configured source selector; its catalog remains proposed.
    pub(super) fn selected(&self) -> &backend_library::sql_workspace::SqlProjectSource { &self.selected }
    /// Host bytes used to mint the checked identities.
    pub(super) fn bytes(&self) -> &[u8] { &self.bytes }
    pub(super) fn input(&self) -> backend_compile::InputContentVersion { self.input }
    pub(super) fn source(&self) -> backend_version::ContentId<backend_version::SourceFactDomain> { self.source }
    /// Why the host could not be captured, if it was selected but unreadable.
    pub(super) fn fault(&self) -> Option<backend_engine::builtin::SqlUnavailable> { self.fault }
    /// Whether the dialect was inferred from the file rather than declared.
    pub(super) fn inferred(&self) -> bool { self.inferred }
    /// Whether no signal decided the dialect, so the parsers should.
    pub(super) fn oracle(&self) -> bool { self.oracle }
    /// A copy of this host re-targeted at the other dialect.
    pub(super) fn as_dialect(&self, dialect: backend_library::sql_workspace::SqlDialect) -> Self {
        let mut other = Self { selected: self.selected.clone(), inferred: self.inferred, oracle: self.oracle, fault: self.fault,
            bytes: self.bytes.clone(), input: self.input, source: self.source };
        other.selected.dialect = dialect;
        other
    }
}
fn sql_capture_fault(fault: SourceFault) -> SqlCaptureFailure {
    match fault {
        SourceFault::Vanished => SqlCaptureFailure::Stale,
        SourceFault::Unavailable(SourceUnavailableReason::TooLarge)
        | SourceFault::UnavailableRead(SourceUnavailableReason::TooLarge, _) => SqlCaptureFailure::Limit,
        SourceFault::Unavailable(_) | SourceFault::UnavailableRead(_, _) => SqlCaptureFailure::Unavailable,
        SourceFault::Fatal(_) => SqlCaptureFailure::Invalid,
    }
}
/// Capture only explicitly configured SQL hosts seen in the same bounded walk.
/// A missing settings file selects no SQL profile; missing/ignored configured
/// hosts and incomplete walks remain unavailable, not a complete empty result.
pub(super) fn capture_sql_project(walk: &SqlWalkEvidence, fence: &CompilerRevisionFence,
    requested_root: &Path, cancellation: &Arc<AtomicBool>) -> Result<SqlProjectCapture, SqlCaptureFailure> {
    if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
    if walk.root != fence.root { return Err(SqlCaptureFailure::Unavailable); }
    let root_result = fence.open_bound_root(requested_root);
    if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
    let root = root_result.map_err(|_| SqlCaptureFailure::Stale)?;
    let config_result = root.read(Path::new(backend_library::sql_workspace::SQL_PROJECT_SETTINGS_PATH),
        backend_library::sql_workspace::MAX_SQL_PROJECT_SETTINGS_BYTES);
    if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
    let (config, implicit) = match config_result {
        Ok(bytes) => (bytes, false),
        Err(SourceFault::Vanished) => (Vec::new(), true),
        Err(error) => return Err(sql_capture_fault(error)),
    };
    let mut hosts = Vec::new();
    let mut total = 0usize;
    let (settings, config) = if implicit {
        // No settings file: select every `.sql` file of the same bounded walk.
        let mut paths = Vec::new();
        for file in &walk.files {
            if file.metadata.is_none() { continue; }
            let Some(text) = file.relative_path.to_str() else { return Err(SqlCaptureFailure::Invalid) };
            let text = text.replace('\\', "/");
            if text == backend_library::sql_workspace::SQL_PROJECT_SETTINGS_PATH
                || !text.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("sql")) { continue; }
            paths.push(text);
        }
        paths.sort();
        paths.dedup();
        if paths.len() > backend_library::sql_workspace::MAX_SQL_PROJECT_HOSTS { return Err(SqlCaptureFailure::Limit); }
        let mut sources = Vec::with_capacity(paths.len());
        let mut read = Vec::with_capacity(paths.len());
        for path in &paths {
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            let result = root.read(Path::new(path), 512 * 1024);
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            let (bytes, fault) = match result {
                Ok(bytes) => (bytes, None),
                // Too large or unreadable hosts are still selected so that the
                // gap is recorded per host rather than silently dropped.
                Err(error) => match sql_capture_fault(error) {
                    SqlCaptureFailure::Limit => (Vec::new(), Some(backend_engine::builtin::SqlUnavailable::Limit)),
                    SqlCaptureFailure::Unavailable => (Vec::new(), Some(backend_engine::builtin::SqlUnavailable::Read)),
                    other => return Err(other),
                },
            };
            total = total.checked_add(bytes.len()).ok_or(SqlCaptureFailure::Limit)?;
            if total > 16 * 1024 * 1024 { return Err(SqlCaptureFailure::Limit); }
            let guess = infer_sql_dialect(path, &bytes);
            read.push((path.clone(), bytes, fault, guess));
        }
        // The migrations of one project share a dialect: when every file that
        // shows a signal agrees, the files with none inherit it. Only a genuine
        // mix leaves a file to the parsers.
        let mut seen = read.iter().filter_map(|(_, _, _, guess)| *guess).collect::<Vec<_>>();
        seen.sort_by_key(|dialect| matches!(dialect, backend_library::sql_workspace::SqlDialect::Postgres));
        seen.dedup();
        let consensus = (seen.len() == 1).then(|| seen[0]);
        for (path, bytes, fault, guess) in read {
            let decided = guess.or(consensus);
            let dialect = decided.unwrap_or(backend_library::sql_workspace::SqlDialect::Sqlite);
            let selected = backend_library::sql_workspace::SqlProjectSource {
                path: path.clone(), dialect, context: backend_library::sql_workspace::SqlProjectContext::Unavailable,
            };
            let verified = backend_compile::VerifiedSourceText::capture(&bytes).map_err(|_| SqlCaptureFailure::Invalid)?;
            let input = verified.content();
            let source = verified.extent(0..bytes.len()).map_err(|_| SqlCaptureFailure::Invalid)?.source();
            sources.push(selected.clone());
            hosts.push(SqlCapturedHost { selected, inferred: true, oracle: decided.is_none(), fault, bytes, input, source });
        }
        let bytes = serde_json::to_vec(&backend_library::sql_workspace::SqlProjectSettings { version: 1, sources })
            .map_err(|_| SqlCaptureFailure::Invalid)?;
        let settings = backend_library::sql_workspace::ParsedSqlProjectSettings::parse(&bytes)
            .map_err(|_| SqlCaptureFailure::Invalid)?;
        (settings, bytes)
    } else {
        // A settings file selects hosts by name; a truncated walk cannot prove
        // that every named file is present and unignored.
        if !walk.inventory_complete { return Err(SqlCaptureFailure::Unavailable); }
        let settings = backend_library::sql_workspace::ParsedSqlProjectSettings::parse(&config)
            .map_err(|_| SqlCaptureFailure::Invalid)?;
        for selected in &settings.settings().sources {
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            let path = Path::new(&selected.path);
            if !walk.files.iter().any(|file| file.relative_path == path && file.metadata.is_some()) {
                return Err(SqlCaptureFailure::Unavailable);
            }
            let result = root.read(path, 512 * 1024);
            if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
            let bytes = result.map_err(sql_capture_fault)?;
            total = total.checked_add(bytes.len()).ok_or(SqlCaptureFailure::Limit)?;
            if total > 16 * 1024 * 1024 { return Err(SqlCaptureFailure::Limit); }
            let verified = backend_compile::VerifiedSourceText::capture(&bytes).map_err(|_| SqlCaptureFailure::Invalid)?;
            let input = verified.content();
            let source = verified.extent(0..bytes.len()).map_err(|_| SqlCaptureFailure::Invalid)?.source();
            hosts.push(SqlCapturedHost { selected: selected.clone(), inferred: false, oracle: false, fault: None, bytes, input, source });
        }
        (settings, config)
    };
    if cancellation.load(Ordering::Acquire) { return Err(SqlCaptureFailure::Cancelled); }
    Ok(SqlProjectCapture { project: walk.project, settings_bytes: config, settings, hosts, cancellation: Arc::clone(cancellation), implicit })
}

/// Choose a dialect from the file's path and its own tokens. `None` means no
/// signal; the owner then lets the native parsers decide. Never a guess that
/// is hidden: such hosts are recorded as inferred.
fn infer_sql_dialect(path: &str, bytes: &[u8]) -> Option<backend_library::sql_workspace::SqlDialect> {
    use backend_library::sql_workspace::SqlDialect;
    let lower_path = path.to_ascii_lowercase();
    let head = &bytes[..bytes.len().min(256 * 1024)];
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    const POSTGRES: &[&str] = &["$$", "::", "create extension", "plpgsql", " serial", "bigserial", "timestamptz",
        "jsonb", "create schema", "returns ", "language sql", "set search_path", "create type", " uuid", "using gin",
        "generated always as identity", "create or replace function", "create sequence", "materialized view", "on conflict"];
    const SQLITE: &[&str] = &["pragma ", "autoincrement", "without rowid", "insert or replace", "insert or ignore",
        "sqlite_", "create virtual table", "strict;"];
    let pg = POSTGRES.iter().filter(|needle| text.contains(**needle)).count()
        + usize::from(lower_path.contains("postgres") || lower_path.contains("pgsql") || lower_path.contains("psql"));
    let lite = SQLITE.iter().filter(|needle| text.contains(**needle)).count()
        + usize::from(lower_path.contains("sqlite"));
    match (pg > 0, lite > 0) {
        (true, false) => Some(SqlDialect::Postgres),
        (false, true) => Some(SqlDialect::Sqlite),
        _ => None,
    }
}

/// Private same-walk Vue host capture. This capability does not authorize a
/// workspace transition on its own; the owner must join the selected Project,
/// exact predecessor, source/configuration basis and closure before publishing.
pub(super) struct VueProjectCapture {
    scan_identity: std::sync::Arc<()>,
    project: [u8; 32],
    basis: [u8; 32],
    project_source_version: [u8; 32],
    source_bytes_read: usize,
    discovery: [u8; 32],
    walk: VueWalkEvidence,
    fence: CompilerRevisionFence,
    rows: Vec<([u8; 32], backend_frontend_vue::VueInventoryRecord)>,
}
impl VueProjectCapture {
    /// This is only a custody check; the owner must authenticate the selected root
    /// and prove its staged project after-image came from this exact scan.
    pub(super) fn belongs_to_scan(&self, scan: &IndexSnapshot, selected_root: &Path) -> bool {
        std::sync::Arc::ptr_eq(&self.scan_identity, &scan.vue_scan_identity)
            && self.walk.root == selected_root
            && scan.vue_walk.root == selected_root
            && self.project == scan.vue_walk.project
            && self.project_source_version == scan.source_version
    }
    pub(super) fn project(&self) -> [u8; 32] {
        self.project
    }
    pub(super) fn project_source_version(&self) -> [u8; 32] { self.project_source_version }
    pub(super) fn walk_complete(&self) -> bool { self.walk.complete }
    pub(super) fn source_bytes_read(&self) -> usize { self.source_bytes_read }
    pub(super) fn rows(&self) -> &[([u8; 32], backend_frontend_vue::VueInventoryRecord)] {
        &self.rows
    }
    /// Recheck exact host bytes and identities through the bound root before
    /// an owner transition. Unavailable hosts cannot become complete negatives.
    pub(super) fn verify(&self, cancellation: &AtomicBool) -> Result<(), String> {
        use backend_frontend_vue::{ExecutionCheckpoint as _, VueInventoryRecord};
        let budget = backend_frontend_vue::ProjectExecutionBudget::new(
            std::time::Instant::now() + std::time::Duration::from_secs(10), cancellation, 5_000_000,
        );
        if !compiler_revision_is_current_with_cancellation(&self.fence, Some(cancellation))? {
            return Err("Vue source fence changed".into());
        }
        let root = self.fence.open_bound_root(&self.fence.requested_root)?;
        if vue_discovery_witness(&self.walk, &root, cancellation)? != self.discovery {
            return Err("Vue discovery policy changed".into());
        }
        let mut verification_bytes = 0usize;
        for (key, row) in &self.rows {
            budget.checkpoint(1).map_err(|_| "Vue verification stopped")?;
            if cancellation.load(Ordering::Acquire) {
                return Err("Vue capture cancelled".into());
            }
            if row.key() != *key {
                return Err("Vue row key differs".into());
            }
            if let VueInventoryRecord::Host(host) = row {
                let (project, path) = host.owner();
                if project != self.project {
                    return Err("Vue host owner differs".into());
                }
                let bytes = root
                    .read(Path::new(path), 1024 * 1024)
                    .map_err(|_| "Vue host unavailable during revalidation")?;
                verification_bytes = verification_bytes.checked_add(bytes.len()).ok_or("Vue verification work overflow")?;
                if verification_bytes > 64 * 1024 * 1024 { return Err("Vue verification work limit".into()); }
                host.verify_host_with_checkpoint(&bytes, *key, self.project, path, self.basis, &budget)?;
            }
        }
        if !compiler_revision_is_current_with_cancellation(&self.fence, Some(cancellation))? {
            return Err("Vue source changed during revalidation".into());
        }
        Ok(())
    }
}
fn vue_discovery_witness(walk: &VueWalkEvidence, root: &ProjectRoot, cancellation: &AtomicBool) -> Result<[u8; 32], String> {
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.vue-discovery.v1\0");
    hash.update(&(walk.discovery_options.len() as u64).to_be_bytes());
    hash.update(&walk.discovery_options);
    hash.update(&walk.source_policy);
    let mut charged = 0usize;
    for path in &walk.local_policy_paths {
        if cancellation.load(Ordering::Acquire) { return Err("Vue discovery cancelled".into()); }
        let relative = path.strip_prefix(&walk.root).map_err(|_| "Vue policy escaped root")?;
        let name = relative.to_str().ok_or("Vue policy path is not UTF-8")?;
        let remaining = (16 * 1024 * 1024usize).saturating_sub(charged);
        if remaining == 0 { return Err("Vue policy byte limit".into()); }
        let bytes = root.read(relative, remaining.min(1024 * 1024)).map_err(|_| "Vue policy unavailable")?;
        charged = charged.checked_add(bytes.len()).ok_or("Vue policy byte overflow")?;
        hash.update(&(name.len() as u64).to_be_bytes()); hash.update(name.as_bytes());
        hash.update(&(bytes.len() as u64).to_be_bytes()); hash.update(&bytes);
    }
    Ok(*hash.finalize().as_bytes())
}

/// Capture Vue candidates from the already-confined source walk. Read and
/// parse failures are explicit host rows; the selection is always partial.
pub(super) fn capture_vue_project(
    scan: &IndexSnapshot,
    cancellation: &AtomicBool,
) -> Result<VueProjectCapture, String> {
    use backend_frontend_vue::{
        ExecutionCheckpoint as _, VueHostInventory, VueInventoryRecord, VueSourceError, VueUnavailable,
    };
    let budget = backend_frontend_vue::ProjectExecutionBudget::new(
        std::time::Instant::now() + std::time::Duration::from_secs(10), cancellation, 5_000_000,
    );
    let walk = &scan.vue_walk;
    let fence = walk.bind_fence(&scan.revision_fence)?;
    if !compiler_revision_is_current_with_cancellation(&fence, Some(cancellation))? {
        return Err("Vue source fence changed".into());
    }
    let root = fence.open_bound_root(&fence.requested_root)?;
    let discovery = vue_discovery_witness(walk, &root, cancellation)?;
    let mut digest = blake3::Hasher::new();
    digest.update(b"nudox.vue-analysis-basis.v1\0");
    digest.update(&backend_frontend_vue::support_fingerprint());
    digest.update(&discovery);
    let basis = *digest.finalize().as_bytes();
    let limits = scan.source_admission_policy.limits();
    let max_file = limits
        .max_file_source_bytes
        .min(limits.max_in_flight_source_bytes)
        .min(1024 * 1024);
    let mut remaining = limits
        .max_project_source_bytes
        .saturating_sub(scan.stylo_budget_baseline.source_bytes)
        .min(64 * 1024 * 1024);
    let mut work = 0usize;
    let mut unavailable = 0u32;
    let mut rows = Vec::with_capacity(walk.files.len().saturating_add(1));
    for file in &walk.files {
        budget.checkpoint(1).map_err(|_| "Vue capture stopped")?;
        if cancellation.load(Ordering::Acquire) {
            return Err("Vue capture cancelled".into());
        }
        let path = file
            .relative_path
            .to_str()
            .ok_or("Vue path is not UTF-8")?
            .replace(std::path::MAIN_SEPARATOR, "/");
        let read = match read_stylo_bounded(
            &root, &file.relative_path, max_file, &mut remaining, &mut work,
        ) {
            Ok(bytes) => Ok(bytes),
            Err(SourceFault::Fatal(message)) => return Err(message),
            Err(SourceFault::Unavailable(SourceUnavailableReason::TooLarge))
            | Err(SourceFault::UnavailableRead(SourceUnavailableReason::TooLarge, _)) => Err(VueUnavailable::Limit),
            Err(_) => Err(VueUnavailable::Read),
        };
        let result = read.and_then(|bytes| {
            let parsed = backend_frontend_vue::parse(&bytes).map_err(|error| match error {
                VueSourceError::NonUtf8 => VueUnavailable::InvalidUtf8,
                VueSourceError::SourceLimit | VueSourceError::WorkLimit => VueUnavailable::Limit,
                _ => VueUnavailable::Parser,
            })?;
            VueHostInventory::capture_with_checkpoint(walk.project, &path, basis, &parsed, &budget)
                .map_err(|_| VueUnavailable::Limit)
        });
        budget.checkpoint(1).map_err(|_| "Vue capture stopped")?;
        let row = match result {
            Ok(host) => VueInventoryRecord::Host(host),
            Err(reason) => {
                unavailable = unavailable
                    .checked_add(1)
                    .ok_or("Vue unavailable count overflow")?;
                VueInventoryRecord::Unavailable {
                    project: walk.project,
                    path,
                    reason,
                }
            }
        };
        rows.push((row.key(), row));
    }
    let mut paths = walk.files.iter().map(|file| file.relative_path.to_str().map(|path| path.replace(std::path::MAIN_SEPARATOR, "/")).ok_or("Vue path is not UTF-8")).collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    let inventory_digest = backend_frontend_vue::vue_path_inventory_digest(&paths)?;
    let selection = VueInventoryRecord::Selection {
        project: walk.project,
        source_root: walk.root.to_str().ok_or("Vue root is not UTF-8")?.to_owned(),
        basis,
        producer: backend_frontend_vue::support_fingerprint(),
        discovered: u32::try_from(walk.files.len()).map_err(|_| "Vue selection overflow")?,
        inventory_digest,
        unavailable,
        walk_truncated: !walk.complete,
    };
    rows.push((selection.key(), selection));
    rows.sort_unstable_by_key(|(key, _)| *key);
    backend_engine::builtin::validate_vue_inventory_rows(&rows)?;
    if !compiler_revision_is_current_with_cancellation(&fence, Some(cancellation))? {
        return Err("Vue source changed during capture".into());
    }
    Ok(VueProjectCapture {
        scan_identity: std::sync::Arc::clone(&scan.vue_scan_identity),
        project: walk.project,
        basis,
        project_source_version: scan.source_version,
        source_bytes_read: work,
        discovery,
        walk: walk.clone(),
        fence,
        rows,
    })
}

#[cfg(test)]
mod vue_same_walk_tests {
    use super::*;
    use backend_frontend_vue::VueInventoryRecord;
    use std::sync::atomic::AtomicBool;

    #[test]
    fn same_walk_vue_capture_rejects_add_edit_and_delete() -> Result<(), String> {
        let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
        fs::create_dir_all(temp.path().join("src")).map_err(|e| e.to_string())?;
        let path = temp.path().join("src/Card.vue");
        fs::write(&path, "\u{feff}<!-- π🚀 -->\r\n<script setup lang='ts'>const café = 1;</script>").map_err(|e| e.to_string())?;
        let root = temp.path().to_str().ok_or("non-UTF8 scratch")?;
        let scan = scan_project(root, [7;32], &BTreeMap::new())?;
        let cancellation = AtomicBool::new(false);
        let capture = capture_vue_project(&scan, &cancellation)?;
        assert_eq!(capture.rows().len(), 2);
        assert!(capture.rows().iter().any(|(_, row)| matches!(row, VueInventoryRecord::Host(_))));
        capture.verify(&cancellation)?;
        assert!(capture.belongs_to_scan(&scan, &scan.vue_walk.root));
        let other = tempfile::tempdir().map_err(|e| e.to_string())?;
        fs::create_dir_all(other.path().join("src")).map_err(|e| e.to_string())?;
        fs::copy(&path, other.path().join("src/Card.vue")).map_err(|e| e.to_string())?;
        let other_root = other.path().to_str().ok_or("non-UTF8 scratch")?;
        let other_scan = scan_project(other_root, [7;32], &BTreeMap::new())?;
        assert_eq!(scan.source_version, other_scan.source_version);
        assert!(!capture.belongs_to_scan(&other_scan, &other_scan.vue_walk.root));
        assert!(!capture.belongs_to_scan(&scan, &other_scan.vue_walk.root));
        let added = temp.path().join("src/New.vue");
        fs::write(&added, "<script>const added = 1;</script>").map_err(|e| e.to_string())?;
        assert!(capture.verify(&cancellation).is_err());
        fs::remove_file(&added).map_err(|e| e.to_string())?;
        let scan = scan_project(root, [7;32], &BTreeMap::new())?;
        let capture = capture_vue_project(&scan, &cancellation)?;
        capture.verify(&cancellation)?;
        fs::write(&path, "<script>const edited = 2;</script>").map_err(|e| e.to_string())?;
        assert!(capture.verify(&cancellation).is_err());
        fs::remove_file(path).map_err(|e| e.to_string())?;
        assert!(capture.verify(&cancellation).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod vue_stylo_region_tests {
    use backend_compile::VerifiedSourceText;
    use backend_frontend_stylo::worker_protocol::{decode_fragment_response, encode_fragment_request, serve};
    use backend_semantic::stylo_analysis::{StyloLimits, StyloParseConfig, StyloSourceKind};

    #[test]
    fn vue_plain_css_flows_through_fragment_worker_with_host_offsets() {
        let text = "\u{feff}<!-- π🚀 -->\r\n<template><p>hello</p></template>\r\n<style scoped>\u{feff} {} .card { --brand: red; }</style>\r\n<style lang='scss'>.skip { @include mix; }</style>";
        let vue = backend_frontend_vue::parse(text.as_bytes()).unwrap();
        let css = vue.inline_css(1).unwrap();
        assert!(vue.inline_css(2).is_err());
        let source = VerifiedSourceText::capture(text.as_bytes()).unwrap();
        let host = source.extent(0..text.len()).unwrap();
        let range = css.byte_range();
        let config = StyloParseConfig {
            source_kind: StyloSourceKind::EmbeddedUnicodeFragment,
            base_url: "https://example.invalid/src/Card.vue".into(), author_origin: true,
            no_quirks: true, allow_imports: false,
            limits: StyloLimits { input_bytes: 4096, rules: 128, depth: 16, records: 256, diagnostics: 32, text_bytes: 65_536 },
        };
        let request = encode_fragment_request(&host, range.clone(), &config).unwrap();
        let mut response = Vec::new();
        serve(request.as_slice(), &mut response).unwrap();
        let analysis = decode_fragment_response(&response, &host, range, &config, backend_frontend_stylo::support_fingerprint()).unwrap();
        assert!(!analysis.analysis().utf8_bom_removed);
        let i = analysis.analysis().occurrences.iter().position(|o| o.raw == "card").unwrap();
        let site = analysis.host_occurrence(i).unwrap();
        assert_eq!(&text[site.byte_range()], "card");
        assert_eq!(site.source(), vue.host().source());
    }
}
