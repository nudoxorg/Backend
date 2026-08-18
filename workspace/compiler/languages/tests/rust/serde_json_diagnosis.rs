//! Focused real-crate diagnosis for the serde_json lowering regression.
//!
//! Run with:
//! `RUSTC_BOOTSTRAP=1 cargo test -p nudox-languages --test rust_serde_json_diagnosis -- --ignored --nocapture`

use nudox_ir::{
    change::{EcosystemId, PackageLineageId, PackageName},
    kind::KindDiscriminant,
};
use nudox_languages::{PackageSource, produce, rust::RustProducer};
use std::{sync::mpsc, time::Duration};

const ACCEPTANCE_BUDGET: Duration = Duration::from_mins(5);

#[test]
#[ignore = "real rust-analyzer corpus diagnosis; emits per-phase timing"]
fn serde_json_lowering_is_measured_and_contains_declared_api() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../result/serde_json-1.0.113");
    assert!(
        root.join("Cargo.toml").is_file(),
        "serde_json corpus checkout is missing at {}; run `nix build .#checks.corpus`",
        root.display()
    );

    let source = PackageSource::new(&root, "serde_json", "1.0.113");
    let lineage = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("serde_json"));
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let measured =
            heart::cost::measured("diagnose/lower/cargo/serde_json-1.0.113", &root, || {
                produce(
                    &RustProducer { direct_repo: false },
                    &source,
                    &lineage,
                    &nudox_ir::foreign::Unlinked,
                )
            });
        sender.send(measured).expect("diagnosis receiver dropped");
    });
    let (result, cost) = receiver
        .recv_timeout(ACCEPTANCE_BUDGET)
        .unwrap_or_else(|_| {
            panic!(
                "serde_json lowering exceeded acceptance budget of {:.0}s; \
             per-phase timings above identify the stalled phase",
                ACCEPTANCE_BUDGET.as_secs_f64()
            )
        });
    let produced = result.unwrap_or_else(|error| panic!("serde_json lowering failed: {error:?}"));

    for (name, kind) in [
        ("Value", KindDiscriminant::Enum),
        ("Map", KindDiscriminant::Record),
        ("to_string", KindDiscriminant::Function),
        ("value", KindDiscriminant::Module),
        ("map", KindDiscriminant::Module),
    ] {
        assert!(
            produced.table.iter().any(|(_, entry)| {
                entry.sym().name == name && entry.kind().discriminant() == Some(kind)
            }),
            "serde_json declaration {name:?} ({kind:?}) was not lowered"
        );
    }

    eprintln!(
        "serde_json diagnosis: entries={} wall_ms={:.1} budget_ms={:.1}",
        produced.table.len(),
        cost.wall.as_secs_f64() * 1_000.0,
        ACCEPTANCE_BUDGET.as_secs_f64() * 1_000.0,
    );
    assert!(
        cost.wall <= ACCEPTANCE_BUDGET,
        "serde_json lowering exceeded acceptance budget: {:.1}s > {:.1}s",
        cost.wall.as_secs_f64(),
        ACCEPTANCE_BUDGET.as_secs_f64()
    );
}
