//! Bounded persisted claims for native source bytes without structural inventory.
//!
//! A decoded row is not proof of file ownership or byte identity. Admission must
//! join the selected project and native capture/image and verify actual confined
//! host bytes in both content domains. Missing rows mean unknown inventory.
use crate::workspace::{WorkspaceRelationError, WorkspaceRelationHandle, WorkspaceSnapshot};
use backend_semantic::vocabulary::LanguageProfile;
use backend_semantic::ir::{DeclarationFamilyId, DeclarationIdentity, VariantFingerprint};
use backend_store::TypedObject;
use backend_version::{
    CanonicalRelation, ObjectKey, Relation, RelationDecodeError, Schema, SchemaIdentity, StateRoot,
};
use std::collections::BTreeMap;

const MAGIC: &[u8; 4] = b"NSE1";
const MAGIC_V2: &[u8; 4] = b"NSE2";
const MAGIC_V3: &[u8; 4] = b"NSE3";
pub const MAX_NATIVE_HEADER_ENTRIES: usize = 4096;
const HEADER_ENTRY_BYTES: usize = 52;
/// Maximum retained line starts per file.
pub const MAX_NATIVE_EVIDENCE_LINES: usize = 1_000_000;
const ROOT_KEY: [u8; 32] = [0x45; 32];
/// Maximum path and captured input sizes, matching native source admission.
pub const MAX_NATIVE_EVIDENCE_PATH: usize = 4096;
pub const MAX_NATIVE_EVIDENCE_SOURCE: u32 = 64 * 1024 * 1024;
/// Limits include canonical keys, values, and per-row framing.
pub const MAX_NATIVE_EVIDENCE_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_NATIVE_EVIDENCE_ROWS: usize = 262_144;
pub const MAX_NATIVE_EVIDENCE_PROJECT_ROWS: usize = 100_000;
/// Maximum canonical row value; framing and key are charged separately.
pub const MAX_NATIVE_EVIDENCE_VALUE: usize =
    4 + 32 + 2 + 2 + MAX_NATIVE_EVIDENCE_PATH + 32 + 32 + 4 + 1 + 1 + 4 + MAX_NATIVE_EVIDENCE_LINES * 4 + 32 + 8 + 32 + 4 + MAX_NATIVE_HEADER_ENTRIES * HEADER_ENTRY_BYTES;

/// Independently selected native-source claim relation.
#[derive(Debug)]
pub struct NativeSourceEvidenceRelation;
/// Typed pointer to the selected evidence root.
pub struct NativeSourceEvidenceRootSchema;
impl Schema for NativeSourceEvidenceRootSchema {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 15;
    const VERSION: u8 = 1;
    type Value = [u8; 32];
    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
}

/// Relationship of the source to the selected native program.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeSourceEvidenceKind {
    /// Member of the program without its own emitted image.
    ProgramMember,
    /// Program member with an independently admitted emitted image.
    Image,
}

/// Untrusted header claims; consumption requires an independent compiler/image join.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeHeaderEvidence {
    pub image_identity: [u8; 32],
    pub image_byte_len: u64,
    pub producer_recipe: [u8; 32],
    pub entries: Vec<NativeHeaderEntry>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeHeaderEntry {
    pub entity_id: u32,
    pub identity: DeclarationIdentity,
    pub compiler_extent: (u32, u32),
    pub header_extent: (u32, u32),
}

/// Untrusted persisted claims, not a verified source capability.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeSourceEvidence {
    pub project: [u8; 32],
    pub profile: LanguageProfile,
    pub path: String,
    /// Raw expected input digest; it cannot be converted into a typed identity.
    pub input_digest: [u8; 32],
    /// Raw expected source fact digest; verification mints the typed identity from bytes.
    pub source_digest: [u8; 32],
    pub byte_length: u32,
    /// Program membership is distinct from image provenance.
    pub kind: NativeSourceEvidenceKind,
    /// Complete line-start offsets captured from these exact bytes, if representable.
    pub line_starts: Option<Vec<u32>>,
    pub headers: Option<NativeHeaderEvidence>,
}
impl NativeSourceEvidence {
    /// Whether this claim names a separately admitted program image.
    pub fn admits_image(&self) -> bool { self.kind == NativeSourceEvidenceKind::Image }

