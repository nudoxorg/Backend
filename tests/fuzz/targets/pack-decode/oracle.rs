//! Oracle for untrusted index-pack bytes.
//!
//! [`IndexPack::open`] is the public decoder. The canonical seed is a
//! zero-segment header whose snapshot id comes from the production identity.
//! `encode_index_pack` takes only a compiler snapshot, so the seeds that carry
//! segments are written here from public `index_core` rows, in the wire layout
//! that the `valid_exact_pack` fixture in `pack/view.rs` writes. Without them
//! the fuzzer never reaches segment or row parsing.

use backend_engine::index_publish::{ExactPackValue, IndexPack};
use backend_semantic::index_core::{
    ENTITY_DOCUMENT_ID_BYTES, EntityArtifactIdentity, EntityDocumentId, ExactRow, ExactSegment,
    IndexSnapshot, LexicalRow, LexicalRowValue, LexicalScore, LexicalSegment,
};
use backend_semantic::index_vocabulary::{
    ExactSegmentId, IndexPackId, IndexSnapshotId, LexicalSegmentId,
};
use backend_semantic::ir::EntityId;
use backend_version::{ArtifactId, GenerationId, IrFragmentDomain, IrFragmentEncoding};

use crate::{OracleFailure, Verdict};

/// Largest input this harness will mutate or replay.
///
/// The integer lives in the sibling `max_len` file so Nix and Rust share it.
/// It covers the fixed header plus a small directory and body.
pub(crate) const MAX_LEN: usize = crate::decimal_usize(include_str!("max_len"));

/// Width of the private `PackHeaderWire` record: magic, version, header width,
/// total, generation, snapshot, two counts, and six reserved bytes.
const HEADER_BYTES: usize = 88;

/// Width of the private `SegmentDirectoryWire` record: segment id, body start,
/// and body length.
const DIRECTORY_BYTES: usize = 40;

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
    let generation = generation();
    let exact: [Option<ExactSegmentId>; 0] = [];
    let lexical: [Option<LexicalSegmentId>; 0] = [];
    let snapshot = IndexSnapshot::canonical_identity_from_slots(
        generation,
        &exact,
        0,
        &lexical,
        0,
    )
    .map_err(|error| error.to_string())?;
    let bytes = header(&generation, &snapshot, 88, 0, 0);
    let id = IndexPackId::from_encoded_bytes(&bytes);
    if IndexPack::open(bytes.as_slice(), id).is_err() {
        return Err("zero-segment index pack was rejected by IndexPack::open".to_string());
    }
    Ok(bytes)
}

/// The committed seeds that carry segments, as (corpus file name, exact lane,
/// lexical lane).
///
/// A name reads `<lanes>-<segments>x<rows per segment>`. The eight exact-only
/// packs are the fuzz bench's `pack-exact` seeds, byte for byte. Lexical rows
/// come in pairs that share a term.
const SEGMENT_SEEDS: [(&str, Lane, Lane); 11] = [
    ("exact-1x1", Lane::new(1, 1, 0), Lane::EMPTY),
    ("exact-1x2", Lane::new(1, 2, 0), Lane::EMPTY),
    ("exact-1x3-tombstone", Lane::new(1, 3, 2), Lane::EMPTY),
    ("exact-2x1", Lane::new(2, 1, 0), Lane::EMPTY),
    ("exact-2x3", Lane::new(2, 3, 0), Lane::EMPTY),
    ("exact-3x2-tombstones", Lane::new(3, 2, 2), Lane::EMPTY),
    ("exact-1x8-tombstones", Lane::new(1, 8, 3), Lane::EMPTY),
    ("exact-4x2", Lane::new(4, 2, 0), Lane::EMPTY),
    ("lexical-1x2", Lane::EMPTY, Lane::new(1, 2, 0)),
    ("lexical-2x3-tombstones", Lane::EMPTY, Lane::new(2, 3, 3)),
    (
        "exact-lexical-2x2-tombstones",
        Lane::new(2, 2, 2),
        Lane::new(2, 2, 2),
    ),
];

