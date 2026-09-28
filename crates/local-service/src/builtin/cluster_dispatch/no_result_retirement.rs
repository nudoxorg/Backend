//! Durable owner-side maintenance for worker NoResult tombstones.
//!
//! The journal contains only authenticated NoResult attempts, not every trusted peer. Turso
//! mints a durable maintenance barrier for each exact terminal scope. The owner retries that
//! exact barrier after restart until the exact worker confirms it.
//!
//! Namespace high-water rows are retained only while that namespace has retirement debt. They
//! validate that the outbox never regresses behind an already persisted barrier; they do not
//! mint epochs or determine which attempt a worker may retire.

use backend_engine::cluster_transport::{AssignmentScope, EndpointId};
use backend_extension_turso::{AuthorityNamespace, AuthorityPlane};
use backend_platform::durable::{
    ensure_private_directory, open_private_read, write_private_atomic,
};
use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAGIC: &[u8; 8] = b"BKNRF001";
const VERSION: u16 = 2;
const CHECKSUM_BYTES: usize = 32;
const MAX_ROWS: usize = 4_096;
const MAX_BYTES: usize = 2 * 1024 * 1024;

type DebtKey = ([u8; 16], [u8; 32]);

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct RetirementTarget {
    pub(super) namespace_id: [u8; 16],
    pub(super) peer: [u8; 32],
    pub(super) no_result_scope: AssignmentScope,
    pub(super) authority_namespace: Option<AuthorityNamespace>,
    pub(super) anchor_scope: AssignmentScope,
    pub(super) retired_through_epoch: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct NoResultDebt {
    /// Newest observed terminal for this namespace+peer. It may be newer than `anchor`'s
    /// retirement prefix while an older prefix is awaiting its durable ACK.
    scope: AssignmentScope,
    /// Canonical authority tuple persisted and revalidated against the 128-bit transport ID.
    namespace: Option<AuthorityNamespace>,
    /// A separately tracked retireable prefix. Acknowledging it leaves `scope` behind whenever
    /// the newest terminal epoch is beyond `through`.
    anchor: Option<(AssignmentScope, u64)>,
}

#[derive(Clone, Default)]
struct JournalState {
    /// Latest exact namespace-global assignment observed by this owner.
    latest: BTreeMap<[u8; 16], AssignmentScope>,
    /// Only peers for which an authenticated NoResult terminal was observed.
    debts: BTreeMap<DebtKey, NoResultDebt>,
}

/// A small, checksummed outbox. Mutations are published in memory only after the atomic file
/// replacement succeeds, so a failed write cannot make debt disappear from the running process.
pub(super) struct NoResultRetirementJournal {
    path: PathBuf,
    owner: [u8; 32],
    state: Mutex<JournalState>,
}

#[derive(Debug)]
pub(super) enum JournalError {
    Io(io::Error),
    Corrupt(&'static str),
    OwnerMismatch,
    Capacity,
    EpochRegression,
    ScopeMismatch,
}

impl std::fmt::Display for JournalError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Corrupt(reason) => {
                write!(formatter, "corrupt NoResult retirement journal: {reason}")
            }
            Self::OwnerMismatch => formatter
                .write_str("NoResult retirement journal belongs to a different coordinator"),
            Self::Capacity => {
                formatter.write_str("NoResult retirement journal reached its bounded capacity")
            }
            Self::EpochRegression => {
                formatter.write_str("authority namespace attempt epoch regressed")
            }
            Self::ScopeMismatch => formatter
                .write_str("NoResult retirement scope does not match its namespace or peer"),
        }
    }
}

impl std::error::Error for JournalError {}

