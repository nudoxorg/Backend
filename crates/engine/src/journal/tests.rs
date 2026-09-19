//! Adversarial tests for frame admission, recovery, and append retry.

use std::io::Write;

use super::append::resolve_append_failure;
use super::frame::encode_frame;
use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TestLog;

impl JournalDomain for TestLog {
    const DOMAIN: u8 = 0xe1;
    const TYPE: u16 = 0x1001;
    const VERSION: u8 = 1;
}

impl JournalCodec for TestLog {
    type Record = Vec<u8>;

    fn encode(record: &Self::Record, output: &mut Vec<u8>) {
        output.extend_from_slice(record);
    }

    fn decode(bytes: &[u8]) -> Result<Self::Record, JournalError> {
        Ok(bytes.to_vec())
    }
}

static NEXT_PATH: AtomicU64 = AtomicU64::new(0);

fn test_path(label: &str) -> PathBuf {
    let nonce = NEXT_PATH.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "backend-journal-{label}-{}-{nonce}.log",
        std::process::id()
    ))
}

fn limits() -> JournalLimits {
    JournalLimits {
        max_frames: 20_000,
        max_bytes: 128 * 1024 * 1024,
    }
}

fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[test]
fn stream_reuses_one_payload_slot_for_large_history() {
    let path = test_path("stream");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let payload = vec![0x5a; 16 * 1024];
    for _ in 0..2_000 {
        journal.append(&payload).expect("append");
    }
    let mut seen = 0usize;
    let scan = journal
        .scan_stream(limits(), |frame| {
            assert_eq!(frame.payload.len(), payload.len());
            seen += 1;
            Ok(())
        })
        .expect("stream");
    assert_eq!(seen, 2_000);
    assert_eq!(scan.frames_scanned, 2_000);
    assert_eq!(scan.peak_payload_bytes, payload.len());
    assert_eq!(scan.last_sequence, Some(1_999));
    remove(&path);
}

#[test]
fn checkpoint_resume_reads_only_the_verified_tail() {
    let path = test_path("checkpoint");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let mut checkpoint = None;
    for index in 0..2_000u16 {
        let receipt = journal
            .append(&index.to_be_bytes().to_vec())
            .expect("append");
        if index == 1_996 {
            checkpoint = Some(receipt.checkpoint());
        }
    }
    let checkpoint = checkpoint.expect("checkpoint");
    let mut sequences = Vec::new();
    let scan = journal
        .scan_from_checkpoint(checkpoint, limits(), |frame| {
            sequences.push(frame.sequence);
            Ok(())
        })
        .expect("tail");
    assert_eq!(sequences, vec![1_997, 1_998, 1_999]);
    assert_eq!(scan.frames_scanned, 3);
    assert!(scan.bytes_scanned <= 3 * (HEADER_BYTES as u64 + 2));
    remove(&path);
}

#[test]
fn checkpoint_open_repairs_only_a_torn_suffix() {
    let path = test_path("repair");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let checkpoint = journal.append(&b"checkpoint".to_vec()).expect("append");
    journal.append(&b"tail".to_vec()).expect("append");
    drop(journal);
    let mut file = OpenOptions::new().append(true).open(&path).expect("append");
    file.write_all(b"torn frame").expect("write");
    file.sync_all().expect("sync");
    drop(file);
    let (resumed, recovery) =
        HashChainJournal::<TestLog>::open_from_checkpoint(&path, checkpoint.checkpoint(), limits())
            .expect("resume");
    assert_eq!(recovery.frames.len(), 1);
    assert!(recovery.truncated_tail);
    let next = resumed.append(&b"after repair".to_vec()).expect("append");
    assert_eq!(next.sequence, 2);
    remove(&path);
}

