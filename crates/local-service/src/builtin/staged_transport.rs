//! Small queue transport over exact typed CAS evidence.

use super::profile::{BuiltinIntent, BuiltinIntentSchema, BuiltinModel, BuiltinModelError};
use super::staged_intent::{self, EvidenceKind, StageAdmission, StageBasis, StagePool};
use backend_engine::{WorkspaceModel, WorkspaceSnapshot};
use backend_store::{
    ArtifactBudget, ArtifactClosureClaim, ClosureCompositionBudget, DurableClosureManifest,
    FileStore, ObjectId, StoreError, UntrustedObjectId,
};
use backend_version::{ObjectVersion, ObjectVersionHasher, Schema, SchemaIdentity};
use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::Path,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

const QUEUE_STAGE_THRESHOLD: usize = 1024 * 1024;
const POINTER_MAGIC: &[u8; 4] = b"BPS1";

pub(super) struct StagedIntent {
    pub basis: StageBasis,
    pub manifest: ObjectId,
    pub membership: DurableClosureManifest,
    store: FileStore,
    cancelled: Arc<AtomicBool>,
    _admission: Option<Arc<StageAdmission>>,
}

impl std::fmt::Debug for StagedIntent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StagedIntent")
            .field("basis", &self.basis)
            .field("manifest", &self.manifest)
            .finish_non_exhaustive()
    }
}
impl PartialEq for StagedIntent {
    fn eq(&self, other: &Self) -> bool {
        self.basis == other.basis && self.manifest == other.manifest
    }
}
impl Eq for StagedIntent {}

fn error(context: &str, error: impl std::fmt::Debug) -> BuiltinModelError {
    BuiltinModelError(format!("{context}: {error:?}"))
}

fn configured_limits() -> Result<(u64, usize), StoreError> {
    let source = super::source_budget::SourceAdmissionPolicy::from_environment()
        .map_err(StoreError::Io)?
        .limits();
    let bytes = source
        .max_project_source_bytes
        .checked_add(source.max_project_record_bytes)
        .and_then(|n| n.checked_add(128 * 64 * 1024))
        .ok_or(StoreError::Bounds)?;
    let pages = bytes
        .div_ceil(64 * 1024)
        .checked_add(3)
        .ok_or(StoreError::Bounds)?;
    Ok((u64::try_from(bytes).map_err(|_| StoreError::Bounds)?, pages))
}

fn budgets(
    pages: usize,
    payload: u64,
) -> Result<(ArtifactBudget, ClosureCompositionBudget), StoreError> {
    let members = pages.checked_add(128).ok_or(StoreError::Bounds)?;
    let changes = pages.checked_add(256).ok_or(StoreError::Bounds)?;
    let verified = payload
        .checked_mul(2)
        .and_then(|n| n.checked_add(128 * 64 * 1024))
        .and_then(|n| n.checked_add((pages as u64).checked_mul(44)?))
        .ok_or(StoreError::Bounds)?;
    Ok((
        ArtifactBudget::new(changes, members, verified, 64 * 1024 + 44, changes),
        ClosureCompositionBudget::new(
            members,
            changes,
            verified,
            ClosureCompositionBudget::metadata_bytes_for(changes)?,
        ),
    ))
}

fn pool() -> Result<&'static StagePool, StoreError> {
    static POOL: OnceLock<StagePool> = OnceLock::new();
    let (bytes, pages) = configured_limits()?;
    let metadata = ClosureCompositionBudget::metadata_bytes_for(pages + 256)?
        .checked_add(64 * 1024 * 8 + pages * size_of::<ObjectId>() * 4)
        .ok_or(StoreError::Bounds)?;
    let total = bytes.checked_mul(4).ok_or(StoreError::Bounds)?;
    Ok(POOL.get_or_init(|| StagePool::new(4, total, pages, metadata)))
}

fn basis(snapshot: &WorkspaceSnapshot, request: [u8; 32]) -> StageBasis {
    StageBasis {
        owner_epoch: snapshot.owner_epoch(),
        workspace_sequence: snapshot.sequence(),
        owner_fence: *snapshot.closure().binding().proof(),
        workspace_root: *snapshot.root().as_bytes(),
        closure_id: *snapshot.closure().membership_id().as_bytes(),
        source_capture: request,
    }
}