impl From<io::Error> for JournalError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl NoResultRetirementJournal {
    pub(super) fn open(path: impl AsRef<Path>, owner: EndpointId) -> Result<Self, JournalError> {
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .ok_or(JournalError::Corrupt("path has no parent"))?;
        ensure_private_directory(parent)?;
        let owner = *owner.as_bytes();
        let state = match fs::symlink_metadata(&path) {
            Ok(_) => {
                let mut file = open_private_read(&path)?;
                let mut bytes = Vec::new();
                file.by_ref()
                    .take((MAX_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > MAX_BYTES {
                    return Err(JournalError::Corrupt("file exceeds byte bound"));
                }
                decode(&bytes, owner)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let state = JournalState::default();
                persist(&path, owner, &state)?;
                state
            }
            Err(error) => return Err(JournalError::Io(error)),
        };
        Ok(Self {
            path,
            owner,
            state: Mutex::new(state),
        })
    }

    pub(super) const fn owner_id(&self) -> [u8; 32] {
        self.owner
    }

    /// Durably records an authenticated terminal before the caller releases its Offer journal
    /// reservation. Older duplicate terminals coalesce into the newest exact scope for the peer.
    pub(super) fn record_no_result(
        &self,
        scope: AssignmentScope,
        peer: EndpointId,
    ) -> Result<(), JournalError> {
        self.record_no_result_in_namespace(scope, peer, None)
    }

    /// Records an authenticated terminal together with its canonical Turso namespace.
    pub(super) fn record_no_result_in_namespace(
        &self,
        scope: AssignmentScope,
        peer: EndpointId,
        namespace: Option<AuthorityNamespace>,
    ) -> Result<(), JournalError> {
        validate_scope(scope)?;
        let peer = *peer.as_bytes();
        validate_peer(peer)?;
        validate_namespace(scope.namespace_id, namespace.as_ref())?;
        let key = (scope.namespace_id, peer);
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next = guard.clone();
        observe_scope(&mut next, scope, false)?;
        match next.debts.get_mut(&key) {
            Some(debt) if debt.scope.attempt > scope.attempt => {}
            Some(debt) if debt.scope.attempt == scope.attempt => {
                if debt.scope != scope
                    || (namespace.is_some()
                        && debt.namespace.as_ref().is_some_and(|known| {
                            Some(known) != namespace.as_ref()
                        }))
                {
                    return Err(JournalError::ScopeMismatch);
                }
                if let Some(namespace) = namespace.clone() {
                    debt.namespace = Some(namespace);
                }
            }
            Some(debt) => {
                debt.scope = scope;
                debt.namespace = namespace.clone();
                debt.anchor = None;
            }
            None => {
                if next.debts.len() >= MAX_ROWS {
                    return Err(JournalError::Capacity);
                }
                next.debts.insert(
                    key,
                    NoResultDebt {
                        scope,
                        namespace: namespace.clone(),
                        anchor: None,
                    },
                );
            }
        }
        validate_state(&next)?;
        persist(&self.path, self.owner, &next)?;
        *guard = next;
        Ok(())
    }

    /// Persists the authority-issued barrier for this exact terminal before any worker request.
    pub(super) fn anchor_turso_barrier(
        &self,
        terminal: AssignmentScope,
        peer: EndpointId,
        namespace: AuthorityNamespace,
        anchor: AssignmentScope,
        through: u64,
    ) -> Result<bool, JournalError> {
        validate_scope(terminal)?;
        validate_scope(anchor)?;
        validate_namespace(terminal.namespace_id, Some(&namespace))?;
        if anchor.namespace_id != terminal.namespace_id
            || through < terminal.attempt
            || through >= anchor.attempt
        {
            return Err(JournalError::ScopeMismatch);
        }
        let key = (terminal.namespace_id, *peer.as_bytes());
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(debt) = guard.debts.get(&key) else {
            return Ok(false);
        };
        if debt.scope != terminal || debt.namespace.as_ref().is_some_and(|known| known != &namespace) {
            return Ok(false);
        }
        let mut next = guard.clone();
        observe_scope(&mut next, anchor, false)?;
        let debt = next.debts.get_mut(&key).ok_or(JournalError::ScopeMismatch)?;
        debt.namespace = Some(namespace);
        debt.anchor = Some((anchor, through));
        validate_state(&next)?;
        persist(&self.path, self.owner, &next)?;
        *guard = next;
        Ok(true)
    }

    /// Lists terminal debts whose Turso barrier was not yet durably joined to the outbox.
    pub(super) fn pending_barriers(&self) -> Vec<RetirementTarget> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .debts
            .iter()
            .filter(|(_, debt)| debt.anchor.is_none())
            .map(|((namespace_id, peer), debt)| RetirementTarget {
                namespace_id: *namespace_id,
                peer: *peer,
                no_result_scope: debt.scope,
                authority_namespace: debt.namespace.clone(),
                anchor_scope: debt.anchor.map_or(debt.scope, |(scope, _)| scope),
                retired_through_epoch: debt.anchor.map_or(0, |(_, through)| through),
            })
            .collect()
    }

    /// Test helper for exercising multi-peer durable ACK behavior with an authority anchor.
    /// Production anchors are accepted only from `TursoAuthority::mint_no_result_retirement_barrier`.
    #[cfg(test)]
    pub(super) fn anchor_namespace(&self, scope: AssignmentScope) -> Result<(), JournalError> {
        validate_scope(scope)?;
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next = guard.clone();
        let has_debt = next
            .debts
            .keys()
            .any(|(namespace, _)| *namespace == scope.namespace_id);
        if !has_debt {
            return Ok(());
        }
        observe_scope(&mut next, scope, true)?;
        let through = scope.attempt.checked_sub(1).filter(|epoch| *epoch > 0);
        if let Some(through) = through {
            for ((namespace, _), debt) in &mut next.debts {
                if *namespace == scope.namespace_id && debt.scope.attempt < scope.attempt {
                    debt.anchor = Some((scope, through));
                }
            }
        }
        validate_state(&next)?;
        persist(&self.path, self.owner, &next)?;
        *guard = next;
        Ok(())
    }

    /// Returns a bounded snapshot of every currently anchored debt. There is intentionally no
    /// rotating prefix: a peer at the end of the ordered set is visited on every full sweep.
    pub(super) fn pending(&self) -> Vec<RetirementTarget> {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state
            .debts
            .iter()
            .filter_map(|((namespace_id, peer), debt)| {
                let (anchor_scope, retired_through_epoch) = debt.anchor?;
                Some(RetirementTarget {
                    namespace_id: *namespace_id,
                    peer: *peer,
                    no_result_scope: debt.scope,
                    authority_namespace: debt.namespace.clone(),
                    anchor_scope,
                    retired_through_epoch,
                })
            })
            .collect()
    }

    /// Clears only the exact snapshot acknowledged by the worker. An ACK for an older anchor
    /// cannot remove debt that was re-anchored while its request was in flight.
    pub(super) fn acknowledge(&self, target: RetirementTarget) -> Result<bool, JournalError> {
        let key = (target.namespace_id, target.peer);
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(debt) = guard.debts.get(&key).cloned() else {
            return Ok(false);
        };
        if debt.scope != target.no_result_scope
            || debt.anchor != Some((target.anchor_scope, target.retired_through_epoch))
        {
            return Ok(false);
        }
        let mut next = guard.clone();
        if debt.scope.attempt > target.retired_through_epoch {
            // The worker's confirmed floor covers the old prefix, but a terminal at or beyond
            // the anchoring assignment is outside it and must remain as fresh unanchored debt.
            if let Some(current) = next.debts.get_mut(&key) {
                current.anchor = None;
            }
        } else {
            next.debts.remove(&key);
            if !next
                .debts
                .keys()
                .any(|(namespace, _)| *namespace == target.namespace_id)
            {
                next.latest.remove(&target.namespace_id);
            }
        }
        persist(&self.path, self.owner, &next)?;
        *guard = next;
        Ok(true)
    }
}

fn observe_scope(
    state: &mut JournalState,
    scope: AssignmentScope,
    require_newer: bool,
) -> Result<(), JournalError> {
    match state.latest.get(&scope.namespace_id).copied() {
        Some(latest) if latest.attempt > scope.attempt => {
            return if require_newer {
                Err(JournalError::EpochRegression)
            } else {
                Ok(())
            };
        }
        Some(latest) if latest.attempt == scope.attempt && latest != scope => {
            return Err(JournalError::ScopeMismatch);
        }
        Some(_) => {}
        None if state.latest.len() >= MAX_ROWS => return Err(JournalError::Capacity),
        None => {}
    }
    state.latest.insert(scope.namespace_id, scope);
    Ok(())
}

fn validate_scope(scope: AssignmentScope) -> Result<(), JournalError> {
    if scope.validate().is_err() {
        return Err(JournalError::ScopeMismatch);
    }
    Ok(())
}

fn validate_peer(peer: [u8; 32]) -> Result<(), JournalError> {
    EndpointId::from_bytes(&peer).map_err(|_| JournalError::ScopeMismatch)?;
    Ok(())
}

fn validate_namespace(
    namespace_id: [u8; 16],
    namespace: Option<&AuthorityNamespace>,
) -> Result<(), JournalError> {
    if namespace.is_some_and(|namespace| namespace.namespace_id() != namespace_id) {
        return Err(JournalError::ScopeMismatch);
    }
    Ok(())
}

fn validate_state(state: &JournalState) -> Result<(), JournalError> {
    if state.debts.len() > MAX_ROWS || state.latest.len() > MAX_ROWS {
        return Err(JournalError::Capacity);
    }
    let debt_namespaces = state
        .debts
        .keys()
        .map(|(namespace, _)| *namespace)
        .collect::<std::collections::BTreeSet<_>>();
    for (namespace, latest) in &state.latest {
        validate_scope(*latest)?;
        if latest.namespace_id != *namespace || !debt_namespaces.contains(namespace) {
            return Err(JournalError::ScopeMismatch);
        }
    }
    for ((namespace, peer), debt) in &state.debts {
        validate_peer(*peer)?;
        validate_scope(debt.scope)?;
        if debt.scope.namespace_id != *namespace {
            return Err(JournalError::ScopeMismatch);
        }
        validate_namespace(*namespace, debt.namespace.as_ref())?;
        let latest = state
            .latest
            .get(namespace)
            .ok_or(JournalError::Corrupt("debt has no namespace high-water"))?;
        if debt.scope.attempt > latest.attempt
            || (debt.scope.attempt == latest.attempt && debt.scope != *latest)
        {
            return Err(JournalError::ScopeMismatch);
        }
        if let Some((anchor, through)) = debt.anchor {
            validate_scope(anchor)?;
            if anchor.namespace_id != *namespace
                || through < debt.scope.attempt
                || through >= anchor.attempt
                || anchor.attempt > latest.attempt
            {
                return Err(JournalError::ScopeMismatch);
            }
        }
    }
    Ok(())
}

fn persist(path: &Path, owner: [u8; 32], state: &JournalState) -> Result<(), JournalError> {
    let bytes = encode(owner, state)?;
    write_private_atomic(path, &bytes)?;
    Ok(())
}

fn encode(owner: [u8; 32], state: &JournalState) -> Result<Vec<u8>, JournalError> {
    validate_state(state)?;
    let mut bytes = Vec::with_capacity(128 + state.latest.len() * 72 + state.debts.len() * 256);
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&VERSION.to_le_bytes());
    bytes.extend_from_slice(&owner);
    put_len(&mut bytes, state.latest.len())?;
    for (namespace, scope) in &state.latest {
        bytes.extend_from_slice(namespace);
        put_scope(&mut bytes, *scope);
    }
    put_len(&mut bytes, state.debts.len())?;
    for ((namespace, peer), debt) in &state.debts {
        bytes.extend_from_slice(namespace);
        bytes.extend_from_slice(peer);
        put_scope(&mut bytes, debt.scope);
        put_namespace(&mut bytes, debt.namespace.as_ref())?;
        match debt.anchor {
            Some((scope, through)) => {
                bytes.push(1);
                put_scope(&mut bytes, scope);
                bytes.extend_from_slice(&through.to_le_bytes());
            }
            None => bytes.push(0),
        }
    }
    if bytes.len() + CHECKSUM_BYTES > MAX_BYTES {
        return Err(JournalError::Capacity);
    }
    let checksum = blake3::hash(&bytes);
    bytes.extend_from_slice(checksum.as_bytes());
    Ok(bytes)
}

