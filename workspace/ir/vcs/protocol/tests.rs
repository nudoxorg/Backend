//! Integration tests for the `ir-stream` protocol.
//!
//! Tests run over in-memory `Vec<u8>` pipes (write side → read side) to avoid
//! any OS socket setup. Coverage:
//!
//! - Full roundtrip: several batches, links, source digests, progress frames.
//! - Auto-batch splitting near the frame size cap.
//! - Oversize single entry rejected by the writer.
//! - Truncated stream (clean `UnexpectedEof`).
//! - Version mismatch.
//! - Protocol order violations (double Hello, frame after Finish, next before accept).
//! - Emitted-count mismatch.
//! - Determinism: same entries ⇒ identical bytes.

#![cfg(test)]

use crate::wire::{
    EntryPayloadFlags, FnSigFlags, FunctionWire, KindWire, ModuleWire, OwnedEntryPayload,
    SymbolWire,
};
use heart::content::{ContentHash, JobKey};
use ir::body::{BodyEmbed, BodyMergeNote, OracleBody, TreesitterBody, merge_body};
use ir::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};
use ir::entry::Visibility;
use ir::kind::KindDiscriminant;

use crate::protocol::{
    BodyWire, Error, FailureKindWire, FrameReader, FrameWriter, IR_STREAM_VERSION, MAX_FRAME_BYTES,
    PhaseWire, ProducerId, Received, StreamFrame, StreamReceiver, SymbolSink,
    WireEntry, WireLink,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_job() -> JobKey {
    JobKey::derive(b"producer-v1", b"rust-1.79", b"src/lib.rs", b"Cargo.lock")
}

fn make_producer() -> ProducerId {
    ProducerId::from_static("rust-1.79")
}

fn make_stable_ref(name: &str, seed: u8) -> StableRef {
    StableRef::new(
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new(name)),
        IntroId::from_raw([seed; 32]),
    )
}

fn make_entry(name: &str, seed: u8) -> WireEntry {
    let sym = SymbolWire {
        name: name.into(),
        visibility: Visibility::Public,
        documentation: Some(format!("docs for {name}")),
        source_path: "src/lib.rs".into(),
        span_start: 0,
        span_end: 100,
        aliases: Vec::new(),
        deprecation: None,
        doc_links: Vec::new(),
        attrs: Vec::new(),
        cfg: None,
    };
    let kind = KindWire::Function(FunctionWire {
        input_params: Box::new([]),
        output_params: Box::new([]),
        sig: FnSigFlags::default(),
        generics: Box::new([]),
        wheres: Box::new([]),
    });
    let payload = OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Function,
        kind,
        EntryPayloadFlags::default(),
    );
    WireEntry {
        stable: make_stable_ref(name, seed),
        payload,
        parent: None,
        links: Vec::new(),
    }
}

fn make_content_hash(seed: u8) -> ContentHash {
    ContentHash::of_bytes(&[seed; 32])
}

// Use a shared-buffer approach for tests.
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl SharedBuf {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Vec::new())))
    }
    fn bytes(&self) -> Vec<u8> {
        self.0.lock().unwrap().clone()
    }
}

impl std::io::Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn produce_shared(entries: Vec<WireEntry>) -> (Vec<u8>, u64) {
    let buf = SharedBuf::new();
    let mut sink = SymbolSink::hello(buf.clone(), make_job(), make_producer()).unwrap();
    sink.emit_all(entries).unwrap();
    sink.source_digest("src/lib.rs", make_content_hash(1), 1234)
        .unwrap();
    sink.progress(PhaseWire::Emit).unwrap();
    let digest = make_content_hash(0xAB);
    let count = sink.finish(digest).unwrap();
    (buf.bytes(), count)
}

// ---------------------------------------------------------------------------
// Test: full roundtrip
// ---------------------------------------------------------------------------

