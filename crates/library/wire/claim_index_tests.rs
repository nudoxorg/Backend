//! Certificate admission cost and exactness.
//!
//! `live_frame_admission_cost` is a measurement, not a gate: it admits the
//! view of a real journal frame named by `NUDOX_LIVE_VIEW_FRAME` (the
//! desktop fixture's 10,460-row frame; never pinned in the repository) and
//! prints the parse and admission times beside the admitted root, so a
//! before/after pair can be compared for both time and result.

#![allow(clippy::expect_used, clippy::print_stderr)]

use super::event::ViewEnvelopeWire;
use super::reply::view_root_from_wire_with_admission;
use super::reply_admission::VerifierAdmission;
use backend_version::{
    ProducerObservationClaims, ProducerObservationVerifier, UntrustedProducerObservation,
};

/// Accepts exactly the observation a certificate carries: the frame was
/// written by an admitted owner, and this measures admission, not authority.
struct Recorded;

impl ProducerObservationVerifier for Recorded {
    type Error = &'static str;

    fn verify(
        &self,
        observation: &UntrustedProducerObservation,
    ) -> Result<ProducerObservationClaims, Self::Error> {
        Ok(ProducerObservationClaims::new(
            observation.producer_identity(),
            observation.scope_root(),
            observation.context(),
            *blake3::hash(observation.evidence()).as_bytes(),
        ))
    }
}

#[test]
#[ignore = "measurement over a local journal frame; set NUDOX_LIVE_VIEW_FRAME"]
fn live_frame_admission_cost() {
    let Some(path) = std::env::var_os("NUDOX_LIVE_VIEW_FRAME") else {
        eprintln!("NUDOX_LIVE_VIEW_FRAME is not set");
        return;
    };
    let bytes = std::fs::read(&path).expect("live view frame");
    for run in 0..2 {
        let parsing = std::time::Instant::now();
        let envelope: ViewEnvelopeWire = serde_json::from_slice(&bytes).expect("view envelope");
        let parsed = parsing.elapsed();
        let certificate = envelope.certificate.as_ref().expect("certificate");
        let admitting = std::time::Instant::now();
        let view = view_root_from_wire_with_admission(
            &envelope.snapshot.root,
            certificate,
            &VerifierAdmission(&Recorded),
            false,
        )
        .expect("admitted view");
        let admitted = admitting.elapsed();
        eprintln!(
            "live frame run {run}: {} bytes, {} claims, {} rows, parse {:.1} ms, admit {:.1} ms, root {:?}",
            bytes.len(),
            certificate.claims.len(),
            view.row_count(),
            parsed.as_secs_f64() * 1e3,
            admitted.as_secs_f64() * 1e3,
            view.root(),
        );
    }
}

const ROWS: usize = 60;

fn address(row: usize) -> String {
    format!("pkg::src/lib.rs:{row}::item{row}")
}

fn key_claim(row: usize) -> crate::WireClaim {
    crate::WireClaim::Key {
        schema: crate::WireSchema::Symbol,
        id: crate::encode_id(crate::canonical::symbol_key(&address(row)).as_bytes()),
        value: address(row),
    }
}

/// A complete 60-row view: its certificate carries 68 claims, past the
/// size where lookups stop scanning.
fn small_frame() -> crate::ViewRoot {
    let source_root = crate::canonical::view_state_root(&[]);
    let basis = crate::Basis::new(source_root, crate::canonical::object_version(b"source"));
    let rows = (0..ROWS)
        .map(|row| {
            crate::Row::new(
                crate::RowId::Symbol(crate::canonical::symbol_key(&address(row))),
                basis,
                address(row),
            )
        })
        .collect();
    crate::ViewRoot::new_checked(
        crate::canonical::view_key(b"view"),
        basis,
        crate::Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
        rows,
        vec![crate::Coverage::Complete],
        super::tests::capability(basis.object),
    )
    .expect("small frame")
}

/// Six certificates over the same frame: one admissible, five that must
/// fail for a claim-level reason (duplicate, conflict, absence, mixing,
/// wrong schema).
fn variants(root: &crate::ViewRoot) -> Vec<(&'static str, crate::WireCertificate)> {
    let all = |skip: Option<usize>| {
        (0..ROWS)
            .filter(|row| Some(*row) != skip)
            .fold(super::tests::certificate(root), |certificate, row| {
                certificate.with_claim(key_claim(row))
            })
    };
    let mut wrong_schema = all(Some(9));
    wrong_schema = wrong_schema.with_claim(crate::WireClaim::Key {
        schema: crate::WireSchema::Package,
        id: crate::encode_id(crate::canonical::symbol_key(&address(9)).as_bytes()),
        value: address(9),
    });
    vec![
        ("valid", all(None)),
        ("duplicate", all(None).with_claim(key_claim(17))),
        (
            "conflict",
            all(None).with_claim(crate::WireClaim::Key {
                schema: crate::WireSchema::Symbol,
                id: crate::encode_id(crate::canonical::symbol_key(&address(17)).as_bytes()),
                value: "forged".to_owned(),
            }),
        ),
        ("missing", all(Some(42))),
        (
            "mixed",
            all(None).with_claim(crate::WireClaim::RowIdentity {
                schema: crate::WireSchema::Symbol,
                id: crate::encode_id(crate::canonical::symbol_key(&address(5)).as_bytes()),
                preimage: address(5),
            }),
        ),
        ("wrong-schema", wrong_schema),
    ]
}