fn decode(bytes: &[u8], expected_owner: [u8; 32]) -> Result<JournalState, JournalError> {
    if bytes.len() < 8 + 2 + 32 + 4 + 4 + CHECKSUM_BYTES || bytes.len() > MAX_BYTES {
        return Err(JournalError::Corrupt("invalid file length"));
    }
    let checksum_offset = bytes.len() - CHECKSUM_BYTES;
    if blake3::hash(&bytes[..checksum_offset]).as_bytes() != &bytes[checksum_offset..] {
        return Err(JournalError::Corrupt("checksum mismatch"));
    }
    let mut cursor = Cursor::new(&bytes[..checksum_offset]);
    if read_array::<8>(&mut cursor)? != *MAGIC {
        return Err(JournalError::Corrupt("magic mismatch"));
    }
    let version = read_u16(&mut cursor)?;
    if version != 1 && version != VERSION {
        return Err(JournalError::Corrupt("unsupported version"));
    }
    if read_array::<32>(&mut cursor)? != expected_owner {
        return Err(JournalError::OwnerMismatch);
    }
    let latest_count = read_len(&mut cursor)?;
    if latest_count > MAX_ROWS {
        return Err(JournalError::Corrupt("too many namespace high-water rows"));
    }
    let mut latest = BTreeMap::new();
    let mut previous_namespace = None;
    for _ in 0..latest_count {
        let namespace = read_array::<16>(&mut cursor)?;
        if previous_namespace.is_some_and(|previous| previous >= namespace) {
            return Err(JournalError::Corrupt("namespace rows are not canonical"));
        }
        previous_namespace = Some(namespace);
        let scope = read_scope(&mut cursor)?;
        if scope.namespace_id != namespace {
            return Err(JournalError::Corrupt("namespace high-water scope mismatch"));
        }
        latest.insert(namespace, scope);
    }
    let debt_count = read_len(&mut cursor)?;
    if debt_count > MAX_ROWS {
        return Err(JournalError::Corrupt("too many peer debt rows"));
    }
    let mut debts = BTreeMap::new();
    let mut previous_key = None;
    for _ in 0..debt_count {
        let namespace = read_array::<16>(&mut cursor)?;
        let peer = read_array::<32>(&mut cursor)?;
        let key = (namespace, peer);
        if previous_key.is_some_and(|previous| previous >= key) {
            return Err(JournalError::Corrupt("peer debt rows are not canonical"));
        }
        previous_key = Some(key);
        let scope = read_scope(&mut cursor)?;
        let (namespace_value, encoded_anchor) = if version == 1 {
            (None, match read_u8(&mut cursor)? {
                0 => None,
                1 => Some((read_scope(&mut cursor)?, read_u64(&mut cursor)?)),
                _ => return Err(JournalError::Corrupt("invalid anchor marker")),
            })
        } else {
            let namespace = read_namespace(&mut cursor)?;
            let anchor = match read_u8(&mut cursor)? {
                0 => None,
                1 => Some((read_scope(&mut cursor)?, read_u64(&mut cursor)?)),
                _ => return Err(JournalError::Corrupt("invalid anchor marker")),
            };
            (namespace, anchor)
        };
        // Version-one anchors were calculated by the coordinator from its local view of the
        // attempt ordinal. Keep the terminal debt, but require Turso to issue a fresh barrier.
        let anchor = if version == 1 { None } else { encoded_anchor };
        debts.insert(
            key,
            NoResultDebt {
                scope,
                namespace: namespace_value,
                anchor,
            },
        );
    }
    if cursor.position() as usize != checksum_offset {
        return Err(JournalError::Corrupt("trailing bytes"));
    }
    // Older journal versions kept every observed assignment, including namespaces with no
    // retirement debt. Drop those rows on load; only debt-bearing namespaces need a high-water
    // to validate and safely re-anchor a pending worker tombstone.
    let debt_namespaces = debts
        .keys()
        .map(|(namespace, _)| *namespace)
        .collect::<std::collections::BTreeSet<_>>();
    let latest = latest
        .into_iter()
        .filter(|(namespace, _)| debt_namespaces.contains(namespace))
        .collect();
    let state = JournalState { latest, debts };
    validate_state(&state)?;
    Ok(state)
}

