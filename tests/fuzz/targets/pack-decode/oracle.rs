//! Oracle for untrusted index-pack bytes.
//!
//! [`IndexPack::open`] is the public decoder. The canonical seed is a
//! zero-segment header whose snapshot id comes from the production identity.

use backend_engine::index_publish::{ExactPackValue, IndexPack};
use backend_semantic::index_core::IndexSnapshot;
use backend_semantic::index_vocabulary::{
    ExactSegmentId, IndexPackId, IndexSnapshotId, LexicalSegmentId,
};
use backend_version::GenerationId;

use crate::{OracleFailure, Verdict};

/// Largest input this harness will mutate or replay.
///
/// The integer lives in the sibling `max_len` file so Nix and Rust share it.
/// It covers the fixed header plus a small directory and body.
pub(crate) const MAX_LEN: usize = crate::decimal_usize(include_str!("max_len"));

/// Width of the private `PackHeaderWire` record: magic, version, header width,
/// total, generation, snapshot, two counts, and six reserved bytes.
const HEADER_BYTES: usize = 88;

const _: () = assert!(HEADER_BYTES == 88);
const _: () = assert!(MAX_LEN >= HEADER_BYTES);
const _: () = assert!(MAX_LEN <= 1024 * 1024);

/// The committed `canonical` seed.
///
/// # Errors
///
/// Returns when the production snapshot identity cannot be derived or when
/// [`IndexPack::open`] rejects the header this function just wrote.
pub(crate) fn canonical() -> Result<Vec<u8>, String> {
    let generation = GenerationId::from_canonical_bytes(b"backend.fuzz.index-pack.v1");
    let exact: [Option<ExactSegmentId>; 0] = [];
    let lexical: [Option<LexicalSegmentId>; 0] = [];
    let snapshot = IndexSnapshot::canonical_identity_from_slots(generation, &exact, 0, &lexical, 0)
        .map_err(|error| error.to_string())?;
    let bytes = header(&generation, &snapshot);
    let id = IndexPackId::from_encoded_bytes(&bytes);
    if IndexPack::open(bytes.as_slice(), id).is_err() {
        return Err("zero-segment index pack was rejected by IndexPack::open".to_string());
    }
    Ok(bytes)
}

/// Replays pack admission.
///
/// # Errors
///
/// Returns when two opens disagree, a foreign artifact id is accepted, or a
/// borrowed row escapes the owner.
pub(crate) fn exercise(bytes: &[u8]) -> Result<(), OracleFailure> {
    judge(bytes).map(|_| ())
}

/// Classifies one buffer after [`IndexPack::open`].
///
/// # Errors
///
/// Returns when a law fails. A clean rejection is [`Verdict::Rejected`].
pub(crate) fn judge(bytes: &[u8]) -> Result<Verdict, OracleFailure> {
    let id = IndexPackId::from_encoded_bytes(bytes);
    let other = IndexPackId::from_encoded_bytes(b"backend.fuzz.pack.other-identity");
    if other != id && IndexPack::open(bytes, other).is_ok() {
        return Err(OracleFailure::new(
            "index pack opened under a different artifact id",
        ));
    }
    let first = IndexPack::open(bytes, id);
    let second = IndexPack::open(bytes, id);
    match (first, second) {
        (Ok(left), Ok(right)) => {
            if facts(&left) != facts(&right) {
                return Err(OracleFailure::new("index pack open is not deterministic"));
            }
            if left.as_ref() != bytes || right.as_ref() != bytes {
                return Err(OracleFailure::new(
                    "opened index pack bytes differ from the input",
                ));
            }
            probe(&left)?;
            probe(&right)?;
            Ok(Verdict::Accepted)
        }
        (Err(_), Err(_)) => Ok(Verdict::Rejected),
        _ => Err(OracleFailure::new("index pack open is not deterministic")),
    }
}

fn header(generation: &GenerationId, snapshot: &impl AsRef<[u8; 32]>) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_BYTES);
    out.extend_from_slice(b"NUDXIPK\0");
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&88u16.to_le_bytes());
    out.extend_from_slice(&88u32.to_le_bytes());
    let generation_bytes: &[u8; 32] = generation;
    out.extend_from_slice(generation_bytes);
    out.extend_from_slice(snapshot.as_ref());
    out.push(0);
    out.push(0);
    out.extend_from_slice(&[0; 6]);
    out
}

fn facts(pack: &IndexPack<&[u8]>) -> (IndexPackId, GenerationId, IndexSnapshotId, usize, usize) {
    (
        pack.id,
        pack.generation,
        pack.snapshot,
        pack.exact_segments,
        pack.lexical_segments,
    )
}

fn probe(pack: &IndexPack<&[u8]>) -> Result<(), OracleFailure> {
    let exact_id = ExactSegmentId::from_canonical_bytes(b"backend.fuzz.pack.absent-exact");
    let lexical_id = LexicalSegmentId::from_canonical_bytes(b"backend.fuzz.pack.absent-lexical");
    let owner = pack.as_ref();
    let view = pack.view();
    match view
        .exact(exact_id)
        .map_err(|error| OracleFailure::new(format!("exact view: {error}")))?
    {
        None => {}
        Some(segment) => {
            let row = segment.lookup(&[]).map_err(|error| {
                OracleFailure::new(format!("exact lookup on an opened pack: {error}"))
            })?;
            if let Some(row) = row {
                if !inside(owner, row.key) {
                    return Err(OracleFailure::new("exact key escapes the opened pack"));
                }
                if let ExactPackValue::Present(value) = row.value
                    && !inside(owner, value)
                {
                    return Err(OracleFailure::new("exact value escapes the opened pack"));
                }
            }
        }
    }
    match view
        .lexical(lexical_id)
        .map_err(|error| OracleFailure::new(format!("lexical view: {error}")))?
    {
        None => {}
        Some(segment) => {
            let range = segment.term_range(&[]).map_err(|error| {
                OracleFailure::new(format!("lexical term range on an opened pack: {error}"))
            })?;
            if range.start < range.end {
                let row = segment.row(range.start).map_err(|error| {
                    OracleFailure::new(format!("lexical row on an opened pack: {error}"))
                })?;
                if !inside(owner, row.term) {
                    return Err(OracleFailure::new("lexical term escapes the opened pack"));
                }
            }
        }
    }
    Ok(())
}

fn inside(owner: &[u8], body: &[u8]) -> bool {
    let span = owner.as_ptr_range();
    let found = body.as_ptr_range();
    found.start >= span.start && found.end <= span.end
}