#[test]
fn checkpoint_identity_and_complete_tail_are_both_authenticated() {
    let path = test_path("tamper");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let checkpoint = journal.append(&b"checkpoint".to_vec()).expect("append");
    let tail = journal.append(&b"tail".to_vec()).expect("append");
    drop(journal);

    let mut forged = checkpoint.checkpoint();
    forged.sequence = forged.sequence.saturating_add(1);
    let error = HashChainJournal::<TestLog>::open_from_checkpoint(&path, forged, limits())
        .expect_err("forged checkpoint");
    assert!(matches!(error, JournalError::Corrupt("checkpoint receipt")));

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open for tamper");
    file.seek(SeekFrom::Start(tail.offset + HEADER_BYTES as u64))
        .expect("seek");
    file.write_all(b"X").expect("tamper");
    file.sync_all().expect("sync");
    let error =
        HashChainJournal::<TestLog>::open_from_checkpoint(&path, checkpoint.checkpoint(), limits())
            .expect_err("tampered tail");
    assert!(matches!(error, JournalError::Corrupt("hash")));
    remove(&path);
}

#[test]
fn point_lookup_misses_do_not_poison_append_state() {
    let path = test_path("point-lookup");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let receipt = journal.append(&b"first".to_vec()).expect("append");

    let frame = journal
        .lookup_frame_at(receipt.offset)
        .expect("point lookup")
        .expect("frame at receipt offset");
    assert_eq!(frame.sequence, receipt.sequence);
    assert_eq!(frame.record, receipt.record);
    assert_eq!(frame.payload.as_ref(), b"first");

    let end = frame.end_offset;
    assert!(
        journal
            .lookup_frame_at(end)
            .expect("lookup at end")
            .is_none()
    );
    let next = journal
        .append(&b"second".to_vec())
        .expect("append after miss");
    assert_eq!(next.offset, end);
    remove(&path);
}

#[test]
fn point_lookup_of_present_corruption_fails_closed() {
    let path = test_path("point-lookup-corrupt");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let receipt = journal.append(&b"corrupt-me".to_vec()).expect("append");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open for tamper");
    file.seek(SeekFrom::Start(receipt.offset + HEADER_BYTES as u64))
        .expect("seek payload");
    file.write_all(b"X").expect("tamper payload");
    file.sync_all().expect("sync tamper");
    drop(file);

    assert!(matches!(
        journal.lookup_frame_at(receipt.offset),
        Err(JournalError::Corrupt("hash"))
    ));
    assert!(matches!(
        journal.append(&b"after-corruption".to_vec()),
        Err(JournalError::Corrupt("journal append state"))
    ));
    remove(&path);
}

#[test]
fn scan_limits_are_enforced_while_reading() {
    let path = test_path("limits");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    journal.append(&vec![0u8; 256]).expect("append");
    let error = journal
        .scan_stream(
            JournalLimits {
                max_frames: 100,
                max_bytes: HEADER_BYTES + 32,
            },
            |_| Ok(()),
        )
        .expect_err("byte limit");
    assert!(matches!(error, JournalError::Bounds));
    remove(&path);
}

#[test]
fn frame_budget_is_checked_before_the_next_payload_is_read() {
    let path = test_path("frame-budget");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    journal.append(&b"one".to_vec()).expect("append");
    let error = journal
        .scan_stream(
            JournalLimits {
                max_frames: 0,
                max_bytes: 1024,
            },
            |_| Ok(()),
        )
        .expect_err("frame budget");
    assert!(matches!(error, JournalError::Bounds));
    remove(&path);
}