fn outcome(root: &crate::ViewRoot, certificate: crate::WireCertificate) -> String {
    let view = crate::ViewDto::new(
        7,
        crate::ViewSnapshot {
            root: root.clone(),
            freshness: crate::Freshness::Current,
            next: None,
            graph_relations: None,
            rich_graph: None,
        },
    )
    .with_certificate(certificate);
    let encoded = serde_json::to_vec(&view).expect("encode small frame");
    match crate::ViewDto::decode_with_certificate(
        &encoded,
        Some(super::tests::capability(root.basis().object)),
    ) {
        Ok(view) => format!(
            "ok root={:?} rows={}",
            view.snapshot.root.root(),
            view.snapshot.root.row_count()
        ),
        Err(error) => format!("err {error}"),
    }
}

/// The exact admission outcome of each variant, pinned from the scanning
/// implementation before the index existed.
const GOLDEN: [(&str, &str); 6] = [
    (
        "valid",
        "ok root=StateRoot:4c72cd290aa638b2c51adb49fb3ba28a542bdb16d8163539e3e7df67a667a9fe rows=60",
    ),
    (
        "duplicate",
        "err view row: identity: missing producer key commitment",
    ),
    (
        "conflict",
        "err view row: identity: missing producer key commitment",
    ),
    (
        "missing",
        "err view row: identity: missing producer key commitment",
    ),
    (
        "mixed",
        "err view row: identity: row identity certificate mixes explicit and ordinary claims",
    ),
    (
        "wrong-schema",
        "err view row: identity: missing producer key commitment",
    ),
];