fn cancelled(flag: &AtomicBool) -> Result<(), StoreError> {
    if flag.load(Ordering::Acquire) {
        Err(StoreError::Io(
            "index scan cancelled during staged admission".to_owned(),
        ))
    } else {
        Ok(())
    }
}

struct AdmissionReader<'a> {
    bytes: &'a [u8],
    cancellation: &'a AtomicBool,
}

impl Read for AdmissionReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if self.cancellation.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "index scan cancelled during staged admission",
            ));
        }
        self.bytes.read(output)
    }
}

struct RawSourceAdmission {
    header: Vec<u8>,
    active: Option<(String, [u8; 32], u64, ObjectVersionHasher)>,
    records: BTreeMap<String, [u8; 32]>,
    max_bytes: u64,
}

impl RawSourceAdmission {
    fn new(max_bytes: u64) -> Self {
        Self {
            header: Vec::new(),
            active: None,
            records: BTreeMap::new(),
            max_bytes,
        }
    }
    fn append(&mut self, mut bytes: &[u8]) -> Result<(), StoreError> {
        while !bytes.is_empty() {
            if let Some((_, _, remaining, hasher)) = &mut self.active {
                let count = bytes
                    .len()
                    .min(usize::try_from(*remaining).map_err(|_| StoreError::Bounds)?);
                hasher
                    .update(&bytes[..count])
                    .map_err(|_| StoreError::Corrupt)?;
                *remaining -= count as u64;
                bytes = &bytes[count..];
                if *remaining == 0 {
                    let (path, expected, _, hasher) =
                        self.active.take().ok_or(StoreError::Corrupt)?;
                    if hasher.finish().map_err(|_| StoreError::Corrupt)? != expected
                        || self.records.insert(path, expected).is_some()
                    {
                        return Err(StoreError::Corrupt);
                    }
                }
                continue;
            }
            let required = if self.header.len() < 4 {
                4
            } else {
                let length = u32::from_be_bytes(
                    self.header[..4]
                        .try_into()
                        .map_err(|_| StoreError::Corrupt)?,
                ) as usize;
                if length == 0 || length > 4096 {
                    return Err(StoreError::Bounds);
                }
                4 + length + 8 + 32
            };
            let count = bytes.len().min(
                required
                    .checked_sub(self.header.len())
                    .ok_or(StoreError::Corrupt)?,
            );
            self.header.extend_from_slice(&bytes[..count]);
            bytes = &bytes[count..];
            if self.header.len() < required || required == 4 {
                continue;
            }
            let path_length = required - 44;
            let path = std::str::from_utf8(&self.header[4..4 + path_length])
                .map_err(|_| StoreError::Corrupt)?
                .to_owned();
            let length = u64::from_be_bytes(
                self.header[4 + path_length..12 + path_length]
                    .try_into()
                    .map_err(|_| StoreError::Corrupt)?,
            );
            if length > self.max_bytes {
                return Err(StoreError::Bounds);
            }
            let expected = self.header[12 + path_length..]
                .try_into()
                .map_err(|_| StoreError::Corrupt)?;
            let schema = SchemaIdentity::new(
                backend_compile::InputContentSchema::DOMAIN,
                backend_compile::InputContentSchema::TYPE,
                backend_compile::InputContentSchema::VERSION,
            );
            let mut hasher = ObjectVersionHasher::new(
                schema,
                usize::try_from(length.checked_add(8).ok_or(StoreError::Bounds)?)
                    .map_err(|_| StoreError::Bounds)?,
            )
            .map_err(|_| StoreError::Bounds)?;
            hasher
                .update(&length.to_be_bytes())
                .map_err(|_| StoreError::Corrupt)?;
            self.header.clear();
            if length == 0 {
                if hasher.finish().map_err(|_| StoreError::Corrupt)? != expected
                    || self.records.insert(path, expected).is_some()
                {
                    return Err(StoreError::Corrupt);
                }
            } else {
                self.active = Some((path, expected, length, hasher));
            }
        }
        Ok(())
    }