#[test]
fn oversized_torn_payload_is_reported_without_allocating_the_declared_size() {
    let path = test_path("oversized-torn");
    let (journal, receipt) = {
        let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
        let receipt = journal.append(&b"small".to_vec()).expect("append");
        (journal, receipt)
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open for length tamper");
    file.seek(SeekFrom::Start(receipt.offset + 54))
        .expect("seek length");
    file.write_all(&(MAX_PAYLOAD as u64).to_be_bytes())
        .expect("write length");
    file.sync_all().expect("sync length");
    drop(file);

    assert!(matches!(
        read_frame_at_path::<TestLog>(&path, receipt.offset),
        Err(JournalError::Corrupt("frame payload"))
    ));
    let scan = journal
        .scan_stream(
            JournalLimits {
                max_frames: 4,
                max_bytes: 128 * 1024 * 1024,
            },
            |_| Ok(()),
        )
        .expect("torn scan");
    assert!(scan.truncated_tail);
    assert_eq!(scan.frames_scanned, 0);
    assert_eq!(scan.peak_payload_bytes, 0);
    assert_eq!(scan.valid_offset, receipt.offset);
    remove(&path);
}

#[test]
fn oversized_declared_torn_payload_is_repaired_without_allocation() {
    let path = test_path("oversized-declared-torn");
    let (journal, receipt) = {
        let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
        let receipt = journal.append(&b"small".to_vec()).expect("append");
        (journal, receipt)
    };
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open for length tamper");
    file.seek(SeekFrom::Start(receipt.offset + 54))
        .expect("seek length");
    file.write_all(&((MAX_PAYLOAD as u64) + 1).to_be_bytes())
        .expect("write length");
    file.sync_all().expect("sync length");
    drop(file);

    assert!(matches!(
        read_frame_at_path::<TestLog>(&path, receipt.offset),
        Err(JournalError::Corrupt("frame payload"))
    ));
    let scan = journal
        .scan_stream(
            JournalLimits {
                max_frames: 4,
                max_bytes: 128 * 1024 * 1024,
            },
            |_| Ok(()),
        )
        .expect("torn scan");
    assert!(scan.truncated_tail);
    assert_eq!(scan.frames_scanned, 0);
    assert_eq!(scan.peak_payload_bytes, 0);
    assert_eq!(scan.valid_offset, receipt.offset);
    drop(journal);
    let (_reopened, recovery) = HashChainJournal::<TestLog>::open(&path).expect("repair");
    assert!(recovery.truncated_tail);
    remove(&path);
}

#[test]
fn randomized_payload_history_preserves_offsets_and_chain() {
    let path = test_path("randomized");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let mut expected = Vec::new();
    let mut receipts = Vec::new();
    let mut seed = 0x9e37_79b9_u64;
    for index in 0..512u64 {
        // Deterministic pseudo-random sizes keep this test independent of
        // a random source while exercising zero, tiny, and larger frames.
        seed ^= seed << 7;
        seed ^= seed >> 9;
        seed ^= seed << 8;
        let length = usize::try_from(seed % 2049).expect("small length");
        let payload = (0..length)
            .map(|offset| {
                let offset = u64::try_from(offset).expect("small offset");
                let value = u8::try_from(seed.wrapping_add(offset) % 256).expect("byte value");
                value ^ u8::try_from(index % 256).expect("byte index")
            })
            .collect::<Vec<_>>();
        let receipt = journal.append(&payload).expect("append");
        expected.push(payload);
        receipts.push(receipt);
    }

    let mut observed = Vec::new();
    let scan = journal
        .scan_stream(
            JournalLimits {
                max_frames: expected.len() + 1,
                max_bytes: 4 * 1024 * 1024,
            },
            |frame| {
                observed.push((frame.sequence, frame.offset, frame.payload.to_vec()));
                Ok(())
            },
        )
        .expect("scan");
    assert_eq!(scan.frames_scanned, expected.len());
    assert_eq!(scan.last_sequence, Some(511));
    assert_eq!(
        scan.valid_offset,
        receipts.last().expect("last").offset
            + HEADER_BYTES as u64
            + expected.last().expect("last payload").len() as u64
    );
    for (index, ((sequence, offset, payload), expected_payload)) in
        observed.iter().zip(expected.iter()).enumerate()
    {
        assert_eq!(*sequence, index as u64);
        assert_eq!(*offset, receipts[index].offset);
        assert_eq!(payload, expected_payload);
    }
    remove(&path);
}

#[test]
fn append_rejects_an_external_length_change_until_recovery_reopens_the_cursor() {
    let path = test_path("append-position");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    journal.append(&b"first".to_vec()).expect("append");
    let mut external = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("external append");
    external.write_all(b"foreign tail").expect("write tail");
    external.sync_all().expect("sync tail");
    drop(external);
    assert!(matches!(
        journal.append(&b"second".to_vec()),
        Err(JournalError::Corrupt("journal append position"))
    ));
    drop(journal);

    // The failed append already rolled back the torn foreign suffix with
    // a verified sync, so reopen starts at the verified prefix.
    let (journal, recovery) = HashChainJournal::<TestLog>::open(&path).expect("recover");
    assert!(!recovery.truncated_tail);
    let receipt = journal
        .append(&b"second".to_vec())
        .expect("append after recovery");
    assert_eq!(receipt.sequence, 1);
    remove(&path);
}

#[test]
fn complete_frame_after_append_error_is_adopted_without_a_duplicate() {
    let path = test_path("append-retry");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let first = journal.append(&b"first".to_vec()).expect("first");
    let payload = b"second".to_vec();
    let sequence = first.sequence + 1;
    let previous = *first.chain.as_bytes();
    let digest = chain_digest::<TestLog>(sequence, &previous, &payload);
    let record = RecordId::from_payload(&payload);
    let frame = encode_frame::<TestLog>(sequence, &previous, &digest, &payload).expect("frame");
    let start = first.offset + HEADER_BYTES as u64 + 5;
    let end = start + frame.len() as u64;

    // Model a write that reached the file but whose sync result was lost
    // by writing the complete candidate behind the journal's cursor.
    let mut external = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("external append");
    external.write_all(&frame).expect("candidate");
    external.sync_all().expect("candidate sync");
    drop(external);

    let mut state = journal.state.lock().expect("state");
    let candidate = append::AppendCandidate {
        start,
        end,
        sequence,
        previous,
        digest,
        record,
        payload: &payload,
        next_sequence: sequence + 1,
    };
    let receipt = resolve_append_failure(
        &path,
        &mut state,
        &candidate,
        JournalError::Io(io::Error::other("sync result lost")),
    )
    .expect("adopt complete candidate");
    assert_eq!(receipt.sequence, sequence);
    drop(state);

    // A caller that retries after the returned failure/uncertain outcome
    // sees the exact frame at the cursor and reuses it. Only the next
    // distinct record is appended.
    let third = journal.append(&b"third".to_vec()).expect("third");
    assert_eq!(third.sequence, sequence + 1);
    let scan = journal.scan_stream(limits(), |_| Ok(())).expect("scan");
    assert_eq!(scan.frames_scanned, 3);
    remove(&path);
}

#[test]
fn retrying_an_exact_complete_tail_adopts_the_existing_frame() {
    let path = test_path("append-exact-retry");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let first = journal.append(&b"first".to_vec()).expect("first");
    let payload = b"second".to_vec();
    let sequence = first.sequence + 1;
    let previous = *first.chain.as_bytes();
    let digest = chain_digest::<TestLog>(sequence, &previous, &payload);
    let frame = encode_frame::<TestLog>(sequence, &previous, &digest, &payload).expect("frame");
    let mut external = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("external append");
    external.write_all(&frame).expect("candidate");
    external.sync_all().expect("candidate sync");
    drop(external);

    // The first append's result was lost after a complete synced write.
    // Retrying the same deterministic payload reuses that exact frame.
    let retry = journal.append(&payload).expect("adopt retry");
    assert_eq!(retry.sequence, sequence);
    assert_eq!(retry.offset, first.offset + HEADER_BYTES as u64 + 5);
    let next = journal.append(&b"third".to_vec()).expect("third");
    assert_eq!(next.sequence, sequence + 1);
    let scan = journal.scan_stream(limits(), |_| Ok(())).expect("scan");
    assert_eq!(scan.frames_scanned, 3);
    remove(&path);
}

#[test]
fn retry_receipt_adopts_a_complete_tail_after_reopen() {
    let path = test_path("append-retry-reopen");
    let (journal, first) = {
        let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
        let first = journal.append(&b"first".to_vec()).expect("first");
        (journal, first)
    };
    let payload = b"second-after-restart".to_vec();
    let sequence = first.sequence + 1;
    let previous = *first.chain.as_bytes();
    let digest = chain_digest::<TestLog>(sequence, &previous, &payload);
    let candidate =
        encode_frame::<TestLog>(sequence, &previous, &digest, &payload).expect("candidate frame");
    let offset = first.offset + HEADER_BYTES as u64 + 5;
    let receipt = JournalReceipt {
        offset,
        sequence,
        chain: ChainHash::from_bytes(digest),
        record: RecordId::from_payload(&payload),
    };
    let mut external = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("external append");
    external.write_all(&candidate).expect("candidate");
    external.sync_all().expect("candidate sync");
    drop(external);
    drop(journal);

    let (reopened, _) = HashChainJournal::<TestLog>::open(&path).expect("reopen");
    let adopted = reopened
        .retry(receipt, &payload)
        .expect("adopt after reopen");
    assert_eq!(adopted, receipt);
    let next = reopened.append(&b"third".to_vec()).expect("third");
    assert_eq!(next.sequence, sequence + 1);
    let scan = reopened.scan_stream(limits(), |_| Ok(())).expect("scan");
    assert_eq!(scan.frames_scanned, 3);
    remove(&path);
}

#[test]
fn retry_receipt_reappends_after_an_uncertain_candidate_was_rolled_back() {
    let path = test_path("append-retry-absent");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let first = journal.append(&b"first".to_vec()).expect("first");
    drop(journal);
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("reopen");
    let payload = b"second-after-rollback".to_vec();
    let sequence = first.sequence + 1;
    let previous = *first.chain.as_bytes();
    let digest = chain_digest::<TestLog>(sequence, &previous, &payload);
    let receipt = JournalReceipt {
        offset: first.offset + HEADER_BYTES as u64 + 5,
        sequence,
        chain: ChainHash::from_bytes(digest),
        record: RecordId::from_payload(&payload),
    };
    let adopted = journal.retry(receipt, &payload).expect("retry absent");
    assert_eq!(adopted, receipt);
    assert_eq!(
        journal
            .scan_stream(limits(), |_| Ok(()))
            .expect("scan")
            .frames_scanned,
        2
    );
    remove(&path);
}

#[test]
fn complete_competing_frame_is_not_destroyed_by_append_resolution() {
    let path = test_path("append-conflict");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let first = journal.append(&b"first".to_vec()).expect("first");
    let intended = b"a deliberately longer candidate".to_vec();
    let sequence = first.sequence + 1;
    let previous = *first.chain.as_bytes();
    let foreign = b"foreign".to_vec();
    let foreign_digest = chain_digest::<TestLog>(sequence, &previous, &foreign);
    let foreign_frame = encode_frame::<TestLog>(sequence, &previous, &foreign_digest, &foreign)
        .expect("foreign frame");
    let mut external = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("external append");
    external.write_all(&foreign_frame).expect("foreign frame");
    external.sync_all().expect("foreign sync");
    drop(external);

    let error = journal.append(&intended).expect_err("conflicting append");
    assert!(matches!(
        error,
        JournalError::Corrupt("journal append conflict")
    ));
    drop(journal);
    let (_reopened, recovery) = HashChainJournal::<TestLog>::open(&path).expect("reopen");
    assert_eq!(recovery.frames.len(), 2);
    assert_eq!(recovery.frames[1].payload.as_ref(), foreign.as_slice());
    remove(&path);
}

#[test]
fn partial_frame_after_write_error_is_rolled_back_before_retry() {
    let path = test_path("append-short-write");
    let (journal, _) = HashChainJournal::<TestLog>::open(&path).expect("open");
    let first = journal.append(&b"first".to_vec()).expect("first");
    let payload = b"second".to_vec();
    let sequence = first.sequence + 1;
    let previous = *first.chain.as_bytes();
    let digest = chain_digest::<TestLog>(sequence, &previous, &payload);
    let record = RecordId::from_payload(&payload);
    let frame = encode_frame::<TestLog>(sequence, &previous, &digest, &payload).expect("frame");
    let start = first.offset + HEADER_BYTES as u64 + 5;
    let end = start + frame.len() as u64;

    let mut external = OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("external append");
    external
        .write_all(&frame[..frame.len() / 2])
        .expect("short candidate");
    external.sync_all().expect("candidate sync");
    drop(external);

    let mut state = journal.state.lock().expect("state");
    let candidate = append::AppendCandidate {
        start,
        end,
        sequence,
        previous,
        digest,
        record,
        payload: &payload,
        next_sequence: sequence + 1,
    };
    let error = resolve_append_failure(
        &path,
        &mut state,
        &candidate,
        JournalError::Io(io::Error::new(io::ErrorKind::WriteZero, "short write")),
    )
    .expect_err("short candidate must be absent after rollback");
    assert!(matches!(error, JournalError::Io(_)));
    drop(state);

    let retry = journal.append(&payload).expect("retry");
    assert_eq!(retry.sequence, sequence);
    let scan = journal.scan_stream(limits(), |_| Ok(())).expect("scan");
    assert_eq!(scan.frames_scanned, 2);
    remove(&path);
}