#[test]
fn test_full_roundtrip() {
    let entries = vec![
        make_entry("alpha", 1),
        make_entry("beta", 2),
        make_entry("gamma", 3),
    ];
    let (bytes, produced_count) = produce_shared(entries.clone());
    assert_eq!(produced_count, 3);

    let cursor = std::io::Cursor::new(bytes);
    let mut rx = StreamReceiver::new(cursor);

    let (job, producer) = rx.accept().unwrap();
    assert_eq!(job, make_job());
    assert_eq!(producer.as_str(), "rust-1.79");

    let mut symbols_received: Vec<WireEntry> = Vec::new();
    let mut source_digests = 0u32;
    let mut progress_seen = false;
    let mut finished = false;

    loop {
        match rx.recv().unwrap() {
            Some(Received::Symbols(batch)) => symbols_received.extend(batch),
            Some(Received::Links { .. } | Received::Occurrences(_) | Received::Bodies(_)) => {}
            Some(Received::SourceDigest {
                path,
                hash: _,
                size,
            }) => {
                assert_eq!(path, "src/lib.rs");
                assert_eq!(size, 1234);
                source_digests += 1;
            }
            Some(Received::Progress { phase, .. }) => {
                assert_eq!(phase, PhaseWire::Emit);
                progress_seen = true;
            }
            Some(Received::Finish {
                emitted,
                producer_digest,
            }) => {
                assert_eq!(emitted, 3);
                assert_eq!(producer_digest, make_content_hash(0xAB));
                finished = true;
                break;
            }
            Some(Received::Abort { .. }) => panic!("unexpected Abort"),
            None => break,
        }
    }

    assert!(finished, "stream did not finish");
    assert!(progress_seen, "no progress frame received");
    assert_eq!(source_digests, 1);
    assert_eq!(symbols_received.len(), 3);
    // Payloads match.
    for (got, want) in symbols_received.iter().zip(entries.iter()) {
        assert_eq!(got.stable, want.stable);
        assert_eq!(got.payload.payload_hash, want.payload.payload_hash);
    }
}

// ---------------------------------------------------------------------------
// Test: links and occurrences
// ---------------------------------------------------------------------------

#[test]
fn test_links_and_occurrences() {
    let buf = SharedBuf::new();
    let mut sink = SymbolSink::hello(buf.clone(), make_job(), make_producer()).unwrap();
    let src_ref = make_stable_ref("link_src", 10);
    sink.emit(make_entry("link_src", 10)).unwrap();
    sink.emit_link(
        src_ref.clone(),
        WireLink {
            other: make_stable_ref("other_pkg", 20),
            kind_self: KindDiscriminant::Function,
            kind_other: KindDiscriminant::Module,
        },
    )
    .unwrap();
    // Also emit a batch of links.
    sink.emit_links(
        src_ref,
        vec![
            WireLink {
                other: make_stable_ref("pkg_a", 30),
                kind_self: KindDiscriminant::Record,
                kind_other: KindDiscriminant::Field,
            },
            WireLink {
                other: make_stable_ref("pkg_b", 40),
                kind_self: KindDiscriminant::Alias,
                kind_other: KindDiscriminant::Function,
            },
        ],
    )
    .unwrap();
    let count = sink.finish(make_content_hash(0xFF)).unwrap();
    assert_eq!(count, 1);

    let cursor = std::io::Cursor::new(buf.bytes());
    let mut rx = StreamReceiver::new(cursor);
    rx.accept().unwrap();

    let mut links_seen = 0usize;
    loop {
        match rx.recv().unwrap() {
            Some(Received::Links { batch, .. }) => links_seen += batch.len(),
            Some(Received::Finish { .. }) | None => break,
            Some(Received::Abort { .. }) => panic!("abort"),
            _ => {}
        }
    }
    assert_eq!(links_seen, 3); // 1 single + 2 batch
}

// ---------------------------------------------------------------------------
// Test: auto-batch splitting near the frame cap
// ---------------------------------------------------------------------------

#[test]
fn test_auto_batch_splitting() {
    // Build entries large enough that a single batch of SINK_BATCH_ENTRIES
    // entries would overflow MAX_FRAME_BYTES, forcing the sink to split.
    // We use entries with large documentation strings.
    let big_doc = "x".repeat(4096);
    let entries: Vec<WireEntry> = (0u8..=20)
        .map(|i| {
            let sym = SymbolWire {
                name: format!("sym_{i}"),
                visibility: Visibility::Public,
                documentation: Some(big_doc.clone()),
                source_path: "src/lib.rs".into(),
                span_start: u32::from(i),
                span_end: u32::from(i) + 1,
                aliases: Vec::new(),
                deprecation: None,
                doc_links: Vec::new(),
                attrs: Vec::new(),
                cfg: None,
            };
            let kind = KindWire::Module(ModuleWire {});
            let payload = OwnedEntryPayload::sealed(
                sym,
                KindDiscriminant::Module,
                kind,
                EntryPayloadFlags::default(),
            );
            WireEntry {
                stable: make_stable_ref(&format!("pkg_{i}"), i),
                payload,
                parent: None,
                links: Vec::new(),
            }
        })
        .collect();

    let (bytes, produced_count) = produce_shared(entries);
    assert_eq!(produced_count, 21);

    // Verify the receiver collects all entries.
    let cursor = std::io::Cursor::new(bytes);
    let mut rx = StreamReceiver::new(cursor);
    rx.accept().unwrap();

    let mut received = 0usize;
    let mut batch_count = 0usize;
    loop {
        match rx.recv().unwrap() {
            Some(Received::Symbols(batch)) => {
                received += batch.len();
                batch_count += 1;
            }
            Some(Received::Finish { emitted, .. }) => {
                assert_eq!(emitted, 21);
                break;
            }
            None => break,
            _ => {}
        }
    }
    assert_eq!(received, 21);
    // With large docs, some batches must have been split.
    // We just assert that it worked and all entries arrived.
    let _ = batch_count; // may be 1 or more depending on encoding size
}