    /// Exact canonical value length, without allocating an encoded copy.
    pub fn checked_encoded_len(&self) -> Option<usize> {
        let base = 4usize.checked_add(32 + 2 + 2)?.checked_add(self.path.len())?.checked_add(32 + 32 + 4 + 1)?;
        if self.headers.is_none() && self.kind == NativeSourceEvidenceKind::Image && self.line_starts.is_none() { return Some(base); }
        let lines = self.line_starts.as_ref().map_or(0, Vec::len);
        let mut size = base.checked_add(1 + 4)?.checked_add(lines.checked_mul(4)?)?;
        if let Some(headers) = &self.headers {
            size = size.checked_add(76)?.checked_add(headers.entries.len().checked_mul(HEADER_ENTRY_BYTES)?)?;
        }
        Some(size)
    }

    /// Heap owned by this row, excluding the row itself and allocator metadata.
    pub fn checked_heap_bytes(&self) -> Option<usize> {
        let lines = self.line_starts.as_ref().map_or(Some(0), |v| v.capacity().checked_mul(std::mem::size_of::<u32>()))?;
        let headers = self.headers.as_ref().map_or(Some(0), |h| h.entries.capacity().checked_mul(std::mem::size_of::<NativeHeaderEntry>()))?;
        self.path.capacity().checked_add(lines)?.checked_add(headers)
    }

    /// Captures a bounded complete line map when line endings are representable.
    pub fn capture_line_starts(bytes: &[u8]) -> Option<Vec<u32>> {
        let text = std::str::from_utf8(bytes).ok()?;
        if text.contains('\u{2028}') || text.contains('\u{2029}') { return None; }
        let mut starts = vec![0];
        for (i, byte) in bytes.iter().enumerate() {
            if *byte == b'\r' && bytes.get(i + 1) != Some(&b'\n') { return None; }
            if *byte == b'\n' {
                if starts.len() == MAX_NATIVE_EVIDENCE_LINES { return None; }
                starts.push(u32::try_from(i + 1).ok()?);
            }
        }
        Some(starts)
    }

    /// Returns the one-based line of an offset in the captured source.
    pub fn line_at(&self, offset: u32) -> Option<u32> {
        if offset > self.byte_length { return None; }
        u32::try_from(self.line_starts.as_ref()?.partition_point(|start| *start <= offset)).ok()
    }

