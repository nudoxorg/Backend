//! Explicit native SDK resolver fixture driver.
//!
//! This is intentionally an example instead of an auto-run test: invoking
//! dotnet is campaign-controlled and requires an allocated host slot.

use backend_frontend_csharp::legacy::resolve_references_with_sdk;
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    path::PathBuf,
    sync::atomic::AtomicBool,
    time::{Duration, Instant},
};

fn usage() -> ! {
    eprintln!(
        "usage: sdk_resolver_fixture <dotnet> <sdk-dir> <target> <candidate-list> [assets] [global.json] [expected-names]"
    );
    std::process::exit(2);
}

fn main() {
    let mut args = env::args_os().skip(1);
    let dotnet = PathBuf::from(args.next().unwrap_or_else(|| usage()));
    let sdk = PathBuf::from(args.next().unwrap_or_else(|| usage()));
    let target = args.next().unwrap_or_else(|| usage());
    let candidate_list = args.next().unwrap_or_else(|| usage());
    let candidates = env::split_paths(&candidate_list).collect::<Vec<_>>();
    if candidates.is_empty() {
        usage();
    }
    let assets = args.next().map(PathBuf::from);
    let global_json = args
        .next()
        .map(|path| fs::read_to_string(path).expect("global.json"));
    let expected = args
        .next()
        .map(|value| value.to_string_lossy().into_owned());
    if args.next().is_some() {
        usage();
    }
    let assets_digest = assets
        .as_ref()
        .map(|path| Sha256::digest(fs::read(path).expect("project.assets.json")).into());
    let cancelled = AtomicBool::new(false);
    let snapshot = resolve_references_with_sdk(
        &dotnet,
        &target.to_string_lossy(),
        &sdk,
        global_json.as_deref(),
        &candidates,
        assets.as_deref(),
        assets_digest,
        Instant::now() + Duration::from_secs(120),
        &cancelled,
    )
    .expect("native SDK resolver");
    assert!(snapshot.receipt_is_consistent(), "selected custody failed");
    let receipt = snapshot.receipt();
    if let Some(expected) = expected {
        for name in expected.split(',').filter(|name| !name.is_empty()) {
            assert!(
                receipt
                    .selected_names
                    .iter()
                    .any(|actual| actual.as_ref() == name),
                "expected selected assembly {name}"
            );
        }
    }
    println!(
        "selected={} candidate_digest={:02x?} selected_digest={:02x?} receipt_digest={:02x?}",
        receipt.selected_names.len(),
        receipt.candidate_digest(),
        receipt.selected_digest(),
        receipt.digest()
    );
}