#[test]
fn small_frame_admission_matches_its_pinned_golden() {
    let root = small_frame();
    let actual = variants(&root)
        .into_iter()
        .map(|(name, certificate)| {
            let claims = certificate.claims.len();
            let outcome = outcome(&root, certificate);
            eprintln!("GOLDEN {name} ({claims} claims): {outcome}");
            (name, outcome)
        })
        .collect::<Vec<_>>();
    let expected = GOLDEN
        .iter()
        .map(|(name, outcome)| (*name, (*outcome).to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

fn id(row: usize) -> String {
    crate::encode_id(crate::canonical::symbol_key(&address(row)).as_bytes())
}

/// Direct lookups the frame outcome hides: on the capability path a
/// duplicate key falls back to the producer commitment, so only a lookup
/// shows that the duplicate itself was seen.
fn lookups(certificate: &crate::WireCertificate) -> String {
    let show = |result: Result<String, String>| match result {
        Ok(value) => format!("ok {value}"),
        Err(error) => format!("err {error}"),
    };
    let key = |schema, row| {
        show(
            certificate
                .key_value::<crate::canonical::SymbolSchema>(schema, &id(row))
                .map(|key| format!("{key:?}")),
        )
    };
    let identity = |row| {
        show(
            certificate
                .row_identity_preimage(crate::WireSchema::Symbol, &id(row))
                .map(|preimage| format!("{preimage:?}")),
        )
    };
    [
        key(crate::WireSchema::Symbol, 17),
        key(crate::WireSchema::Symbol, 42),
        key(crate::WireSchema::Symbol, 9),
        key(crate::WireSchema::Package, 9),
        identity(5),
        identity(6),
    ]
    .join(" | ")
}

const LOOKUP_GOLDEN: [(&str, &str); 6] = [
    (
        "valid",
        "ok ObjectKey:4a5b1593903a4b782c9838c23f4157a7232f7d236618e79439c5bd442681327d | ok ObjectKey:32a207916dde8db98ea11c0ca3baf809d31d5f360cf24f3ddf0d55e19183ee16 | ok ObjectKey:6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | err missing Package key certificate claim for 6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | ok None | ok None",
    ),
    (
        "duplicate",
        "err duplicate identity certificate claim | ok ObjectKey:32a207916dde8db98ea11c0ca3baf809d31d5f360cf24f3ddf0d55e19183ee16 | ok ObjectKey:6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | err missing Package key certificate claim for 6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | ok None | ok None",
    ),
    (
        "conflict",
        "err duplicate identity certificate claim | ok ObjectKey:32a207916dde8db98ea11c0ca3baf809d31d5f360cf24f3ddf0d55e19183ee16 | ok ObjectKey:6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | err missing Package key certificate claim for 6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | ok None | ok None",
    ),
    (
        "missing",
        "ok ObjectKey:4a5b1593903a4b782c9838c23f4157a7232f7d236618e79439c5bd442681327d | err missing Symbol key certificate claim for 32a207916dde8db98ea11c0ca3baf809d31d5f360cf24f3ddf0d55e19183ee16 | ok ObjectKey:6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | err missing Package key certificate claim for 6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | ok None | ok None",
    ),
    (
        "mixed",
        "ok ObjectKey:4a5b1593903a4b782c9838c23f4157a7232f7d236618e79439c5bd442681327d | ok ObjectKey:32a207916dde8db98ea11c0ca3baf809d31d5f360cf24f3ddf0d55e19183ee16 | ok ObjectKey:6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | err missing Package key certificate claim for 6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | err row identity certificate mixes explicit and ordinary claims | ok None",
    ),
    (
        "wrong-schema",
        "ok ObjectKey:4a5b1593903a4b782c9838c23f4157a7232f7d236618e79439c5bd442681327d | ok ObjectKey:32a207916dde8db98ea11c0ca3baf809d31d5f360cf24f3ddf0d55e19183ee16 | err missing Symbol key certificate claim for 6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | ok ObjectKey:6f6f894ad96ed53bb4ac1d0ff7ed1497fd4f1e1be61ae945d2117ce456d2a503 | ok None | ok None",
    ),
];

#[test]
fn small_frame_lookups_match_their_pinned_golden() {
    let root = small_frame();
    let actual = variants(&root)
        .into_iter()
        .map(|(name, certificate)| {
            let outcome = lookups(&certificate);
            eprintln!("LOOKUP {name}: {outcome}");
            (name, outcome)
        })
        .collect::<Vec<_>>();
    let expected = LOOKUP_GOLDEN
        .iter()
        .map(|(name, outcome)| (*name, (*outcome).to_owned()))
        .collect::<Vec<_>>();
    assert_eq!(actual, expected);
}

/// How a certificate answers lookups in one run of the equivalence check.
#[derive(Clone, Copy, Debug)]
enum Regime {
    Hashed,
    AllCollide,
    ThreeBuckets,
    Scanned,
}

fn prepared(certificate: &crate::WireCertificate, regime: Regime) {
    match regime {
        Regime::Hashed => {}
        Regime::AllCollide => certificate.index_with(|_| 0),
        Regime::ThreeBuckets => certificate.index_with(|id| {
            id.len() as u64 % 3 + u64::from(id.as_bytes().first().copied().unwrap_or(0) % 3)
        }),
        Regime::Scanned => certificate.scan_only(),
    }
}

/// The index can change which claims a lookup visits, never what it
/// answers: every regime reproduces both goldens exactly, including the
/// duplicate, the conflict and the mixed-claim rejections.
#[test]
fn every_index_regime_reproduces_the_scanning_goldens() {
    let root = small_frame();
    let capability = super::tests::capability(root.basis().object);
    for regime in [
        Regime::Hashed,
        Regime::AllCollide,
        Regime::ThreeBuckets,
        Regime::Scanned,
    ] {
        for ((name, certificate), ((_, frame_golden), (_, lookup_golden))) in variants(&root)
            .into_iter()
            .zip(GOLDEN.iter().zip(LOOKUP_GOLDEN.iter()))
        {
            prepared(&certificate, regime);
            assert_eq!(
                lookups(&certificate),
                *lookup_golden,
                "{regime:?} {name} lookups"
            );
            let wire = super::reply::view_root_to_wire(&root);
            let admitted = super::reply::view_root_from_wire_with_admission(
                &wire,
                &certificate,
                &super::reply_admission::CapabilityAdmission(Some(capability.clone())),
                false,
            )
            .map_or_else(
                |error| format!("err {error}"),
                |view| format!("ok root={:?} rows={}", view.root(), view.row_count()),
            );
            assert_eq!(admitted, *frame_golden, "{regime:?} {name} admission");
        }
    }
}

/// `claims` is a public field: once it is replaced, an index built for the
/// old slice must not answer for the new one.
#[test]
fn a_replaced_claim_list_is_never_answered_from_the_old_index() {
    let root = small_frame();
    let variants = variants(&root);
    let mut certificate = variants[0].1.clone();
    assert!(
        lookups(&certificate).starts_with("ok ObjectKey"),
        "the index is built"
    );
    certificate.claims = variants[1].1.claims.clone();
    assert_eq!(
        lookups(&certificate),
        LOOKUP_GOLDEN[1].1,
        "the duplicate is seen"
    );
}