    /// No declaration inventory completeness follows from this row.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.byte_length > MAX_NATIVE_EVIDENCE_SOURCE {
            return Err("invalid native source evidence length");
        }
        if let Some(lines) = &self.line_starts {
            if lines.is_empty() || lines.len() > MAX_NATIVE_EVIDENCE_LINES || lines[0] != 0
                || lines.windows(2).any(|pair| pair[0] >= pair[1])
                || lines.last().is_some_and(|last| *last > self.byte_length)
            { return Err("invalid native source line map"); }
        }
        if let Some(headers) = &self.headers {
            if !self.admits_image() || headers.entries.len() > MAX_NATIVE_HEADER_ENTRIES
                || headers.entries.windows(2).any(|p| p[0].entity_id >= p[1].entity_id)
                || headers.entries.iter().any(|entry| {
                    let (start, end) = entry.compiler_extent;
                    let (head_start, head_end) = entry.header_extent;
                    start >= end || end > self.byte_length || head_start > start
                        || start >= head_end || head_end > end
                }) { return Err("invalid native header evidence"); }
        }
        native_source_evidence_key(self.project, self.profile, &self.path).map(|_| ())
    }
    /// Checks actual bytes against this claim and an independently admitted image identity.
    /// This does not establish workspace selection, ownership, or freshness.
    pub fn verify_host<'a>(
        &self,
        bytes: &'a [u8],
        image_source: backend_version::ContentId<backend_version::SourceFactDomain>,
    ) -> Result<backend_compile::VerifiedSourceText<'a>, &'static str> {
        self.validate()?;
        if !self.admits_image() {
            return Err("native program member has no admitted image");
        }
        if bytes.len() != self.byte_length as usize {
            return Err("native source length differs");
        }
        if self.line_starts.is_some() && Self::capture_line_starts(bytes) != self.line_starts {
            return Err("native source line map differs");
        }
        if image_source.as_ref() != self.source_digest.as_slice() {
            return Err("native image source differs from evidence");
        }
        backend_compile::VerifiedSourceText::admit_stored(bytes, self.input_digest, image_source)
            .map_err(|_| "native source bytes do not match evidence")
    }
    /// Key binds the complete canonical owner, profile and path.
    pub fn key(&self) -> Result<[u8; 32], &'static str> {
        self.validate()?;
        native_source_evidence_key(self.project, self.profile, &self.path)
    }
}
/// Derives a lookup key without constructing or trusting a source claim.
pub fn native_source_evidence_key(
    project: [u8; 32],
    profile: LanguageProfile,
    path: &str,
) -> Result<[u8; 32], &'static str> {
    if project == [0; 32]
        || path.is_empty()
        || path.len() > MAX_NATIVE_EVIDENCE_PATH
        || path.starts_with('/')
        || path.contains('\\')
        || path.bytes().any(|b| b.is_ascii_control())
        || (path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic())
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err("invalid native source evidence owner");
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.native-source-evidence.v1\0");
    hash.update(&project);
    hash.update(&<[u8; 2]>::from(profile));
    hash.update(&(path.len() as u32).to_be_bytes());
    hash.update(path.as_bytes());
    Ok(*hash.finalize().as_bytes())
}
impl Relation for NativeSourceEvidenceRelation {
    const DOMAIN: u8 = 0x97;
    const TYPE: u16 = 14;
    const VERSION: u8 = 1;
    type Key = [u8; 32];
    type Value = NativeSourceEvidence;
    fn encode_key(value: &Self::Key, output: &mut Vec<u8>) {
        output.extend_from_slice(value);
    }
    fn encode_value(value: &Self::Value, output: &mut Vec<u8>) {
        let legacy = value.headers.is_none() && value.kind == NativeSourceEvidenceKind::Image && value.line_starts.is_none();
        output.extend_from_slice(if value.headers.is_some() { MAGIC_V3 } else if legacy { MAGIC } else { MAGIC_V2 });
        output.extend_from_slice(&value.project);
        output.extend_from_slice(&<[u8; 2]>::from(value.profile));
        output.extend_from_slice(&(value.path.len() as u16).to_be_bytes());
        output.extend_from_slice(value.path.as_bytes());
        output.extend_from_slice(&value.input_digest);
        output.extend_from_slice(&value.source_digest);
        output.extend_from_slice(&value.byte_length.to_be_bytes());
        output.push(0); // InventoryUnknown; no complete-inventory claim.
        if legacy { return; }
        output.push(match value.kind { NativeSourceEvidenceKind::ProgramMember => 0, NativeSourceEvidenceKind::Image => 1 });
        let lines = value.line_starts.as_deref().unwrap_or(&[]);
        output.extend_from_slice(&(lines.len() as u32).to_be_bytes());
        for line in lines { output.extend_from_slice(&line.to_be_bytes()); }
        if let Some(headers) = &value.headers {
            output.extend_from_slice(&headers.image_identity);
            output.extend_from_slice(&headers.image_byte_len.to_be_bytes());
            output.extend_from_slice(&headers.producer_recipe);
            output.extend_from_slice(&(headers.entries.len() as u32).to_be_bytes());
            for entry in &headers.entries {
                output.extend_from_slice(&entry.entity_id.to_be_bytes());
                output.extend_from_slice(entry.identity.family.as_bytes());
                output.extend_from_slice(entry.identity.variant.as_bytes());
                for extent in [entry.compiler_extent.0, entry.compiler_extent.1, entry.header_extent.0, entry.header_extent.1] {
                    output.extend_from_slice(&extent.to_be_bytes());
                }
            }
        }
    }
}
impl CanonicalRelation for NativeSourceEvidenceRelation {
    fn decode_key(bytes: &[u8]) -> Result<Self::Key, RelationDecodeError> {
        bytes.try_into().map_err(|_| RelationDecodeError::Malformed)
    }
    fn decode_value(bytes: &[u8]) -> Result<Self::Value, RelationDecodeError> {
        fn parse(bytes: &[u8]) -> Option<NativeSourceEvidence> {
            if bytes.len() > MAX_NATIVE_EVIDENCE_VALUE || (bytes.get(..4)? != MAGIC && bytes.get(..4)? != MAGIC_V2 && bytes.get(..4)? != MAGIC_V3) {
                return None;
            }
            let project = bytes.get(4..36)?.try_into().ok()?;
            let profile =
                LanguageProfile::try_from(<[u8; 2]>::try_from(bytes.get(36..38)?).ok()?).ok()?;
            let len = u16::from_be_bytes(bytes.get(38..40)?.try_into().ok()?) as usize;
            if len == 0 || len > MAX_NATIVE_EVIDENCE_PATH { return None; }
            let end = 40usize.checked_add(len)?;
            let path = std::str::from_utf8(bytes.get(40..end)?).ok()?.to_owned();
            let rest = bytes.get(end..)?;
            if rest.len() < 69 || rest[68] != 0 { return None; }
            let (kind, line_starts, headers) = if bytes.get(..4)? == MAGIC {
                if rest.len() != 69 { return None; }
                (NativeSourceEvidenceKind::Image, None, None)
            } else {
                let kind = match *rest.get(69)? { 0 => NativeSourceEvidenceKind::ProgramMember, 1 => NativeSourceEvidenceKind::Image, _ => return None };
                let count = u32::from_be_bytes(rest.get(70..74)?.try_into().ok()?) as usize;
                let line_end = 74usize.checked_add(count.checked_mul(4)?)?;
                if count > MAX_NATIVE_EVIDENCE_LINES || line_end > rest.len() { return None; }
                if bytes.get(..4)? == MAGIC_V2 && line_end != rest.len() { return None; }
                if bytes.get(..4)? == MAGIC_V2 && kind == NativeSourceEvidenceKind::Image && count == 0 { return None; }
                let headers = if bytes.get(..4)? == MAGIC_V3 {
                    let tail = rest.get(line_end..)?;
                    let header_count = u32::from_be_bytes(tail.get(72..76)?.try_into().ok()?) as usize;
                    if header_count > MAX_NATIVE_HEADER_ENTRIES || tail.len() != 76usize.checked_add(header_count.checked_mul(HEADER_ENTRY_BYTES)?)? { return None; }
                    let mut entries = Vec::with_capacity(header_count);
                    for chunk in tail.get(76..)?.chunks_exact(HEADER_ENTRY_BYTES) {
                        let number = |offset| Some(u32::from_be_bytes(chunk.get(offset..offset+4)?.try_into().ok()?));
                        entries.push(NativeHeaderEntry {
                            entity_id: number(0)?,
                            identity: DeclarationIdentity { family: DeclarationFamilyId::from_raw(chunk.get(4..20)?.try_into().ok()?), variant: VariantFingerprint::from_raw(chunk.get(20..36)?.try_into().ok()?) },
                            compiler_extent: (number(36)?, number(40)?), header_extent: (number(44)?, number(48)?),
                        });
                    }
                    Some(NativeHeaderEvidence { image_identity: tail.get(..32)?.try_into().ok()?, image_byte_len: u64::from_be_bytes(tail.get(32..40)?.try_into().ok()?), producer_recipe: tail.get(40..72)?.try_into().ok()?, entries })
                } else { None };
                let mut lines = Vec::with_capacity(count);
                for chunk in rest.get(74..line_end)?.chunks_exact(4) { lines.push(u32::from_be_bytes(chunk.try_into().ok()?)); }
                (kind, if count == 0 { None } else { Some(lines) }, headers)
            };
            let record = NativeSourceEvidence {
                project,
                profile,
                path,
                input_digest: rest.get(..32)?.try_into().ok()?,
                source_digest: rest.get(32..64)?.try_into().ok()?,
                byte_length: u32::from_be_bytes(rest.get(64..68)?.try_into().ok()?),
                kind,
                line_starts,
                headers,
            };
            record.validate().ok()?;
            Some(record)
        }
        parse(bytes).ok_or(RelationDecodeError::Malformed)
    }
}
/// Validates canonical ownership, ordering, and exact encoded work before staging.
pub fn validate_native_source_evidence_rows(
    rows: &[([u8; 32], NativeSourceEvidence)],
) -> Result<(), &'static str> {
    if rows.len() > MAX_NATIVE_EVIDENCE_ROWS {
        return Err("native source evidence row limit");
    }
    let mut bytes = 0usize;
    let mut previous = None;
    let mut projects = BTreeMap::<[u8; 32], usize>::new();
    for (key, row) in rows {
        if previous.is_some_and(|prior| prior >= *key) || row.key()? != *key {
            return Err("noncanonical native source evidence key");
        }
        previous = Some(*key);
        let count = projects.entry(row.project).or_default();
        *count = count
            .checked_add(1)
            .ok_or("native source evidence count overflow")?;
        if *count > MAX_NATIVE_EVIDENCE_PROJECT_ROWS {
            return Err("native source evidence project limit");
        }
        // One 32-byte key plus two u32 lengths frames each exact record.
        bytes = bytes
            .checked_add(32 + 8)
            .and_then(|n| n.checked_add(row.checked_encoded_len()?))
            .ok_or("native source evidence byte overflow")?;
        if bytes > MAX_NATIVE_EVIDENCE_BYTES {
            return Err("native source evidence byte limit");
        }
    }
    Ok(())
}
/// Canonical pointer key for selected evidence.
pub fn native_source_evidence_root_pointer_key() -> [u8; 32] {
    *ObjectKey::<NativeSourceEvidenceRootSchema>::from_value(&ROOT_KEY).as_bytes()
}
/// Makes a pointer; authority is established by the selected workspace closure.
pub fn native_source_evidence_root_object(
    root: StateRoot<NativeSourceEvidenceRelation>,
) -> TypedObject {
    let key = ObjectKey::<NativeSourceEvidenceRootSchema>::from_value(&ROOT_KEY);
    TypedObject::from_value(&key, root.as_bytes())
}
/// Opens selected evidence; absent root remains unknown.
pub fn native_source_evidence_relation(
    snapshot: &WorkspaceSnapshot,
) -> Result<Option<WorkspaceRelationHandle<NativeSourceEvidenceRelation>>, WorkspaceRelationError> {
    let key = ObjectKey::<NativeSourceEvidenceRootSchema>::from_value(&ROOT_KEY);
    snapshot.auxiliary_relation::<NativeSourceEvidenceRelation>(
        SchemaIdentity::new(
            NativeSourceEvidenceRootSchema::DOMAIN,
            NativeSourceEvidenceRootSchema::TYPE,
            NativeSourceEvidenceRootSchema::VERSION,
        ),
        *key.as_bytes(),
    )
}