/// Fresh encodes of the committed segment seeds, by corpus file name.
///
/// The non-test library build does not read these files. The seed test does,
/// so this function is live only under `cfg(test)`.
///
/// # Errors
///
/// Returns when `index_core` rejects a fixture row set or snapshot, or when
/// [`IndexPack::open`] rejects a pack this function just wrote.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the seed test is the only caller")
)]
pub(crate) fn segment_seeds() -> Result<Vec<(&'static str, Vec<u8>)>, String> {
    SEGMENT_SEEDS
        .iter()
        .map(|&(name, exact, lexical)| Ok((name, segment_pack(exact, lexical)?)))
        .collect()
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

fn header(
    generation: &GenerationId,
    snapshot: &impl AsRef<[u8; 32]>,
    total: u32,
    exact: u8,
    lexical: u8,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_BYTES);
    out.extend_from_slice(b"NUDXIPK\0");
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&88u16.to_le_bytes());
    out.extend_from_slice(&total.to_le_bytes());
    let generation_bytes: &[u8; 32] = generation;
    out.extend_from_slice(generation_bytes);
    out.extend_from_slice(snapshot.as_ref());
    out.push(exact);
    out.push(lexical);
    out.extend_from_slice(&[0; 6]);
    out
}

fn generation() -> GenerationId {
    GenerationId::from_canonical_bytes(b"backend.fuzz.index-pack.v1")
}

/// One lane of a segment seed: its segment count, the rows in each segment,
/// and every `tombstone`-th row deleted (0 deletes none).
#[derive(Clone, Copy)]
struct Lane {
    segments: usize,
    rows: u16,
    tombstone: u16,
}

impl Lane {
    const EMPTY: Self = Self::new(0, 0, 0);

    const fn new(segments: usize, rows: u16, tombstone: u16) -> Self {
        Self {
            segments,
            rows,
            tombstone,
        }
    }

    const fn deletes(self, row: u16) -> bool {
        self.tombstone > 0 && row % self.tombstone == self.tombstone - 1
    }

    /// Exact rows: distinct keys, values of one to three repeats.
    fn exact_cells(self, segment: usize) -> Vec<Cell> {
        (0..self.rows)
            .map(|row| Cell {
                key: format!("seg{segment}-key{row:02}").into_bytes(),
                value: format!("value-{segment}-{row}")
                    .repeat(usize::from(1 + row % 3))
                    .into_bytes(),
                ordinal: row,
                deleted: self.deletes(row),
            })
            .collect()
    }

    /// Lexical rows: pairs of rows share a term, in document order.
    fn lexical_cells(self, segment: usize) -> Vec<Cell> {
        (0..self.rows)
            .map(|row| Cell {
                key: format!("seg{segment}-term{:02}", row / 2).into_bytes(),
                value: Vec::new(),
                ordinal: row,
                deleted: self.deletes(row),
            })
            .collect()
    }
}

/// Owned bytes behind one fixture row. `ordinal` picks the lexical document
/// and score.
struct Cell {
    key: Vec<u8>,
    value: Vec<u8>,
    ordinal: u16,
    deleted: bool,
}

impl Cell {
    fn exact(&self) -> ExactRow<'_> {
        if self.deleted {
            ExactRow::tombstone(&self.key)
        } else {
            ExactRow::present(&self.key, &self.value)
        }
    }

    fn lexical(&self) -> LexicalRow<'_> {
        let document = EntityDocumentId {
            artifact: EntityArtifactIdentity::Compact(ArtifactId::<
                IrFragmentEncoding,
                IrFragmentDomain,
            >::from_encoded_bytes(
                b"backend.fuzz.index-pack.document"
            )),
            entity: EntityId::new(u32::from(self.ordinal)),
        };
        if self.deleted {
            LexicalRow::tombstone(&self.key, document)
        } else {
            LexicalRow::new(
                &self.key,
                document,
                LexicalScore::from(u32::from(self.ordinal) + 1),
            )
        }
    }
}