fn put_scope(bytes: &mut Vec<u8>, scope: AssignmentScope) {
    bytes.extend_from_slice(&scope.namespace_id);
    bytes.extend_from_slice(&scope.work_id);
    bytes.extend_from_slice(&scope.attempt.to_le_bytes());
    bytes.extend_from_slice(&scope.fence);
}

fn read_scope(cursor: &mut Cursor<&[u8]>) -> Result<AssignmentScope, JournalError> {
    let namespace = read_array::<16>(cursor)?;
    let work = read_array::<16>(cursor)?;
    let attempt = read_u64(cursor)?;
    let fence = read_array::<32>(cursor)?;
    AssignmentScope::new(namespace, work, attempt, fence)
        .map_err(|_| JournalError::Corrupt("invalid assignment scope"))
}

fn put_namespace(
    bytes: &mut Vec<u8>,
    namespace: Option<&AuthorityNamespace>,
) -> Result<(), JournalError> {
    let Some(namespace) = namespace else {
        bytes.push(0);
        return Ok(());
    };
    bytes.push(1);
    put_string(bytes, namespace.package())?;
    put_string(bytes, namespace.source())?;
    put_string(bytes, namespace.branch())?;
    put_string(bytes, namespace.environment())?;
    match namespace.plane() {
        AuthorityPlane::PackageMetadata => bytes.push(0),
        AuthorityPlane::SemanticProfile(profile) => {
            bytes.push(1);
            put_string(bytes, profile)?;
        }
    }
    Ok(())
}