    fn admit(self, intent: &BuiltinIntent) -> Result<(), StoreError> {
        if !self.header.is_empty() || self.active.is_some() {
            return Err(StoreError::Corrupt);
        }
        if self.records.is_empty() {
            return Ok(());
        }
        let expected = intent
            .changes()
            .iter()
            .filter_map(|change| {
                change
                    .after
                    .as_ref()
                    .and_then(|record| record.file_fields())
            })
            .filter(|file| file.content_version != [0; 32])
            .map(|file| (file.path.to_owned(), file.content_version))
            .collect::<BTreeMap<_, _>>();
        if self.records != expected {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
}

pub(super) fn stage(
    mut intent: BuiltinIntent,
    snapshot: &WorkspaceSnapshot,
    source_root: Option<&Path>,
    cancellation: Arc<AtomicBool>,
) -> Result<BuiltinIntent, BuiltinModelError> {
    if intent.staged().is_some() {
        return Ok(intent);
    }
    let (bytes, source_end) = intent.staging_parts();
    if bytes.len() <= QUEUE_STAGE_THRESHOLD && snapshot.closure().stored_membership().is_none() {
        return Ok(intent);
    }
    let (maximum_bytes, maximum_pages) =
        configured_limits().map_err(|e| error("stage source admission policy", e))?;
    let (_, composition_budget) =
        budgets(maximum_pages, maximum_bytes).map_err(|e| error("stage budgets", e))?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(error("stage intent byte budget", StoreError::Bounds));
    }
    let store = snapshot.durable_store().ok_or_else(|| {
        BuiltinModelError("staged intent requires the snapshot's durable CAS".to_owned())
    })?;
    let request = ObjectVersion::<BuiltinIntentSchema>::from_value(&bytes).to_bytes();
    let bound = basis(snapshot, request);
    let mut writer = pool()
        .map_err(|e| error("stage pool", e))?
        .begin(store, bound, maximum_bytes)
        .map_err(|e| error("admit staged intent", e))?;
    cancelled(&cancellation).map_err(|e| error("stage cancelled", e))?;
    writer
        .append(
            EvidenceKind::SourceRows,
            &mut AdmissionReader {
                bytes: &bytes[..source_end],
                cancellation: &cancellation,
            },
        )
        .map_err(|e| error("stage source rows", e))?;
    writer
        .append(
            EvidenceKind::CompleteFacts,
            &mut AdmissionReader {
                bytes: &bytes[source_end..],
                cancellation: &cancellation,
            },
        )
        .map_err(|e| error("stage complete facts", e))?;
    if let Some(root) = source_root {
        for change in intent.changes() {
            let Some(file) = change
                .after
                .as_ref()
                .and_then(|record| record.file_fields())
            else {
                continue;
            };
            if file.content_version == [0; 32] {
                continue;
            }
            cancelled(&cancellation).map_err(|e| error("stage source cancelled", e))?;
            let path = Path::new(file.path);
            if path.is_absolute()
                || path
                    .components()
                    .any(|part| !matches!(part, std::path::Component::Normal(_)))
            {
                return Err(BuiltinModelError(
                    "staged source path escaped captured root".to_owned(),
                ));
            }
            let mut input =
                File::open(root.join(path)).map_err(|e| error("open captured staged source", e))?;
            let before = input
                .metadata()
                .map_err(|e| error("stat captured staged source", e))?;
            let length = usize::try_from(before.len()).map_err(|e| error("source length", e))?;
            if length as u64 > maximum_bytes {
                return Err(error("stage source byte budget", StoreError::Bounds));
            }
            let mut hasher = ObjectVersionHasher::new(
                SchemaIdentity::new(
                    backend_compile::InputContentSchema::DOMAIN,
                    backend_compile::InputContentSchema::TYPE,
                    backend_compile::InputContentSchema::VERSION,
                ),
                length
                    .checked_add(8)
                    .ok_or_else(|| error("source length", StoreError::Bounds))?,
            )
            .map_err(|e| error("source identity", e))?;
            hasher
                .update(&(length as u64).to_be_bytes())
                .map_err(|e| error("source identity", e))?;
            let mut header = Vec::new();
            header.extend_from_slice(&(file.path.len() as u32).to_be_bytes());
            header.extend_from_slice(file.path.as_bytes());
            header.extend_from_slice(&(length as u64).to_be_bytes());
            header.extend_from_slice(&file.content_version);
            writer
                .append(EvidenceKind::RawSource, &mut header.as_slice())
                .map_err(|e| error("stage source header", e))?;
            let mut chunk = [0_u8; 64 * 1024];
            loop {
                cancelled(&cancellation).map_err(|e| error("stage source cancelled", e))?;
                let count = input
                    .read(&mut chunk)
                    .map_err(|e| error("read captured staged source", e))?;
                if count == 0 {
                    break;
                }
                hasher
                    .update(&chunk[..count])
                    .map_err(|e| error("source changed while staging", e))?;
                writer
                    .append(EvidenceKind::RawSource, &mut &chunk[..count])
                    .map_err(|e| error("stage captured source", e))?;
            }
            let after = input
                .metadata()
                .map_err(|e| error("stat staged source", e))?;
            if before.len() != after.len()
                || before.modified().ok() != after.modified().ok()
                || hasher
                    .finish()
                    .map_err(|e| error("source changed while staging", e))?
                    != file.content_version
            {
                return Err(BuiltinModelError(
                    "captured source changed during staging; retry indexing".to_owned(),
                ));
            }
        }
    }
    cancelled(&cancellation).map_err(|e| error("stage cancelled", e))?;
    let (manifest, pin, admission) = writer
        .finish()
        .map_err(|e| error("finish staged evidence", e))?
        .into_transport();
    let membership = DurableClosureManifest::from_pinned(store, pin, composition_budget)
        .map_err(|e| error("admit staged membership", e))?
        .with_cancellation(Arc::clone(&cancellation));
    let staged = Arc::new(StagedIntent {
        basis: bound,
        manifest,
        membership,
        store: store.clone(),
        cancelled: cancellation,
        _admission: Some(Arc::new(admission)),
    });
    intent.set_staged(staged, true);
    Ok(intent)
}

impl StagedIntent {
    pub fn validate_control_union(
        &self,
        controls: &backend_store::ClosureManifest,
    ) -> Result<(), BuiltinModelError> {
        self.validate_membership_union(&self.membership, controls)
    }