/// One valid pack: `exact` and `lexical` segments, each lane in ascending
/// segment id order, as the decoder requires.
fn segment_pack(exact: Lane, lexical: Lane) -> Result<Vec<u8>, String> {
    let exact_cells: Vec<Vec<Cell>> = (0..exact.segments)
        .map(|segment| exact.exact_cells(segment))
        .collect();
    let lexical_cells: Vec<Vec<Cell>> = (0..lexical.segments)
        .map(|segment| lexical.lexical_cells(segment))
        .collect();
    let exact_rows: Vec<Vec<ExactRow<'_>>> = exact_cells
        .iter()
        .map(|cells| cells.iter().map(Cell::exact).collect())
        .collect();
    let lexical_rows: Vec<Vec<LexicalRow<'_>>> = lexical_cells
        .iter()
        .map(|cells| cells.iter().map(Cell::lexical).collect())
        .collect();
    let mut exact_segments = exact_rows
        .iter()
        .map(|rows| ExactSegment::new(rows).map_err(|error| format!("exact seed rows: {error:?}")))
        .collect::<Result<Vec<_>, _>>()?;
    let mut lexical_segments = lexical_rows
        .iter()
        .map(|rows| {
            LexicalSegment::new(rows).map_err(|error| format!("lexical seed rows: {error:?}"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    exact_segments.sort_by_key(|segment| *segment.id);
    lexical_segments.sort_by_key(|segment| *segment.id);
    seal(&exact_segments, &lexical_segments)
}

/// Writes the header, directory, and bodies, then opens the result.
fn seal(exact: &[ExactSegment<'_>], lexical: &[LexicalSegment<'_>]) -> Result<Vec<u8>, String> {
    let exact_ids: Vec<ExactSegmentId> = exact.iter().map(|segment| segment.id).collect();
    let lexical_ids: Vec<LexicalSegmentId> = lexical.iter().map(|segment| segment.id).collect();
    let generation = generation();
    let snapshot = IndexSnapshot::new(generation, &exact_ids, &lexical_ids)
        .map_err(|error| error.to_string())?
        .id;
    let bodies = bodies(exact, lexical)?;
    let directory_end = HEADER_BYTES + DIRECTORY_BYTES * bodies.len();
    let total = directory_end + bodies.iter().map(|(_, body)| body.len()).sum::<usize>();
    let total = u32::try_from(total).map_err(|_| "seed pack exceeds u32".to_string())?;
    let count = |lane: usize| u8::try_from(lane).map_err(|_| "too many seed segments".to_string());
    let mut bytes = header(
        &generation,
        &snapshot,
        total,
        count(exact.len())?,
        count(lexical.len())?,
    );
    let mut start = directory_end;
    for (id, body) in &bodies {
        bytes.extend_from_slice(id);
        bytes.extend_from_slice(&wire_u32(start)?);
        bytes.extend_from_slice(&wire_u32(body.len())?);
        start += body.len();
    }
    for (_, body) in &bodies {
        bytes.extend_from_slice(body);
    }
    let id = IndexPackId::from_encoded_bytes(&bytes);
    if let Err(rejected) = IndexPack::open(bytes.as_slice(), id) {
        return Err(format!(
            "segment seed was rejected by IndexPack::open: {}",
            rejected.error
        ));
    }
    Ok(bytes)
}

/// One segment's directory id and encoded body.
type Body = ([u8; 32], Vec<u8>);

/// Each segment's id and body: the exact lane, then the lexical lane.
fn bodies(exact: &[ExactSegment<'_>], lexical: &[LexicalSegment<'_>]) -> Result<Vec<Body>, String> {
    let mut bodies = Vec::with_capacity(exact.len() + lexical.len());
    for segment in exact {
        let rows = segment
            .rows
            .iter()
            .map(exact_record)
            .collect::<Result<Vec<_>, _>>()?;
        bodies.push((*segment.id, segment_body(&rows)?));
    }
    for segment in lexical {
        let rows = segment
            .rows
            .iter()
            .map(lexical_record)
            .collect::<Result<Vec<_>, _>>()?;
        bodies.push((*segment.id, segment_body(&rows)?));
    }
    Ok(bodies)
}

/// Row count, each row's offset from the body start, then the row records.
fn segment_body(rows: &[Vec<u8>]) -> Result<Vec<u8>, String> {
    let count = u16::try_from(rows.len()).map_err(|_| "too many seed rows".to_string())?;
    let mut out = count.to_le_bytes().to_vec();
    let mut offset = size_of::<u16>() + size_of::<u32>() * rows.len();
    for row in rows {
        out.extend_from_slice(&wire_u32(offset)?);
        offset += row.len();
    }
    for row in rows {
        out.extend_from_slice(row);
    }
    Ok(out)
}

/// State, key length, value length, key, value.
fn exact_record(row: &ExactRow<'_>) -> Result<Vec<u8>, String> {
    let value = row.value_bytes().unwrap_or_default();
    let mut out = vec![u8::from(!row.is_tombstone())];
    out.extend_from_slice(&wire_u32(row.key.len())?);
    out.extend_from_slice(&wire_u32(value.len())?);
    out.extend_from_slice(row.key);
    out.extend_from_slice(value);
    Ok(out)
}

/// Term length, document, state, score, term. A tombstone's score is 0.
fn lexical_record(row: &LexicalRow<'_>) -> Result<Vec<u8>, String> {
    let (state, score) = match row.value {
        LexicalRowValue::Present(score) => (1, u32::from(score)),
        LexicalRowValue::Tombstone => (0, 0),
    };
    let document: [u8; ENTITY_DOCUMENT_ID_BYTES] = row.document.into();
    let mut out = wire_u32(row.term.len())?.to_vec();
    out.extend_from_slice(&document);
    out.push(state);
    out.extend_from_slice(&score.to_le_bytes());
    out.extend_from_slice(row.term);
    Ok(out)
}

fn wire_u32(value: usize) -> Result<[u8; 4], String> {
    u32::try_from(value)
        .map(u32::to_le_bytes)
        .map_err(|_| format!("{value} does not fit a u32 pack field"))
}

fn facts(
    pack: &IndexPack<&[u8]>,
) -> (IndexPackId, GenerationId, IndexSnapshotId, usize, usize) {
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
                    return Err(OracleFailure::new(
                        "lexical term escapes the opened pack",
                    ));
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

#[cfg(test)]
mod segment_seed_files {
    use std::path::Path;

    use super::{MAX_LEN, judge, segment_seeds};

    /// Every drifted or missing seed is reported with its fresh hex, so one
    /// run is enough to replace them all.
    #[test]
    fn segment_seeds_are_the_fresh_encode() -> Result<(), String> {
        let dir = Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/targets/pack-decode/corpus"
        ));
        let mut drifted = Vec::new();
        for (name, fresh) in segment_seeds()? {
            let path = dir.join(name);
            let stored = match std::fs::read(&path) {
                Ok(stored) => stored,
                Err(error) => {
                    drifted.push(format!(
                        "reading {}: {error} (fresh {} bytes, fresh hex {})",
                        path.display(),
                        fresh.len(),
                        crate::hex_bytes(&fresh)
                    ));
                    continue;
                }
            };
            if stored != fresh {
                drifted.push(format!(
                    "{name} drifted from a fresh encode (stored {} bytes, fresh {} bytes, fresh hex {})",
                    stored.len(),
                    fresh.len(),
                    crate::hex_bytes(&fresh)
                ));
                continue;
            }
            crate::require_seed(dir, name, MAX_LEN, true, judge)?;
        }
        if drifted.is_empty() {
            Ok(())
        } else {
            Err(drifted.join("\n"))
        }
    }
}