fn read_namespace(
    cursor: &mut Cursor<&[u8]>,
) -> Result<Option<AuthorityNamespace>, JournalError> {
    match read_u8(cursor)? {
        0 => Ok(None),
        1 => {
            let package = read_string(cursor)?;
            let source = read_string(cursor)?;
            let branch = read_string(cursor)?;
            let environment = read_string(cursor)?;
            let plane = match read_u8(cursor)? {
                0 => AuthorityPlane::PackageMetadata,
                1 => AuthorityPlane::semantic_profile(read_string(cursor)?)
                    .map_err(|_| JournalError::Corrupt("invalid authority profile"))?,
                _ => return Err(JournalError::Corrupt("invalid authority plane")),
            };
            AuthorityNamespace::with_plane(package, source, branch, environment, plane)
                .map(Some)
                .map_err(|_| JournalError::Corrupt("invalid authority namespace"))
        }
        _ => Err(JournalError::Corrupt("invalid namespace marker")),
    }
}

fn put_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), JournalError> {
    let length = u32::try_from(value.len()).map_err(|_| JournalError::Capacity)?;
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn read_string(cursor: &mut Cursor<&[u8]>) -> Result<String, JournalError> {
    let length = usize::try_from(read_u32(cursor)?)
        .map_err(|_| JournalError::Corrupt("namespace string length overflow"))?;
    if length > 4_096 {
        return Err(JournalError::Corrupt("namespace string exceeds bound"));
    }
    let mut bytes = vec![0; length];
    cursor.read_exact(&mut bytes)?;
    String::from_utf8(bytes).map_err(|_| JournalError::Corrupt("namespace string is not UTF-8"))
}

fn put_len(bytes: &mut Vec<u8>, len: usize) -> Result<(), JournalError> {
    let len = u32::try_from(len).map_err(|_| JournalError::Capacity)?;
    bytes.extend_from_slice(&len.to_le_bytes());
    Ok(())
}

fn read_len(cursor: &mut Cursor<&[u8]>) -> Result<usize, JournalError> {
    usize::try_from(read_u32(cursor)?).map_err(|_| JournalError::Corrupt("count overflow"))
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> Result<u8, JournalError> {
    let mut value = [0; 1];
    cursor.read_exact(&mut value)?;
    Ok(value[0])
}

fn read_u16(cursor: &mut Cursor<&[u8]>) -> Result<u16, JournalError> {
    Ok(u16::from_le_bytes(read_array(cursor)?))
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> Result<u32, JournalError> {
    Ok(u32::from_le_bytes(read_array(cursor)?))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> Result<u64, JournalError> {
    Ok(u64::from_le_bytes(read_array(cursor)?))
}

fn read_array<const N: usize>(cursor: &mut Cursor<&[u8]>) -> Result<[u8; N], JournalError> {
    let mut value = [0; N];
    cursor.read_exact(&mut value)?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_engine::cluster_transport::SecretKey;
    use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock is after epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "backend-noresult-outbox-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create test directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700))
                .expect("make test directory private");
        }
        path.join("outbox.v1")
    }

    fn scope(attempt: u64) -> AssignmentScope {
        let work = [u8::try_from(attempt).expect("test attempt fits"); 16];
        let fence = [u8::try_from(attempt).expect("test attempt fits"); 32];
        AssignmentScope::new([0x51; 16], work, attempt, fence).expect("valid attempt")
    }

    fn scope_in(namespace: [u8; 16], attempt: u64) -> AssignmentScope {
        AssignmentScope::new(namespace, [0x61; 16], attempt, [0x62; 32])
            .expect("valid namespaced attempt")
    }

    #[test]
    fn durable_debt_keeps_the_canonical_typed_namespace_and_exact_barrier() {
        let path = temp_path();
        let owner = EndpointId::from_bytes(SecretKey::from_bytes(&[0x70; 32]).public().as_bytes())
            .expect("valid owner ID");
        let peer = peers(1)[0];
        let namespace = AuthorityNamespace::semantic_profile(
            "pkg:cargo/widget",
            "registry:crates-io",
            "main",
            "stable",
            "rust/2024/lower-ir",
        )
        .expect("typed semantic namespace");
        let terminal = AssignmentScope::new(
            namespace.namespace_id(),
            [0x71; 16],
            5,
            [0x72; 32],
        )
        .expect("exact terminal");
        let anchor = AssignmentScope::new(
            namespace.namespace_id(),
            [0x73; 16],
            6,
            [0x74; 32],
        )
        .expect("Turso-issued barrier scope");
        let journal = NoResultRetirementJournal::open(&path, owner).expect("open journal");
        journal
            .record_no_result_in_namespace(terminal, peer, Some(namespace.clone()))
            .expect("persist canonical authority namespace with terminal debt");
        assert!(matches!(
            journal.record_no_result_in_namespace(
                terminal,
                peer,
                Some(AuthorityNamespace::package_metadata(
                    "pkg:cargo/other",
                    "registry:crates-io",
                    "main",
                    "stable",
                )
                .expect("different namespace")),
            ),
            Err(JournalError::ScopeMismatch)
        ));
        assert!(journal
            .anchor_turso_barrier(terminal, peer, namespace.clone(), anchor, 5)
            .expect("persist exact barrier"));
        drop(journal);

        let reopened = NoResultRetirementJournal::open(&path, owner).expect("cold reopen");
        let target = reopened.pending().pop().expect("anchored exact debt");
        assert_eq!(target.authority_namespace, Some(namespace));
        assert_eq!(target.no_result_scope, terminal);
        assert_eq!(target.anchor_scope, anchor);
        assert_eq!(target.retired_through_epoch, terminal.attempt);
        fs::remove_dir_all(path.parent().expect("test directory"))
            .expect("remove test directory");
    }

    fn peers(count: usize) -> Vec<EndpointId> {
        let mut peers = (1..=count)
            .map(|seed| {
                let seed = u8::try_from(seed).expect("test peer seed fits");
                EndpointId::from_bytes(SecretKey::from_bytes(&[seed; 32]).public().as_bytes())
                    .expect("valid endpoint ID")
            })
            .collect::<Vec<_>>();
        peers.sort_by_key(|peer| *peer.as_bytes());
        peers
    }

    #[test]
    fn last_peer_debt_survives_restart_and_reanchoring_rejects_old_ack() {
        let path = temp_path();
        let owner = EndpointId::from_bytes(SecretKey::from_bytes(&[0x77; 32]).public().as_bytes())
            .expect("valid owner ID");
        let peers = peers(65);
        let journal = NoResultRetirementJournal::open(&path, owner).expect("open journal");

        // Independently record 65 ordered workers. The last worker's offline response must not
        // be skipped by a bounded 64-peer prefix after a restart.
        for (index, peer) in peers.iter().copied().enumerate() {
            journal
                .record_no_result(scope(index as u64 + 1), peer)
                .expect("persist exact NoResult");
        }
        let anchor = scope(66);
        journal
            .anchor_namespace(anchor)
            .expect("persist newer namespace attempt");
        drop(journal);

        let restarted =
            Arc::new(NoResultRetirementJournal::open(&path, owner).expect("cold reopen"));
        let snapshot = restarted.pending();
        assert_eq!(snapshot.len(), 65);
        let last_peer = *peers.last().expect("65 peers").as_bytes();
        let last = snapshot.last().expect("last ordered target");
        assert_eq!(last.peer, last_peer);
        assert_eq!(last.anchor_scope, anchor);

        // Simulate a complete bounded-concurrency sweep: the first 64 peers confirm, while the
        // last is offline. Persisted state must retain it without relying on a process cursor.
        let report = futures_executor::block_on(super::super::sweep_no_result_retirement_targets(
            Arc::clone(&restarted),
            &snapshot,
            {
                let last_peer = last_peer;
                move |target| async move { target.peer != last_peer }
            },
        ));
        assert_eq!(report.attempted, 65);
        assert_eq!(report.confirmed, 64);
        drop(restarted);
        let restarted =
            Arc::new(NoResultRetirementJournal::open(&path, owner).expect("second cold reopen"));
        let pending = restarted.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].peer, last_peer);

        let newer_anchor = scope(67);
        restarted
            .anchor_namespace(newer_anchor)
            .expect("fresh attempt reanchors pending debt");
        assert!(
            !restarted
                .acknowledge(pending[0])
                .expect("late old ACK is harmless")
        );
        let exact = restarted.pending();
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].anchor_scope, newer_anchor);
        let retry_report =
            futures_executor::block_on(super::super::sweep_no_result_retirement_targets(
                Arc::clone(&restarted),
                &exact,
                |_| async { true },
            ));
        assert_eq!(retry_report.attempted, 1);
        assert_eq!(retry_report.confirmed, 1);
        assert!(restarted.pending().is_empty());
        fs::remove_dir_all(path.parent().expect("test directory")).expect("clean test directory");
    }

    #[test]
    fn newer_no_result_does_not_erase_in_flight_retirement_prefix() {
        let path = temp_path();
        let owner = EndpointId::from_bytes(SecretKey::from_bytes(&[0x7c; 32]).public().as_bytes())
            .expect("valid owner ID");
        let peer = peers(1)[0];
        let journal = NoResultRetirementJournal::open(&path, owner).expect("open journal");
        journal
            .record_no_result(scope(5), peer)
            .expect("record first terminal");
        let first_anchor = scope(6);
        journal
            .anchor_namespace(first_anchor)
            .expect("anchor old terminal");
        let stale_snapshot = journal.pending()[0].clone();
        journal
            .record_no_result(first_anchor, peer)
            .expect("newer terminal arrives before old-prefix ACK");
        drop(journal);

        let journal = Arc::new(NoResultRetirementJournal::open(&path, owner).expect("cold reopen"));
        let in_flight = journal.pending();
        assert_eq!(in_flight.len(), 1);
        assert_eq!(in_flight[0].no_result_scope, first_anchor);
        assert_eq!(in_flight[0].anchor_scope, first_anchor);
        assert_eq!(in_flight[0].retired_through_epoch, 5);
        assert!(
            !journal
                .acknowledge(stale_snapshot)
                .expect("pre-update receipt is stale")
        );

        let report = futures_executor::block_on(super::super::sweep_no_result_retirement_targets(
            Arc::clone(&journal),
            &in_flight,
            |_| async { true },
        ));
        assert_eq!(report.confirmed, 1);
        assert!(journal.pending().is_empty());

        // ACKing through epoch 5 retires only that prefix. The epoch-6 terminal remains and is
        // re-anchored by the next fresh attempt, then can be safely acknowledged through 6.
        let second_anchor = scope(7);
        journal
            .anchor_namespace(second_anchor)
            .expect("anchor retained newer terminal");
        let residual = journal.pending();
        assert_eq!(residual.len(), 1);
        assert_eq!(residual[0].no_result_scope, first_anchor);
        assert_eq!(residual[0].anchor_scope, second_anchor);
        assert_eq!(residual[0].retired_through_epoch, 6);
        let report = futures_executor::block_on(super::super::sweep_no_result_retirement_targets(
            Arc::clone(&journal),
            &residual,
            |_| async { true },
        ));
        assert_eq!(report.confirmed, 1);
        assert!(journal.pending().is_empty());
        fs::remove_dir_all(path.parent().expect("test directory")).expect("clean test directory");
    }

    #[test]
    fn journal_is_owner_bound_and_corruption_fails_closed() {
        let path = temp_path();
        let owner = EndpointId::from_bytes(SecretKey::from_bytes(&[0x78; 32]).public().as_bytes())
            .expect("valid owner ID");
        NoResultRetirementJournal::open(&path, owner).expect("create journal");
        let other = EndpointId::from_bytes(SecretKey::from_bytes(&[0x79; 32]).public().as_bytes())
            .expect("valid other owner ID");
        assert!(matches!(
            NoResultRetirementJournal::open(&path, other),
            Err(JournalError::OwnerMismatch)
        ));
        let mut bytes = fs::read(&path).expect("read journal");
        bytes[10] ^= 0x80;
        fs::write(&path, bytes).expect("corrupt journal");
        assert!(matches!(
            NoResultRetirementJournal::open(&path, owner),
            Err(JournalError::Corrupt("checksum mismatch"))
        ));
        fs::remove_dir_all(path.parent().expect("test directory")).expect("clean test directory");
    }

    #[test]
    fn late_no_result_uses_persisted_newer_namespace_anchor() {
        let path = temp_path();
        let owner = EndpointId::from_bytes(SecretKey::from_bytes(&[0x7a; 32]).public().as_bytes())
            .expect("valid owner ID");
        let peers = peers(2);
        let journal = NoResultRetirementJournal::open(&path, owner).expect("open journal");
        let anchor = scope(6);
        journal
            .record_no_result(scope(4), peers[0])
            .expect("existing debt retains namespace high-water");
        journal
            .anchor_namespace(anchor)
            .expect("store namespace high-water");
        journal
            .record_no_result(scope(5), peers[1])
            .expect("late terminal is accepted under fresh anchor");

        let pending = journal.pending();
        assert_eq!(pending.len(), 2);
        let late = pending
            .iter()
            .find(|target| target.peer == *peers[1].as_bytes())
            .expect("late worker debt");
        assert_eq!(late.no_result_scope, scope(5));
        assert_eq!(late.anchor_scope, anchor);
        assert_eq!(late.retired_through_epoch, 5);
        fs::remove_dir_all(path.parent().expect("test directory")).expect("clean test directory");
    }

    #[test]
    fn retrying_durable_anchor_after_restart_is_idempotent() {
        let path = temp_path();
        let owner = EndpointId::from_bytes(SecretKey::from_bytes(&[0x7d; 32]).public().as_bytes())
            .expect("valid owner ID");
        let peer = peers(1)[0];
        let anchor = scope(6);
        {
            let journal = NoResultRetirementJournal::open(&path, owner).expect("open journal");
            journal
                .record_no_result(scope(5), peer)
                .expect("persist terminal before anchor");
            journal
                .anchor_namespace(anchor)
                .expect("persist anchor before remote visibility");
        }

        // The owner may crash after persisting the anchor but before returning the assignment.
        // Retrying the exact database token must accept the equal high-water scope and preserve
        // the pending retirement prefix.
        let journal = NoResultRetirementJournal::open(&path, owner).expect("cold reopen");
        journal
            .anchor_namespace(anchor)
            .expect("same authoritative anchor is idempotent");
        let pending = journal.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].peer, *peer.as_bytes());
        assert_eq!(pending[0].no_result_scope, scope(5));
        assert_eq!(pending[0].anchor_scope, anchor);
        assert_eq!(pending[0].retired_through_epoch, 5);
        fs::remove_dir_all(path.parent().expect("test directory")).expect("clean test directory");
    }

    #[test]
    fn no_debt_namespace_anchors_do_not_consume_capacity_and_late_debt_waits_for_new_anchor() {
        const NO_DEBT_NAMESPACES: u64 = MAX_ROWS as u64 + 17;
        let path = temp_path();
        let owner = EndpointId::from_bytes(SecretKey::from_bytes(&[0x7e; 32]).public().as_bytes())
            .expect("valid owner ID");
        let peer = peers(1)[0];
        let protected_namespace = [0xa1; 16];
        let journal = NoResultRetirementJournal::open(&path, owner).expect("open journal");
        journal
            .record_no_result(scope_in(protected_namespace, 1), peer)
            .expect("record debt before anchor");
        let protected_anchor = scope_in(protected_namespace, 2);
        journal
            .anchor_namespace(protected_anchor)
            .expect("anchor debt-bearing namespace");

        // Simulate thousands of independent fresh namespaces which have no worker debt. Their
        // anchors must not accumulate durable high-water rows or crowd out the actionable one.
        for index in 0..NO_DEBT_NAMESPACES {
            let mut namespace = [0x33; 16];
            namespace[8..].copy_from_slice(&(index + 1).to_be_bytes());
            journal
                .anchor_namespace(scope_in(namespace, 1))
                .expect("no-debt anchor needs no retained row");
        }
        {
            let state = journal
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(state.latest.len(), 1);
            assert_eq!(state.debts.len(), 1);
            assert_eq!(state.latest[&protected_namespace], protected_anchor);
        }
        assert_eq!(journal.pending().len(), 1);

        // An old terminal arriving after a no-debt anchor cannot use an evicted high-water as
        // authority. It persists across restart as unanchored debt, then becomes retireable only
        // after another real, newer assignment is durably observed.
        let late_namespace = [0xb2; 16];
        journal
            .anchor_namespace(scope_in(late_namespace, 2))
            .expect("no-debt anchor is intentionally not retained");
        journal
            .record_no_result(scope_in(late_namespace, 1), peer)
            .expect("late terminal remains safe unanchored debt");
        assert_eq!(journal.pending().len(), 1);
        drop(journal);

        let journal = NoResultRetirementJournal::open(&path, owner).expect("cold reopen");
        assert_eq!(journal.pending().len(), 1);
        {
            let state = journal
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(state.latest.len(), 2);
            assert_eq!(state.debts.len(), 2);
            let mut first_no_debt_namespace = [0x33; 16];
            first_no_debt_namespace[8..].copy_from_slice(&1_u64.to_be_bytes());
            assert!(!state.latest.contains_key(&first_no_debt_namespace));
        }
        let late_anchor = scope_in(late_namespace, 3);
        journal
            .anchor_namespace(late_anchor)
            .expect("new real assignment anchors late debt");
        let pending = journal.pending();
        assert_eq!(pending.len(), 2);
        let late = pending
            .iter()
            .find(|target| target.namespace_id == late_namespace)
            .expect("late debt is now anchored");
        assert_eq!(late.no_result_scope, scope_in(late_namespace, 1));
        assert_eq!(late.anchor_scope, late_anchor);
        assert_eq!(late.retired_through_epoch, 2);
        drop(journal);

        let journal = NoResultRetirementJournal::open(&path, owner).expect("second cold reopen");
        assert_eq!(journal.pending().len(), 2);
        fs::remove_dir_all(path.parent().expect("test directory")).expect("clean test directory");
    }

    #[test]
    fn final_debt_ack_prunes_namespace_high_water() {
        let path = temp_path();
        let owner = EndpointId::from_bytes(SecretKey::from_bytes(&[0x7f; 32]).public().as_bytes())
            .expect("valid owner ID");
        let peer = peers(1)[0];
        let journal = NoResultRetirementJournal::open(&path, owner).expect("open journal");
        journal
            .record_no_result(scope(5), peer)
            .expect("record terminal");
        journal.anchor_namespace(scope(6)).expect("anchor terminal");
        let target = journal.pending()[0].clone();
        assert!(journal.acknowledge(target).expect("exact retirement ACK"));
        {
            let state = journal
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert!(state.latest.is_empty());
            assert!(state.debts.is_empty());
        }
        drop(journal);

        let journal = NoResultRetirementJournal::open(&path, owner).expect("cold reopen");
        journal
            .anchor_namespace(scope(7))
            .expect("no-debt anchor requires no persisted high-water");
        let state = journal
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(state.latest.is_empty());
        assert!(state.debts.is_empty());
        drop(state);
        drop(journal);
        fs::remove_dir_all(path.parent().expect("test directory")).expect("clean test directory");
    }

    #[test]
    fn sixty_fifth_grant_with_the_only_no_result_is_swept() {
        let path = temp_path();
        let owner = EndpointId::from_bytes(SecretKey::from_bytes(&[0x7b; 32]).public().as_bytes())
            .expect("valid owner ID");
        let peers = peers(65);
        let namespace = [0x51; 16];
        let address = "127.0.0.1:34001".parse().expect("valid worker address");
        let mut policy = super::super::TrustedCompilerWorkerPolicy::default();
        for (index, peer) in peers.iter().copied().enumerate() {
            let recipe = [u8::try_from(index + 1).expect("test recipe fits"); 32];
            policy
                .add(
                    super::super::TrustedCompilerWorkerGrant::new(
                        peer,
                        address,
                        namespace,
                        recipe,
                        LanguageProfile::Rust(RustEdition::Rust2024),
                        Stage::LowerIr,
                        [0x41; 32],
                        [0x42; 32],
                        [0x43; 32],
                    )
                    .expect("valid trusted worker grant"),
                )
                .expect("distinct worker grant");
        }
        let routes = super::super::trusted_no_result_retirement_routes(&policy);
        assert_eq!(routes.len(), 65);

        let journal =
            Arc::new(NoResultRetirementJournal::open(&path, owner).expect("open journal"));
        let last_peer = *peers.last().expect("65 peers");
        journal
            .record_no_result(scope(5), last_peer)
            .expect("persist the last worker's authenticated NoResult");
        journal
            .anchor_namespace(scope(6))
            .expect("persist fresh scope for the existing debt");
        let targets = journal.pending();
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].peer, *last_peer.as_bytes());

        // The owner resolves only the peer with durable NoResult debt. It does not truncate or
        // sweep a policy-wide list whose first 64 rows could hide this 65th worker.
        let routes = Arc::new(routes);
        let report = futures_executor::block_on(super::super::sweep_no_result_retirement_targets(
            Arc::clone(&journal),
            &targets,
            {
                let routes = Arc::clone(&routes);
                move |target| {
                    let routes = Arc::clone(&routes);
                    async move { routes.contains_key(&(target.namespace_id, target.peer)) }
                }
            },
        ));
        assert_eq!(report.attempted, 1);
        assert_eq!(report.confirmed, 1);
        assert!(journal.pending().is_empty());
        fs::remove_dir_all(path.parent().expect("test directory")).expect("clean test directory");
    }
}
