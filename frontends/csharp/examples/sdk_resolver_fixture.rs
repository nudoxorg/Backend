//! Explicit native SDK resolver fixture driver.
//!
//! This is intentionally an example instead of an auto-run test: invoking
//! dotnet is campaign-controlled and requires an allocated host slot.

use backend_frontend_csharp::legacy::{resolve_references_with_sdk, CSharpSdkKind};
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
    // Separate package candidates may have the same filename as framework
    // candidates. Their identities must reach SDK resolution independently.
    let package_candidates = env::var_os("CSHARP_FIXTURE_PACKAGE_CANDIDATES")
        .map(|value| env::split_paths(&value).collect::<Vec<_>>())
        .unwrap_or_default();
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
    let assets_bytes = assets.as_ref().map(|path| fs::read(path).expect("project.assets.json"));
    let cancelled = AtomicBool::new(false);
    let started = Instant::now();
    eprintln!("SDK fixture: resolver start; pack candidates={}, package candidates={}, assets bytes={}",
        candidates.len(), package_candidates.len(), assets_bytes.as_ref().map_or(0, Vec::len));
    let snapshot = resolve_references_with_sdk(
        &dotnet,
        &target.to_string_lossy(),
        &sdk,
        global_json.as_deref(),
        if env::var("CSHARP_FIXTURE_SDK_KIND").as_deref() == Ok("Web") { CSharpSdkKind::Web } else { CSharpSdkKind::Standard },
        &env::var("CSHARP_FIXTURE_NULLABLE").unwrap_or_else(|_| "enable".into()),
        &env::var("CSHARP_FIXTURE_LANGUAGE").unwrap_or_else(|_| "default".into()),
        &env::var("CSHARP_FIXTURE_OUTPUT").unwrap_or_else(|_| "Exe".into()),
        &env::var("CSHARP_FIXTURE_FRAMEWORKS").unwrap_or_default().split(',').filter(|value| !value.is_empty()).map(str::to_owned).collect::<Vec<_>>(),
        &candidates,
        &package_candidates,
        assets_bytes.as_deref(),
        Instant::now() + Duration::from_secs(120),
        &cancelled,
    )
    .unwrap_or_else(|error| panic!("native SDK resolver failed after {:?}: {error}", started.elapsed()));
    eprintln!("SDK fixture: resolver returned after {:?}; selected={}", started.elapsed(), snapshot.references().len());
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
    for (path, fusion_name) in snapshot
        .references()
        .iter()
        .zip(receipt.selected_fusion_names.iter())
    {
        let bytes = fs::read(path).expect("selected reference");
        println!("{}", serde_json::json!({
            "path": path.display().to_string(),
            "name": path.file_name().and_then(|name| name.to_str()).expect("UTF-8 assembly name"),
            "fusion_name": fusion_name.to_string(),
            "length": bytes.len(),
            "sha256": format!("{:x}", Sha256::digest(&bytes)),
        }));
    }
}