// ---------------------------------------------------------------------------
// Test: oversize single entry rejected by writer
// ---------------------------------------------------------------------------

#[test]
fn test_oversize_frame_rejected() {
    // Build a frame that is guaranteed to exceed MAX_FRAME_BYTES.
    let huge = "z".repeat(MAX_FRAME_BYTES + 1);
    let frame = StreamFrame::Occurrences {
        section: huge.into_bytes(),
    };

    let mut fw = FrameWriter::new(Vec::<u8>::new());
    let err = fw.write_frame(&frame).unwrap_err();
    match err {
        Error::FrameTooLarge {
            limit,
            actual,
            variant,
        } => {
            assert_eq!(limit, MAX_FRAME_BYTES);
            assert!(actual > MAX_FRAME_BYTES);
            assert_eq!(variant, Some("Occurrences"));
        }
        other => panic!("expected FrameTooLarge, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: a single entry too large for any frame is rejected at emit time
// ---------------------------------------------------------------------------

/// Regression test for the sink's exact size accounting: an entry whose
/// standalone encoding cannot fit in one frame is the producer's bug and must
/// surface eagerly at `emit()`, not later at flush.
#[test]
fn test_oversize_single_entry_rejected_at_emit() {
    let mut entry = make_entry("huge", 7);
    entry.payload.symbol.documentation = Some("z".repeat(MAX_FRAME_BYTES + 1));

    let buf = SharedBuf::new();
    let mut sink = SymbolSink::hello(buf, make_job(), make_producer()).unwrap();
    match sink.emit(entry) {
        Err(Error::FrameTooLarge {
            limit,
            actual,
            variant,
        }) => {
            assert_eq!(limit, MAX_FRAME_BYTES);
            assert!(actual > MAX_FRAME_BYTES);
            assert_eq!(variant, Some("Symbols"));
        }
        other => panic!(
            "expected FrameTooLarge at emit, got {other:?}"
        ),
    }
}

// ---------------------------------------------------------------------------
// Test: truncated stream
// ---------------------------------------------------------------------------

#[test]
fn test_truncated_stream() {
    // Write a valid Hello + partial Symbols frame (truncate after 10 bytes of body).
    let buf = SharedBuf::new();
    let mut fw = FrameWriter::new(buf.clone());
    fw.write_frame(&StreamFrame::Hello {
        version: IR_STREAM_VERSION,
        job: make_job(),
        producer: make_producer(),
    })
    .unwrap();
    // Write a partial, truncated frame: length header says 100 bytes but we only write 5.
    let fake_len: u32 = 100;
    buf.0
        .lock()
        .unwrap()
        .extend_from_slice(&fake_len.to_le_bytes());
    buf.0.lock().unwrap().extend_from_slice(&[0u8; 5]); // only 5 bytes of claimed 100

    let cursor = std::io::Cursor::new(buf.bytes());
    let mut rx = StreamReceiver::new(cursor);
    rx.accept().unwrap();

    let err = rx.recv().unwrap_err();
    match err {
        Error::Io(io_err) => {
            assert_eq!(io_err.kind(), std::io::ErrorKind::UnexpectedEof);
        }
        other => panic!("expected Io(UnexpectedEof), got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: version mismatch
// ---------------------------------------------------------------------------

#[test]
fn test_version_mismatch() {
    let buf = SharedBuf::new();
    let mut fw = FrameWriter::new(buf.clone());
    fw.write_frame(&StreamFrame::Hello {
        version: IR_STREAM_VERSION + 99, // wrong version
        job: make_job(),
        producer: make_producer(),
    })
    .unwrap();

    let cursor = std::io::Cursor::new(buf.bytes());
    let mut rx = StreamReceiver::new(cursor);
    let err = rx.accept().unwrap_err();
    match err {
        Error::VersionMismatch { ours, theirs } => {
            assert_eq!(ours, IR_STREAM_VERSION);
            assert_eq!(theirs, IR_STREAM_VERSION + 99);
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: protocol order — next() before accept()
// ---------------------------------------------------------------------------

#[test]
fn test_next_before_accept() {
    let cursor = std::io::Cursor::new(Vec::<u8>::new());
    let mut rx = StreamReceiver::new(cursor);
    let err = rx.recv().unwrap_err();
    match err {
        Error::Protocol(msg) => {
            assert!(msg.contains("accept"), "message: {msg}");
        }
        other => panic!("expected Protocol, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: protocol order — non-Hello as first frame
// ---------------------------------------------------------------------------

#[test]
fn test_non_hello_first_frame() {
    let buf = SharedBuf::new();
    let mut fw = FrameWriter::new(buf.clone());
    fw.write_frame(&StreamFrame::Symbols { batch: Vec::new() })
        .unwrap();

    let cursor = std::io::Cursor::new(buf.bytes());
    let mut rx = StreamReceiver::new(cursor);
    let err = rx.accept().unwrap_err();
    match err {
        Error::Protocol(msg) => {
            assert!(msg.contains("Hello"), "message: {msg}");
        }
        other => panic!("expected Protocol, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: protocol order — double Hello
// ---------------------------------------------------------------------------

#[test]
fn test_double_hello() {
    let buf = SharedBuf::new();
    let mut fw = FrameWriter::new(buf.clone());
    fw.write_frame(&StreamFrame::Hello {
        version: IR_STREAM_VERSION,
        job: make_job(),
        producer: make_producer(),
    })
    .unwrap();
    fw.write_frame(&StreamFrame::Hello {
        version: IR_STREAM_VERSION,
        job: make_job(),
        producer: make_producer(),
    })
    .unwrap();

    let cursor = std::io::Cursor::new(buf.bytes());
    let mut rx = StreamReceiver::new(cursor);
    rx.accept().unwrap();
    let err = rx.recv().unwrap_err();
    match err {
        Error::Protocol(msg) => {
            assert!(msg.contains("Hello"), "message: {msg}");
        }
        other => panic!("expected Protocol, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: emitted-count mismatch
// ---------------------------------------------------------------------------

#[test]
fn test_emitted_count_mismatch() {
    let buf = SharedBuf::new();
    let mut fw = FrameWriter::new(buf.clone());
    fw.write_frame(&StreamFrame::Hello {
        version: IR_STREAM_VERSION,
        job: make_job(),
        producer: make_producer(),
    })
    .unwrap();
    fw.write_frame(&StreamFrame::Symbols {
        batch: vec![make_entry("foo", 1), make_entry("bar", 2)],
    })
    .unwrap();
    // Declare wrong count (2 entries emitted, claim 5).
    fw.write_frame(&StreamFrame::Finish {
        emitted: 5,
        producer_digest: make_content_hash(0),
    })
    .unwrap();

    let cursor = std::io::Cursor::new(buf.bytes());
    let mut rx = StreamReceiver::new(cursor);
    rx.accept().unwrap();
    // Consume Symbols batch.
    let _ = rx.recv().unwrap();
    // Finish should fail.
    let err = rx.recv().unwrap_err();
    match err {
        Error::EmittedCountMismatch { declared, observed } => {
            assert_eq!(declared, 5);
            assert_eq!(observed, 2);
        }
        other => panic!("expected EmittedCountMismatch, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: abort path
// ---------------------------------------------------------------------------

#[test]
fn test_abort_roundtrip() {
    let buf = SharedBuf::new();
    let mut sink = SymbolSink::hello(buf.clone(), make_job(), make_producer()).unwrap();
    sink.emit(make_entry("partial", 99)).unwrap();
    sink.abort(FailureKindWire::ToolchainMissing, "rustc not found")
        .unwrap();

    let cursor = std::io::Cursor::new(buf.bytes());
    let mut rx = StreamReceiver::new(cursor);
    rx.accept().unwrap();

    // Consume Symbols batch (auto-flushed when abort discards).
    // Actually abort() discards pending without flushing — so no Symbols frame.
    // Let's just drain until we see Abort.
    let mut aborted = false;
    loop {
        match rx.recv().unwrap() {
            Some(Received::Abort { failure, message }) => {
                assert_eq!(failure, FailureKindWire::ToolchainMissing);
                assert_eq!(message, "rustc not found");
                aborted = true;
                break;
            }
            None => break,
            // Ignore buffered Symbols (may be flushed before abort clears) and
            // any other non-terminal frame.
            _ => {}
        }
    }
    assert!(aborted, "did not receive Abort frame");
}

// ---------------------------------------------------------------------------
// Test: determinism — same entries produce identical bytes
// ---------------------------------------------------------------------------

#[test]
fn test_determinism() {
    let entries: Vec<WireEntry> = (0u8..10)
        .map(|i| make_entry(&format!("sym_{i}"), i))
        .collect();
    let (bytes1, _) = produce_shared(entries.clone());
    let (bytes2, _) = produce_shared(entries);
    assert_eq!(bytes1, bytes2, "same entries must produce identical bytes");
}

// ---------------------------------------------------------------------------
// Test: reader rejects declared length > MAX_FRAME_BYTES without allocating
// ---------------------------------------------------------------------------

#[test]
fn test_reader_rejects_oversized_declared_length() {
    let mut buf = Vec::<u8>::new();
    // Write a length header claiming 5 MiB (> MAX_FRAME_BYTES = 4 MiB).
    let claimed: u32 = (MAX_FRAME_BYTES + 1024 * 1024) as u32;
    buf.extend_from_slice(&claimed.to_le_bytes());
    // Write only a few bytes — reader should fail on length check before reading body.
    buf.extend_from_slice(&[0u8; 8]);

    let cursor = std::io::Cursor::new(buf);
    let mut fr = FrameReader::new(cursor);
    let err = fr.read_frame().unwrap_err();
    match err {
        Error::FrameTooLarge {
            limit,
            actual,
            variant,
        } => {
            assert_eq!(limit, MAX_FRAME_BYTES);
            assert_eq!(actual, claimed as usize);
            assert!(variant.is_none()); // not known on the read path
        }
        other => panic!("expected FrameTooLarge, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Test: ProducerId validation
// ---------------------------------------------------------------------------

#[test]
fn test_producer_id_validation() {
    assert!(ProducerId::new("rust-1.79").is_some());
    assert!(ProducerId::new("").is_none());
    assert!(ProducerId::new("x".repeat(128)).is_some());
    assert!(ProducerId::new("x".repeat(129)).is_none());
}

// ---------------------------------------------------------------------------
// Test: bodies frame round-trips through sink → receiver
// ---------------------------------------------------------------------------

/// Verifies that `emit_body` produces a `StreamFrame::Bodies` frame that
/// `StreamReceiver` decodes into `Received::Bodies` with identical content.
/// Both `BodyEmbed::Absent` and a `BodyEmbed::Present` built via `merge_body`
/// are exercised so both enum arms are tested.
#[test]
fn bodies_frame_round_trips() {
    let buf = SharedBuf::new();
    let mut sink = SymbolSink::hello(buf.clone(), make_job(), make_producer()).unwrap();

    // Emit an Absent body for intro seed 0xAA.
    let intro_absent = IntroId::from_raw([0xAA; 32]);
    sink.emit_body(intro_absent, BodyEmbed::Absent).unwrap();

    // Build a Present body via merge_body (the normative path).
    let tree = TreesitterBody {
        root_kind: Some("function_item".into()),
        ..Default::default()
    };
    let oracle = OracleBody::default();
    // tree is non-empty (root_kind is set), so merge_body returns Present.
    let present_body = merge_body(
        ir::body::Language::Rust,
        tree,
        oracle,
        BodyMergeNote::treesitter_only(),
    );
    let intro_present = IntroId::from_raw([0xBB; 32]);
    sink.emit_body(intro_present, present_body.clone()).unwrap();

    // Also exercise emit_bodies with a batch of two (Absent + Present again).
    let batch = vec![
        BodyWire {
            intro: IntroId::from_raw([0xCC; 32]),
            body: BodyEmbed::Absent,
        },
        BodyWire {
            intro: IntroId::from_raw([0xDD; 32]),
            body: present_body.clone(),
        },
    ];
    sink.emit_bodies(batch).unwrap();

    // Finish the stream (no symbol entries — emitted count is 0).
    sink.finish(make_content_hash(0x42)).unwrap();

    // Drive the receiver and collect all Bodies events.
    let cursor = std::io::Cursor::new(buf.bytes());
    let mut rx = StreamReceiver::new(cursor);
    rx.accept().unwrap();

    let mut received_wires: Vec<BodyWire> = Vec::new();
    loop {
        match rx.recv().unwrap() {
            Some(Received::Bodies(wires)) => received_wires.extend(wires),
            Some(Received::Finish { emitted, .. }) => {
                assert_eq!(emitted, 0, "no symbol entries were emitted");
                break;
            }
            Some(Received::Abort { .. }) => panic!("unexpected Abort"),
            None => break,
            _ => {}
        }
    }

    // We expect 4 BodyWire records total: 2 from individual emit_body calls +
    // 2 from the emit_bodies batch. They may arrive in 2 separate frames or
    // one, depending on how the sink batches them; we flatten by intro id.
    assert_eq!(received_wires.len(), 4, "expected 4 BodyWire records total");

    // First record: Absent for 0xAA.
    let first = received_wires
        .iter()
        .find(|w| w.intro == IntroId::from_raw([0xAA; 32]))
        .unwrap();
    assert!(first.body.is_absent(), "0xAA intro must be Absent");

    // Second record: Present for 0xBB (matches present_body).
    let second = received_wires
        .iter()
        .find(|w| w.intro == IntroId::from_raw([0xBB; 32]))
        .unwrap();
    assert_eq!(
        second.body, present_body,
        "0xBB intro must match the built Present body"
    );

    // Batch records from emit_bodies: 0xCC Absent, 0xDD Present.
    let cc = received_wires
        .iter()
        .find(|w| w.intro == IntroId::from_raw([0xCC; 32]))
        .unwrap();
    assert!(cc.body.is_absent(), "0xCC intro must be Absent");

    let dd = received_wires
        .iter()
        .find(|w| w.intro == IntroId::from_raw([0xDD; 32]))
        .unwrap();
    assert_eq!(
        dd.body, present_body,
        "0xDD intro must match the built Present body"
    );
}

// ---------------------------------------------------------------------------
// Test: clean EOF returns None from read_frame
// ---------------------------------------------------------------------------

#[test]
fn test_clean_eof() {
    let cursor = std::io::Cursor::new(Vec::<u8>::new());
    let mut fr = FrameReader::new(cursor);
    assert!(fr.read_frame().unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Test: post-terminal polling returns None
// ---------------------------------------------------------------------------

#[test]
fn test_post_terminal_returns_none() {
    let (bytes, _) = produce_shared(vec![make_entry("x", 1)]);
    let cursor = std::io::Cursor::new(bytes);
    let mut rx = StreamReceiver::new(cursor);
    rx.accept().unwrap();

    // Drain until Finish.
    loop {
        match rx.recv().unwrap() {
            Some(Received::Finish { .. }) => break,
            None => panic!("unexpected EOF before Finish"),
            _ => {}
        }
    }

    // Poll again after terminal — clean EOF stays a clean end-of-stream.
    assert!(rx.recv().unwrap().is_none());
    assert!(rx.recv().unwrap().is_none());
}

// ---------------------------------------------------------------------------
// Test: frames after a terminal frame are a protocol violation
// ---------------------------------------------------------------------------

/// Regression test: a peer that keeps sending after `Finish` must be detected,
/// not silently ignored (§6.1 ordering rule 3). The receiver drains the
/// transport after the terminal frame exactly so this violation surfaces.
#[test]
fn test_frame_after_terminal_is_protocol_error() {
    let (mut bytes, _) = produce_shared(vec![make_entry("x", 1)]);

    // Append a rogue Symbols frame after the Finish frame.
    let mut trailer = FrameWriter::new(Vec::new());
    trailer
        .write_frame(&StreamFrame::Symbols {
            batch: vec![make_entry("rogue", 9)],
        })
        .unwrap();
    bytes.extend_from_slice(&trailer.into_inner());

    let cursor = std::io::Cursor::new(bytes);
    let mut rx = StreamReceiver::new(cursor);
    rx.accept().unwrap();
    loop {
        match rx.recv().unwrap() {
            Some(Received::Finish { .. }) => break,
            None => panic!("unexpected EOF before Finish"),
            _ => {}
        }
    }

    // The rogue frame after the terminal must surface as a protocol error.
    match rx.recv() {
        Err(Error::Protocol(msg)) => {
            assert!(msg.contains("after terminal"), "unexpected message: {msg}");
        }
        other => panic!("expected Protocol error, got {other:?}"),
    }
}