/// Scratch reserved before native relation admission and every bounded read.
/// Canonical nodes have a 64 KiB wire ceiling and 1024 entries. This covers
/// 32 simultaneously retained traversal levels, their node copies and branch
/// vectors, a whole decoded leaf (including allocator overhead), and a cloned
/// result. Bounded reads reject deeper paths; the store enforces the node limit
/// before allocating its envelope.
pub const NATIVE_EVIDENCE_READ_SCRATCH: usize = 32 * 1024 * 1024;
const NATIVE_EVIDENCE_READ_DEPTH: usize = 32;
const NATIVE_EVIDENCE_ROOT_RESIDENT: usize = 2 * 64 * 1024 + 1024;

fn native_read_reserve(budget: &mut usize) -> Result<(), WorkspaceRelationError> {
    *budget = budget.checked_sub(NATIVE_EVIDENCE_READ_SCRATCH)
        .ok_or(WorkspaceRelationError::Store(backend_store::StoreError::Bounds))?;
    Ok(())
}
fn native_read_refund(budget: &mut usize, retained: usize) -> Result<(), WorkspaceRelationError> {
    let refund = NATIVE_EVIDENCE_READ_SCRATCH.checked_sub(retained)
        .ok_or(WorkspaceRelationError::Store(backend_store::StoreError::Bounds))?;
    *budget = budget.checked_add(refund).ok_or(WorkspaceRelationError::Store(backend_store::StoreError::Bounds))?;
    Ok(())
}
fn native_row_resident(row: &NativeSourceEvidence) -> Result<usize, WorkspaceRelationError> {
    row.checked_heap_bytes().and_then(|n| n.checked_add(std::mem::size_of::<NativeSourceEvidence>() + 128))
        .ok_or(WorkspaceRelationError::Store(backend_store::StoreError::Bounds))
}

