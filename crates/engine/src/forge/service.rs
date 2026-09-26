use super::*;

mod acquire;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgeJournalRecord {
    coordinate: ForgeCoordinate,
    resolution: ForgeResolution,
    archive: [u8; ID_BYTES],
    tree: Vec<ForgeJournalEntry>,
    metadata: ForgeRepositoryMetadata,
    manifests: Vec<ForgePackageManifest>,
    source: [u8; ID_BYTES],
    cursor: [u8; ID_BYTES],
    facts_frontier: [u8; ID_BYTES],
    base_snapshot: [u8; ID_BYTES],
    target_snapshot: [u8; ID_BYTES],
    delta: [u8; ID_BYTES],
    observed_at_millis: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ForgeJournalEntry {
    path: Arc<str>,
    object: [u8; ID_BYTES],
    mode: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "kebab-case")]
enum ForgeJournalEvent {
    /// A durable source snapshot and its canonical acquisition delta.
    Published(ForgeJournalRecord),
    /// A durable negative observation. It remains in the append-only history
    /// until a later publication supersedes it.
    Tombstone {
        coordinate: ForgeCoordinate,
        reason: ForgeRejectReason,
    },
}

struct ForgeLog;

impl JournalDomain for ForgeLog {
    const DOMAIN: u8 = 0x92;
    const TYPE: u16 = 1;
    const VERSION: u8 = 1;
}

impl JournalCodec for ForgeLog {
    type Record = ForgeJournalEvent;

    fn encode(record: &Self::Record, output: &mut Vec<u8>) {
        // serde_json emits fields in declaration order, which is the
        // canonical payload grammar for this private typed journal.
        if let Ok(bytes) = serde_json::to_vec(record) {
            output.extend_from_slice(&bytes);
        }
    }

    fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
        serde_json::from_slice(bytes).map_err(|_| JournalError::Corrupt("forge record"))
    }
}

impl ForgeJournalEvent {
    fn coordinate(&self) -> &ForgeCoordinate {
        match self {
            Self::Published(record) => &record.coordinate,
            Self::Tombstone { coordinate, .. } => coordinate,
        }
    }
}

/// Durable source acquisition owner for all forge providers.
pub struct ForgeAcquisitionService {
    store: ContentAddressedStore,
    journal: Arc<HashChainJournal<ForgeLog>>,
    catalog: Arc<Mutex<BTreeMap<[u8; ID_BYTES], ForgeJournalEvent>>>,
    policy: ForgeAcquisitionPolicy,
    limits: ForgeAcquisitionLimits,
}

impl fmt::Debug for ForgeAcquisitionService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForgeAcquisitionService")
            .field("policy", &self.policy)
            .field("limits", &self.limits)
            .field("journal_path", &self.journal.path())
            .finish_non_exhaustive()
    }
}

/// Forge owner failure after transport acquisition.
#[derive(Debug)]
pub enum ForgeAcquisitionError {
    /// Durable content store failure.
    Content(ContentStoreError),
    /// Journal or filesystem failure.
    Io(io::Error),
    /// Shared hash-chain journal failure.
    Journal(JournalError),
    /// Journal or object state was corrupt.
    Corrupt,
    /// Typed protocol/policy rejection.
    Rejected(ForgeRejectReason),
}

impl fmt::Display for ForgeAcquisitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Content(error) => write!(formatter, "forge content admission failed: {error}"),
            Self::Io(error) => write!(formatter, "forge persistence failed: {error}"),
            Self::Journal(error) => write!(formatter, "forge journal failed: {error}"),
            Self::Corrupt => formatter.write_str("forge cache or journal is corrupt"),
            Self::Rejected(reason) => write!(formatter, "forge acquisition rejected: {reason:?}"),
        }
    }
}
impl std::error::Error for ForgeAcquisitionError {}

fn content_error(error: ContentStoreError) -> ForgeAcquisitionError {
    if matches!(error, ContentStoreError::Bounds { .. }) {
        ForgeAcquisitionError::Rejected(ForgeRejectReason::Bounds)
    } else {
        ForgeAcquisitionError::Content(error)
    }
}