    pub fn validate_membership_union(
        &self,
        membership: &DurableClosureManifest,
        controls: &backend_store::ClosureManifest,
    ) -> Result<(), BuiltinModelError> {
        let manifest = self
            .store
            .read_object(self.manifest)
            .map_err(|e| error("read exact staged manifest", e))?;
        let mut count = staged_intent::staged_member_count(&manifest)
            .map_err(|e| error("stage member count", e))?;
        for object in controls.objects() {
            if object.id() == self.manifest {
                continue;
            }
            if object.schema().domain() == 0x96 && matches!(object.schema().ty(), 10..=13) {
                return Err(BuiltinModelError(
                    "staged page entered typed control frontier".to_owned(),
                ));
            }
            count = count
                .checked_add(1)
                .ok_or_else(|| error("staged union count", StoreError::Bounds))?;
            if !membership
                .contains(object.id())
                .map_err(|e| error("control membership", e))?
            {
                return Err(error(
                    "control object missing from staged union",
                    StoreError::Corrupt,
                ));
            }
        }
        if membership.object_count() != count {
            return Err(BuiltinModelError(
                "selected membership is not exact union of controls and staged evidence".to_owned(),
            ));
        }
        Ok(())
    }
    pub fn decode(
        bytes: &[u8],
        store: &FileStore,
        closure: backend_store::ClosureId,
    ) -> Result<Arc<Self>, BuiltinModelError> {
        if bytes.len() != 180 || &bytes[..4] != POINTER_MAGIC {
            return Err(BuiltinModelError(
                "malformed staged intent pointer".to_owned(),
            ));
        }
        let manifest = store
            .read_object_claim(UntrustedObjectId::from_bytes(
                bytes[4..36]
                    .try_into()
                    .map_err(|e| error("stage manifest ID", e))?,
            ))
            .map_err(|e| error("read stage manifest", e))?;
        let basis = staged_intent::admit_manifest_object(&manifest)
            .map_err(|e| error("admit stage manifest", e))?;
        let (pages, payload) = staged_intent::staged_scope_limits(&manifest)
            .map_err(|e| error("staged replay scope", e))?;
        let index = store
            .open_closure(closure)
            .map_err(|e| error("selected stage index", e))?;
        if index.object_count() < pages as u64 + 1 || index.object_count() > pages as u64 + 128 {
            return Err(error("staged replay union bound", StoreError::Corrupt));
        }
        let (_, composition_budget) =
            budgets(pages, payload).map_err(|e| error("staged replay budgets", e))?;
        let pin = store
            .reopen_pinned_workspace_closure(
                ArtifactClosureClaim::from_id(closure),
                composition_budget,
            )
            .map_err(|e| error("reopen selected staged membership", e))?;
        let membership = DurableClosureManifest::from_pinned(store, pin, composition_budget)
            .map_err(|e| error("admit selected staged membership", e))?;
        if !membership
            .contains(manifest.id())
            .map_err(|e| error("prove stage manifest membership", e))?
        {
            return Err(BuiltinModelError(
                "stage manifest is outside selected membership".to_owned(),
            ));
        }
        let staged = Arc::new(Self {
            basis,
            manifest: manifest.id(),
            membership,
            store: store.clone(),
            cancelled: Arc::new(AtomicBool::new(false)),
            _admission: None,
        });
        if staged.encode() != bytes {
            return Err(BuiltinModelError(
                "staged pointer substituted its owner/workspace/command fence".to_owned(),
            ));
        }
        Ok(staged)
    }
    pub fn request(&self) -> [u8; 32] {
        self.basis.source_capture
    }
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::from(&POINTER_MAGIC[..]);
        bytes.extend_from_slice(self.manifest.as_bytes());
        bytes.extend_from_slice(&self.basis.owner_epoch.to_be_bytes());
        bytes.extend_from_slice(&self.basis.workspace_sequence.to_be_bytes());
        for id in [
            self.basis.owner_fence,
            self.basis.workspace_root,
            self.basis.closure_id,
            self.basis.source_capture,
        ] {
            bytes.extend_from_slice(&id);
        }
        bytes
    }
    pub fn hydrate(
        &self,
        expected: Option<&WorkspaceSnapshot>,
    ) -> Result<BuiltinIntent, BuiltinModelError> {
        if expected.is_some_and(|snapshot| basis(snapshot, self.request()) != self.basis) {
            return Err(error("staged intent fence changed", StoreError::WrongBase));
        }
        let mut bytes = Vec::new();
        let manifest = self
            .store
            .read_object(self.manifest)
            .map_err(|e| error("staged replay manifest", e))?;
        let (pages, payload_bytes) = staged_intent::staged_scope_limits(&manifest)
            .map_err(|e| error("staged replay scope", e))?;
        let (artifact_budget, _) =
            budgets(pages, payload_bytes).map_err(|e| error("staged replay budgets", e))?;
        let mut raw = RawSourceAdmission::new(payload_bytes);
        staged_intent::visit_staged_scope(
            &self.store,
            self.manifest,
            self.membership.id(),
            self.basis,
            artifact_budget,
            None,
            |kind, payload| {
                cancelled(&self.cancelled)?;
                if kind == EvidenceKind::RawSource {
                    raw.append(payload)?;
                } else {
                    let length = bytes
                        .len()
                        .checked_add(payload.len())
                        .ok_or(StoreError::Bounds)?;
                    if length as u64 > payload_bytes {
                        return Err(StoreError::Bounds);
                    }
                    bytes
                        .try_reserve(payload.len())
                        .map_err(|_| StoreError::Bounds)?;
                    bytes.extend_from_slice(payload);
                }
                Ok(())
            },
        )
        .map_err(|e| error("hydrate exact staged intent", e))?;
        let intent = BuiltinIntent::decode(&bytes)?;
        raw.admit(&intent)
            .map_err(|e| error("admit exact staged source bytes", e))?;
        if BuiltinModel.request_id(&intent) != self.request() || intent.encode() != bytes {
            return Err(BuiltinModelError(
                "staged source/facts bytes do not match immutable request identity".to_owned(),
            ));
        }
        Ok(intent)
    }
}