/// Opens a selected relation only after reserving node-admission scratch.
pub fn native_source_evidence_relation_budgeted(
    snapshot: &WorkspaceSnapshot, budget: &mut usize,
) -> Result<Option<WorkspaceRelationHandle<NativeSourceEvidenceRelation>>, WorkspaceRelationError> {
    native_read_reserve(budget)?;
    let result = native_source_evidence_relation(snapshot)?;
    native_read_refund(budget, if result.is_some() { NATIVE_EVIDENCE_ROOT_RESIDENT } else { 0 })?;
    Ok(result)
}

/// Reads the selected root object under the same node scratch reservation.
pub fn native_source_evidence_root_object_budgeted(
    relation: &WorkspaceRelationHandle<NativeSourceEvidenceRelation>, budget: &mut usize,
) -> Result<TypedObject, WorkspaceRelationError> {
    native_read_reserve(budget)?;
    let object = relation.root_object()?;
    native_read_refund(budget, NATIVE_EVIDENCE_ROOT_RESIDENT)?;
    Ok(object)
}

/// Looks up a row after reserving scratch for every sibling in its node.
/// The retained row remains charged to the same budget on return.
pub fn native_source_evidence_lookup_budgeted(
    relation: &WorkspaceRelationHandle<NativeSourceEvidenceRelation>, key: &[u8; 32], budget: &mut usize,
) -> Result<Option<NativeSourceEvidence>, WorkspaceRelationError> {
    native_read_reserve(budget)?;
    let row = relation.lookup(key)?;
    let retained = row.as_ref().map(native_row_resident).transpose()?.unwrap_or(0);
    native_read_refund(budget, retained)?;
    Ok(row)
}

/// Reads at most one row with a depth limit and charges its retained allocation.
pub fn native_source_evidence_page_budgeted(
    relation: &WorkspaceRelationHandle<NativeSourceEvidenceRelation>, after: Option<&[u8; 32]>, budget: &mut usize,
) -> Result<backend_version::LazyTreePage<NativeSourceEvidenceRelation>, WorkspaceRelationError> {
    native_read_reserve(budget)?;
    let page = relation.page_one_with_depth_limit(after, NATIVE_EVIDENCE_READ_DEPTH)?;
    let retained = match page.entries().first() {
        Some((_, row)) => native_row_resident(row)?.checked_add(128).ok_or(WorkspaceRelationError::Store(backend_store::StoreError::Bounds))?,
        None => 0,
    };
    native_read_refund(budget, retained)?;
    Ok(page)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{GoVersion, LanguageProfile};

    fn row() -> NativeSourceEvidence {
        NativeSourceEvidence {
            project: [7; 32],
            profile: LanguageProfile::Go(GoVersion::Go125),
            path: "pkg/π.go".into(),
            input_digest: [8; 32],
            source_digest: [9; 32],
            byte_length: 42,
            kind: NativeSourceEvidenceKind::Image,
            line_starts: None,
            headers: None,
        }
    }

    #[test]
    fn budgeted_typed_relation_reads_charge_before_decoding_siblings() {
        use backend_store::{FileStore, RelationAdmissionRegistry};
        use backend_version::{CoverageWitness, RelationState, ScopeRoot, UntrustedCoverageScope};
        use std::sync::Arc;
        let directory = tempfile::tempdir().unwrap();
        let registry = RelationAdmissionRegistry::new().with_relation::<NativeSourceEvidenceRelation>().unwrap();
        let store = FileStore::open_with_registry(directory.path(), 1024 * 1024, registry).unwrap();
        let mut rows = Vec::new();
        for i in 0..200 {
            let mut item = row();
            item.path = format!("pkg/{i:03}.go");
            item.byte_length = 2048;
            item.line_starts = Some((0..1024).map(|n| n * 2).collect());
            rows.push((item.key().unwrap(), item));
        }
        rows.sort_unstable_by_key(|(key, _)| *key);
        let state = RelationState::<NativeSourceEvidenceRelation>::from_entries(rows.clone(),
            CoverageWitness::Partial(UntrustedCoverageScope::from_scope_root(ScopeRoot::from_u64(1)))).unwrap();
        store.write_relation_state(&state).unwrap();
        let relation = WorkspaceRelationHandle::<NativeSourceEvidenceRelation>::open(Arc::new(store), *state.root().as_bytes()).unwrap();
        let mut short = NATIVE_EVIDENCE_READ_SCRATCH - 1;
        assert!(native_source_evidence_lookup_budgeted(&relation, &rows[100].0, &mut short).is_err());
        assert_eq!(short, NATIVE_EVIDENCE_READ_SCRATCH - 1);
        let mut budget = 64 * 1024 * 1024;
        let found = native_source_evidence_lookup_budgeted(&relation, &rows[100].0, &mut budget).unwrap().unwrap();
        assert_eq!(found, rows[100].1);
        assert!(budget < 64 * 1024 * 1024);
        let mut cursor = None;
        let mut visited = 0;
        loop {
            let page = native_source_evidence_page_budgeted(&relation, cursor.as_ref(), &mut budget).unwrap();
            if page.entries().is_empty() { break; }
            assert_eq!(page.entries()[0], rows[visited]);
            visited += 1;
            cursor = page.next().copied();
            if cursor.is_none() { break; }
        }
        assert_eq!(visited, rows.len());
    }

    #[test]
    fn claims_roundtrip_and_reject_noncanonical_input() {
        let row = row();
        let mut bytes = Vec::new();
        NativeSourceEvidenceRelation::encode_value(&row, &mut bytes);
        assert_eq!(row.checked_encoded_len(), Some(bytes.len()));
        assert_eq!(
            NativeSourceEvidenceRelation::decode_value(&bytes).unwrap(),
            row
        );
        let last = bytes.len() - 1;
        bytes[last] = 1; // Unknown inventory is the only represented status.
        assert!(NativeSourceEvidenceRelation::decode_value(&bytes).is_err());
        bytes[last] = 0;
        bytes.push(0);
        assert!(NativeSourceEvidenceRelation::decode_value(&bytes).is_err());
        for path in ["", "../x.go", "/x.go", "a//x.go", "C:/x.go", "a\\x.go"] {
            let mut invalid = row.clone();
            invalid.path = path.into();
            assert!(invalid.key().is_err());
        }
        let mut too_large = row;
        too_large.byte_length = MAX_NATIVE_EVIDENCE_SOURCE + 1;
        assert!(too_large.key().is_err());
    }

    #[test]
    fn line_map_roundtrips_and_is_bound_to_verified_bytes() {
        let bytes = "α\r\nsecond\n".as_bytes();
        let image = backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(bytes);
        let mut claim = row();
        claim.byte_length = bytes.len() as u32;
        claim.input_digest = backend_compile::typed_of::<backend_compile::InputContentSchema>(bytes).to_bytes();
        claim.source_digest.copy_from_slice(image.as_ref());
        claim.line_starts = NativeSourceEvidence::capture_line_starts(bytes);
        assert_eq!(claim.line_at(0), Some(1));
        assert_eq!(claim.line_at(4), Some(2));
        assert_eq!(claim.line_at(bytes.len() as u32), Some(3));
        let mut encoded = Vec::new();
        NativeSourceEvidenceRelation::encode_value(&claim, &mut encoded);
        assert_eq!(&encoded[..4], MAGIC_V2);
        assert_eq!(claim.checked_encoded_len(), Some(encoded.len()));
        assert_eq!(NativeSourceEvidenceRelation::decode_value(&encoded).unwrap(), claim);
        assert!(claim.verify_host(bytes, image).is_ok());
        claim.line_starts.as_mut().unwrap()[1] += 1;
        assert!(claim.verify_host(bytes, image).is_err());
        assert!(NativeSourceEvidence::capture_line_starts(b"a\rb").is_none());
        let long = vec![b'\n'; 20_000];
        assert_eq!(NativeSourceEvidence::capture_line_starts(&long).unwrap().len(), 20_001);
        assert!(NativeSourceEvidence::capture_line_starts(&vec![b'\n'; MAX_NATIVE_EVIDENCE_LINES]).is_none());
        let mut invalid = claim.clone();
        invalid.line_starts = Some(vec![0, 0]);
        assert!(invalid.validate().is_err());
        assert!(invalid.key().is_err());
        let mut malformed = Vec::new();
        NativeSourceEvidenceRelation::encode_value(&invalid, &mut malformed);
        assert!(NativeSourceEvidenceRelation::decode_value(&malformed).is_err());
        assert!(NativeSourceEvidence::capture_line_starts("a\u{2028}b".as_bytes()).is_none());
        claim.kind = NativeSourceEvidenceKind::ProgramMember;
        assert!(!claim.admits_image());
        assert!(claim.verify_host(bytes, image).is_err());
    }

    #[test]
    fn nse3_headers_are_bounded_untrusted_claims() {
        let mut claim = row();
        claim.headers = Some(NativeHeaderEvidence {
            image_identity: [3; 32], image_byte_len: 100, producer_recipe: [4; 32],
            entries: vec![NativeHeaderEntry { entity_id: 7,
                identity: DeclarationIdentity { family: DeclarationFamilyId::from_raw([5; 16]), variant: VariantFingerprint::from_raw([6; 16]) },
                compiler_extent: (1, 40), header_extent: (1, 12) }],
        });
        let mut bytes = Vec::new();
        NativeSourceEvidenceRelation::encode_value(&claim, &mut bytes);
        assert_eq!(&bytes[..4], MAGIC_V3);
        assert_eq!(claim.checked_encoded_len(), Some(bytes.len()));
        assert!(claim.checked_heap_bytes().unwrap() >= claim.path.len() + std::mem::size_of::<NativeHeaderEntry>());
        assert_eq!(NativeSourceEvidenceRelation::decode_value(&bytes).unwrap(), claim);
        for end in 0..bytes.len() { assert!(NativeSourceEvidenceRelation::decode_value(&bytes[..end]).is_err()); }
        let mut invalid = claim.clone();
        let duplicate = invalid.headers.as_ref().unwrap().entries[0].clone();
        invalid.headers.as_mut().unwrap().entries.push(duplicate);
        assert!(invalid.validate().is_err());
        let mut invalid = claim.clone();
        invalid.headers.as_mut().unwrap().entries[0].header_extent = (2, 12);
        assert!(invalid.validate().is_err());
        invalid.kind = NativeSourceEvidenceKind::ProgramMember;
        assert!(invalid.validate().is_err());
        let count_offset = bytes.len() - HEADER_ENTRY_BYTES - 4;
        bytes[count_offset..count_offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(NativeSourceEvidenceRelation::decode_value(&bytes).is_err());
    }

    #[test]
    fn host_verification_checks_both_domains_and_length() {
        let source = "π emoji 😀".as_bytes();
        let image =
            backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                source,
            );
        let mut claim = row();
        claim.byte_length = source.len() as u32;
        claim.input_digest =
            backend_compile::typed_of::<backend_compile::InputContentSchema>(source).to_bytes();
        claim.source_digest.copy_from_slice(image.as_ref());
        assert!(claim.verify_host(source, image).is_ok());
        assert!(claim.verify_host("λ emoji 😀".as_bytes(), image).is_err());
        let mut wrong = claim.clone();
        wrong.input_digest[0] ^= 1;
        assert!(wrong.verify_host(source, image).is_err());
        wrong = claim.clone();
        wrong.source_digest[0] ^= 1;
        assert!(wrong.verify_host(source, image).is_err());
        wrong = claim;
        wrong.byte_length -= 1;
        assert!(wrong.verify_host(source, image).is_err());
    }

    #[test]
    fn aggregate_and_per_project_budgets_are_enforced() {
        let mut rows = Vec::with_capacity(MAX_NATIVE_EVIDENCE_PROJECT_ROWS + 1);
        for i in 0..=MAX_NATIVE_EVIDENCE_PROJECT_ROWS {
            let mut item = row();
            item.path = format!("p/{i}.go");
            rows.push((item.key().unwrap(), item));
        }
        rows.sort_unstable_by_key(|entry| entry.0);
        assert_eq!(
            validate_native_source_evidence_rows(&rows),
            Err("native source evidence project limit")
        );
        drop(rows);
        let mut rows = Vec::new();
        for i in 0..17_000 {
            let mut item = row();
            item.path = format!("{i:05}{}", "x".repeat(MAX_NATIVE_EVIDENCE_PATH - 5));
            rows.push((item.key().unwrap(), item));
        }
        rows.sort_unstable_by_key(|entry| entry.0);
        assert_eq!(
            validate_native_source_evidence_rows(&rows),
            Err("native source evidence byte limit")
        );
    }

    #[test]
    fn staging_requires_matching_unique_ordered_keys() {
        let first = row();
        let key = first.key().unwrap();
        assert_eq!(
            native_source_evidence_key(first.project, first.profile, &first.path),
            Ok(key)
        );
        assert!(validate_native_source_evidence_rows(&[(key, first.clone())]).is_ok());
        assert!(validate_native_source_evidence_rows(&[([0; 32], first.clone())]).is_err());
        assert!(
            validate_native_source_evidence_rows(&[(key, first.clone()), (key, first)]).is_err()
        );
    }
}
