use super::*;
use backend_execution::acquisition_work_key;
use std::{
    env, fs, io,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

const CHILD_MODE: &str = "BACKEND_ACQUISITION_LEASE_CHILD";
const CHILD_ROOT: &str = "BACKEND_ACQUISITION_LEASE_ROOT";
const CHILD_MARKERS: &str = "BACKEND_ACQUISITION_LEASE_MARKERS";

fn test_key() -> WorkKey {
    acquisition_work_key([0x51; 32], b"lease-process-test", [0xa2; 32], 1, 9)
}

fn different_stripe_key(key: WorkKey) -> WorkKey {
    (0..=u8::MAX)
        .map(|seed| acquisition_work_key([seed; 32], b"other-lease-stripe", [!seed; 32], 1, 9))
        .find(|candidate| candidate.as_bytes()[0] != key.as_bytes()[0])
        .expect("one of 256 work keys has another stripe")
}

fn two_stripe_key_pairs() -> ((WorkKey, WorkKey), (WorkKey, WorkKey)) {
    let mut groups: Vec<(u8, Vec<WorkKey>)> = Vec::new();
    for seed in 0..4096_u32 {
        let mut owner = [0; 32];
        owner[..4].copy_from_slice(&seed.to_be_bytes());
        let mut cursor = [0; 32];
        cursor[..4].copy_from_slice(&seed.rotate_left(13).to_be_bytes());
        let key = acquisition_work_key(owner, b"paired-lease-stripe-test", cursor, 4, 12);
        let stripe = key.as_bytes()[0];
        let group = groups
            .iter_mut()
            .find(|(group_stripe, _)| *group_stripe == stripe);
        if let Some((_, keys)) = group {
            if !keys.contains(&key) {
                keys.push(key);
            }
        } else {
            groups.push((stripe, vec![key]));
        }

        let mut complete: Vec<_> = groups.iter().filter(|(_, keys)| keys.len() >= 2).collect();
        if complete.len() >= 2 {
            complete.sort_by_key(|(stripe, _)| *stripe);
            return (
                (complete[0].1[0], complete[0].1[1]),
                (complete[1].1[0], complete[1].1[1]),
            );
        }
    }
    panic!("could not find two stripes with distinct WorkKeys");
}

fn fresh_root(label: &str) -> PathBuf {
    let path = env::temp_dir().join(format!(
        "acquisition-lease-{label}-{}-{}",
        std::process::id(),
        TOKEN_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&path);
    path
}

fn wait_for_path(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(5));
    }
    path.exists()
}

fn spawn_child(mode: &str, root: &Path, markers: &Path) -> Child {
    Command::new(env::current_exe().expect("test executable"))
        .args([
            "--exact",
            "acquisition::lease::tests::lease_child_process_helper",
            "--nocapture",
        ])
        .env(CHILD_MODE, mode)
        .env(CHILD_ROOT, root)
        .env(CHILD_MARKERS, markers)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn lease test child")
}

#[test]
fn lease_child_process_helper() {
    let Ok(mode) = env::var(CHILD_MODE) else {
        return;
    };
    let root = PathBuf::from(env::var_os(CHILD_ROOT).expect("child root"));
    let markers = PathBuf::from(env::var_os(CHILD_MARKERS).expect("child markers"));
    fs::create_dir_all(&markers).expect("marker directory");
    let store = LeaseStore::open(root).expect("child lease store");
    let key = test_key();

    match mode.as_str() {
        "race" => {
            assert!(wait_for_path(
                &markers.join("start"),
                Duration::from_secs(5)
            ));
            let guard = store
                .acquire(key, Duration::from_secs(30))
                .expect("race acquire");
            let outcome = if guard.is_some() { "winner" } else { "loser" };
            fs::write(markers.join(outcome), std::process::id().to_string())
                .expect("write race outcome");
            if let Some(guard) = guard {
                assert!(wait_for_path(
                    &markers.join("release"),
                    Duration::from_secs(8)
                ));
                drop(guard);
            }
        }
        "crash" => {
            let guard = store
                .acquire(key, Duration::from_millis(120))
                .expect("crash acquire")
                .expect("child owns lease");
            fs::write(markers.join("held"), "held").expect("write crash marker");
            loop {
                thread::sleep(Duration::from_millis(100));
                std::hint::black_box(&guard);
            }
        }
        "pair-race-low-first" | "pair-race-high-first" => {
            let ((low_first, low_second), (high_first, high_second)) = two_stripe_key_pairs();
            let (first_key, second_key) = if mode == "pair-race-low-first" {
                (low_first, high_first)
            } else {
                (high_second, low_second)
            };
            let mut first = store
                .acquire(first_key, Duration::from_secs(30))
                .expect("first paired acquire")
                .expect("first paired lease");
            let mut second = store
                .acquire(second_key, Duration::from_secs(30))
                .expect("second paired acquire")
                .expect("second paired lease");
            assert!(wait_for_path(
                &markers.join("start"),
                Duration::from_secs(5)
            ));
            let marker = markers.join(mode);
            let result = first
                .publish_if_both_current(&mut second, Duration::from_secs(30), |_, _| {
                    thread::sleep(Duration::from_millis(5));
                    fs::write(marker, "published")?;
                    Ok(())
                })
                .expect("paired process publication");
            assert!(result.is_some(), "paired process publication was fenced");
        }
        other => panic!("unknown helper mode {other}"),
    }
}

#[test]
fn expired_guard_cannot_delete_successor_or_publish_with_its_fence() {
    let root = fresh_root("stale-observer");
    let store = LeaseStore::open(&root).expect("store");
    let reopened = LeaseStore::open(&root).expect("reopened store");
    let mut expired = store
        .acquire(test_key(), Duration::from_millis(20))
        .expect("first acquire")
        .expect("first lease");
    thread::sleep(Duration::from_millis(35));

    let mut successor = reopened
        .acquire(test_key(), Duration::from_secs(30))
        .expect("takeover")
        .expect("expired lease is available");
    assert_ne!(expired.lease().token, successor.lease().token);
    assert!(!expired.renew(Duration::from_secs(30)).expect("stale renew"));

    let mut callback_ran = false;
    assert!(
        expired
            .publish_if_current(Duration::from_secs(30), |_| {
                callback_ran = true;
                Ok(())
            })
            .expect("stale publication check")
            .is_none()
    );
    assert!(!callback_ran);

    drop(expired);
    assert!(successor.owns(), "stale drop removed the successor record");
    assert!(
        reopened
            .acquire(test_key(), Duration::from_secs(30))
            .expect("contended acquire")
            .is_none()
    );
    drop(successor);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn expired_long_effect_fails_closed_before_publication() {
    let root = fresh_root("expired-publication");
    let store = LeaseStore::open(&root).expect("store");
    let mut lease = store
        .acquire(test_key(), Duration::from_millis(20))
        .expect("acquire")
        .expect("lease");
    thread::sleep(Duration::from_millis(35));

    let mut committed = false;
    assert!(
        lease
            .publish_if_current(Duration::from_secs(30), |_| {
                committed = true;
                Ok(())
            })
            .expect("publication check")
            .is_none()
    );
    assert!(!committed);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn submillisecond_ttls_are_rejected_without_mutating_a_lease() {
    let root = fresh_root("invalid-ttl");
    let store = LeaseStore::open(&root).expect("store");
    assert!(matches!(
        store.acquire(test_key(), Duration::from_micros(500)),
        Err(error) if error.kind() == io::ErrorKind::InvalidInput
    ));

    let mut lease = store
        .acquire(test_key(), Duration::from_secs(30))
        .expect("acquire")
        .expect("lease");
    assert_eq!(
        lease
            .renew(Duration::from_micros(500))
            .expect_err("submillisecond renew TTL")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(lease.owns(), "invalid renew TTL must not alter ownership");

    let mut callback_ran = false;
    assert_eq!(
        lease
            .publish_if_current(Duration::ZERO, |_| {
                callback_ran = true;
                Ok(())
            })
            .expect_err("zero publication TTL")
            .kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(!callback_ran);
    drop(lease);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn malformed_lease_record_does_not_authorize_takeover() {
    let root = fresh_root("malformed-record");
    let store = LeaseStore::open(&root).expect("store");
    let path = store.lease_slot_path(test_key(), 0);
    fs::write(&path, b"truncated fence").expect("write malformed record");

    assert!(matches!(
        store.acquire(test_key(), Duration::from_secs(30)),
        Err(error) if error.kind() == io::ErrorKind::InvalidData
    ));
    assert_eq!(
        fs::read(&path).expect("record stays untouched"),
        b"truncated fence"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn torn_alternate_slot_keeps_previous_lease_recoverable() {
    let root = fresh_root("torn-slot");
    let store = LeaseStore::open(&root).expect("store");
    let key = test_key();
    let lease = store
        .acquire(key, Duration::from_millis(120))
        .expect("acquire")
        .expect("lease");
    let current = store
        .latest_lease(key)
        .expect("read current lease")
        .expect("current record");
    let inactive_slot = 1 - current.slot.expect("versioned record");
    fs::remove_file(store.lease_slot_path(key, inactive_slot))
        .expect("remove the older slot before replacement");
    fs::write(store.lease_temp_path(key), b"torn newer record")
        .expect("leave an interrupted temp record");
    std::mem::forget(lease);

    let reopened = LeaseStore::open(&root).expect("reopen store");
    assert!(
        reopened
            .acquire(key, Duration::from_secs(30))
            .expect("the complete older slot remains readable")
            .is_none(),
        "a torn inactive slot must not authorize takeover"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn release_tombstone_survives_reopen() {
    let root = fresh_root("release-tombstone");
    let store = LeaseStore::open(&root).expect("store");
    let key = test_key();
    let lease = store
        .acquire(key, Duration::from_secs(30))
        .expect("acquire")
        .expect("lease");
    drop(lease);

    let released = store
        .latest_lease(key)
        .expect("released record")
        .expect("tombstone");
    assert!(!released.active);
    let older_slot = 1 - released.slot.expect("versioned tombstone");
    fs::remove_file(store.lease_slot_path(key, older_slot)).expect("remove older slot");
    fs::write(store.lease_temp_path(key), b"interrupted next acquisition")
        .expect("leave orphan temp");

    let reopened = LeaseStore::open(&root).expect("reopen store");
    let next = reopened
        .acquire(key, Duration::from_secs(30))
        .expect("acquire after tombstone")
        .expect("durably released lease is immediately available");
    drop(next);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn unrelated_work_key_can_acquire_while_another_key_publishes() {
    let root = fresh_root("independent-stripes");
    let store = LeaseStore::open(&root).expect("store");
    let key = test_key();
    let other_key = different_stripe_key(key);
    let mut lease = store
        .acquire(key, Duration::from_secs(30))
        .expect("first acquire")
        .expect("first lease");
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let thread_entered = Arc::clone(&entered);
    let thread_release = Arc::clone(&release);
    let publication = thread::spawn(move || {
        lease
            .publish_if_current(Duration::from_secs(30), |_| {
                thread_entered.wait();
                thread_release.wait();
                Ok(())
            })
            .expect("publication callback")
    });

    entered.wait();
    let other_lease = store.acquire(other_key, Duration::from_secs(30));
    release.wait();
    assert!(publication.join().expect("publication join").is_some());
    let other_lease = other_lease.expect("different stripe acquire");
    assert!(other_lease.is_some());
    drop(other_lease);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn paired_publication_deduplicates_a_shared_stripe() {
    let root = fresh_root("paired-same-stripe");
    let store = LeaseStore::open(&root).expect("store");
    let ((first_key, second_key), _) = two_stripe_key_pairs();
    assert_eq!(first_key.as_bytes()[0], second_key.as_bytes()[0]);
    let mut first = store
        .acquire(first_key, Duration::from_secs(30))
        .expect("first acquire")
        .expect("first lease");
    let mut second = store
        .acquire(second_key, Duration::from_secs(30))
        .expect("second acquire")
        .expect("second lease");

    let published = first
        .publish_if_both_current(&mut second, Duration::from_secs(30), |first, second| {
            assert_eq!(first.key, first_key);
            assert_eq!(second.key, second_key);
            assert_eq!(first.expires_at_millis, second.expires_at_millis);
            Ok(())
        })
        .expect("paired publication");
    assert!(published.is_some());
    assert!(first.owns());
    assert!(second.owns());
    drop(first);
    drop(second);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn reverse_stripe_order_threads_publish_without_deadlock() {
    let root = fresh_root("paired-reverse-thread-order");
    let store = LeaseStore::open(&root).expect("store");
    let ((low_a, low_b), (high_a, high_b)) = two_stripe_key_pairs();
    assert!(low_a.as_bytes()[0] < high_a.as_bytes()[0]);
    let mut first_low = store
        .acquire(low_a, Duration::from_secs(30))
        .expect("first low acquire")
        .expect("first low lease");
    let mut first_high = store
        .acquire(high_a, Duration::from_secs(30))
        .expect("first high acquire")
        .expect("first high lease");
    let mut second_high = store
        .acquire(high_b, Duration::from_secs(30))
        .expect("second high acquire")
        .expect("second high lease");
    let mut second_low = store
        .acquire(low_b, Duration::from_secs(30))
        .expect("second low acquire")
        .expect("second low lease");
    let start = Arc::new(Barrier::new(3));
    let first_start = Arc::clone(&start);
    let second_start = Arc::clone(&start);
    let first_thread = thread::spawn(move || {
        first_start.wait();
        first_low
            .publish_if_both_current(&mut first_high, Duration::from_secs(30), |_, _| Ok(()))
            .expect("low-high publication")
            .is_some()
    });
    let second_thread = thread::spawn(move || {
        second_start.wait();
        second_high
            .publish_if_both_current(&mut second_low, Duration::from_secs(30), |_, _| Ok(()))
            .expect("high-low publication")
            .is_some()
    });
    start.wait();

    assert!(first_thread.join().expect("first thread"));
    assert!(second_thread.join().expect("second thread"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn valid_endpoint_cannot_publish_with_expired_stolen_product_fence() {
    let root = fresh_root("paired-stolen-product");
    let store = LeaseStore::open(&root).expect("store");
    let endpoint_key = test_key();
    let product_key = different_stripe_key(endpoint_key);
    let mut endpoint = store
        .acquire(endpoint_key, Duration::from_secs(30))
        .expect("endpoint acquire")
        .expect("endpoint lease");
    let mut stale_product = store
        .acquire(product_key, Duration::from_millis(20))
        .expect("product acquire")
        .expect("product lease");
    thread::sleep(Duration::from_millis(35));
    let successor = store
        .acquire(product_key, Duration::from_secs(30))
        .expect("product takeover")
        .expect("successor product lease");

    let mut callback_ran = false;
    assert!(
        endpoint
            .publish_if_both_current(&mut stale_product, Duration::from_secs(30), |_, _| {
                callback_ran = true;
                Ok(())
            })
            .expect("paired stale-fence check")
            .is_none()
    );
    assert!(!callback_ran);
    assert!(endpoint.owns(), "valid endpoint fence was lost");
    assert!(successor.owns(), "stale product fence displaced successor");
    drop(stale_product);
    assert!(successor.owns(), "stale product drop removed successor");
    drop(endpoint);
    drop(successor);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn paired_publication_recovers_after_torn_followup_generation() {
    let root = fresh_root("paired-torn-recovery");
    let store = LeaseStore::open(&root).expect("store");
    let ((endpoint_key, product_key), _) = two_stripe_key_pairs();
    let mut endpoint = store
        .acquire(endpoint_key, Duration::from_secs(30))
        .expect("endpoint acquire")
        .expect("endpoint lease");
    let mut product = store
        .acquire(product_key, Duration::from_secs(30))
        .expect("product acquire")
        .expect("product lease");
    let published_marker = root.join("paired-publish-marker");
    assert!(
        endpoint
            .publish_if_both_current(&mut product, Duration::from_secs(30), |_, _| {
                fs::write(&published_marker, "both fences renewed")?;
                Ok(())
            })
            .expect("paired durable publication")
            .is_some()
    );
    std::mem::forget(endpoint);
    std::mem::forget(product);

    let current = store
        .latest_lease(product_key)
        .expect("current product generation")
        .expect("product record");
    let inactive_slot = 1 - current.slot.expect("versioned product record");
    fs::remove_file(store.lease_slot_path(product_key, inactive_slot))
        .expect("remove older product slot before next write");
    fs::write(
        store.lease_temp_path(product_key),
        b"torn followup generation",
    )
    .expect("leave orphan temp as after interrupted write");

    let reopened = LeaseStore::open(&root).expect("reopen after interrupted followup");
    assert_eq!(
        fs::read(&published_marker).expect("published marker"),
        b"both fences renewed"
    );
    assert!(
        reopened
            .acquire(endpoint_key, Duration::from_secs(30))
            .expect("endpoint recovered")
            .is_none(),
        "the endpoint fence must remain live after recovery"
    );
    assert!(
        reopened
            .acquire(product_key, Duration::from_secs(30))
            .expect("product recovered from last valid slot")
            .is_none(),
        "the product fence must remain live after torn followup recovery"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn reverse_stripe_order_processes_publish_without_deadlock() {
    let root = fresh_root("paired-reverse-process-order");
    let markers = root.join("markers");
    fs::create_dir_all(&markers).expect("markers");
    let mut low_first = spawn_child("pair-race-low-first", &root, &markers);
    let mut high_first = spawn_child("pair-race-high-first", &root, &markers);
    fs::write(markers.join("start"), "go").expect("start children");
    let first_done = wait_for_path(&markers.join("pair-race-low-first"), Duration::from_secs(5));
    let second_done = wait_for_path(
        &markers.join("pair-race-high-first"),
        Duration::from_secs(5),
    );
    let first_status = low_first.wait().expect("wait low-first child");
    let second_status = high_first.wait().expect("wait high-first child");
    assert!(first_done && second_done, "both paired callbacks must run");
    assert!(first_status.success());
    assert!(second_status.success());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn short_stripe_contention_waits_briefly_for_release() {
    let root = fresh_root("short-contention");
    let store = LeaseStore::open(&root).expect("store");
    let key = test_key();
    let gate = store.open_gate_file(key).expect("open stripe");
    gate.lock().expect("hold stripe");
    let waiting_store = store.clone();
    let (started, waiting) = std::sync::mpsc::channel();
    let acquisition = thread::spawn(move || {
        started.send(()).expect("signal start");
        waiting_store
            .acquire(key, Duration::from_secs(30))
            .expect("acquire after short contention")
            .is_some()
    });
    waiting.recv().expect("wait for contender");
    thread::sleep(Duration::from_millis(5));
    drop(gate);

    assert!(acquisition.join().expect("join contender"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn independent_processes_have_one_lease_winner() {
    let root = fresh_root("process-race");
    let markers = root.join("markers");
    fs::create_dir_all(&markers).expect("markers");
    let mut first = spawn_child("race", &root, &markers);
    let mut second = spawn_child("race", &root, &markers);
    fs::write(markers.join("start"), "go").expect("start children");

    let completed = wait_for_path(&markers.join("winner"), Duration::from_secs(5))
        && wait_for_path(&markers.join("loser"), Duration::from_secs(5));
    fs::write(markers.join("release"), "release").expect("release winner");
    let first_status = first.wait().expect("first child exit");
    let second_status = second.wait().expect("second child exit");
    assert!(completed, "both process results were not written");
    assert!(first_status.success());
    assert!(second_status.success());
    assert!(markers.join("winner").exists());
    assert!(markers.join("loser").exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn crashed_process_can_be_reopened_after_lease_expiry() {
    let root = fresh_root("process-crash");
    let markers = root.join("markers");
    fs::create_dir_all(&markers).expect("markers");
    let mut child = spawn_child("crash", &root, &markers);
    assert!(wait_for_path(&markers.join("held"), Duration::from_secs(5)));
    child.kill().expect("kill child");
    let _ = child.wait().expect("wait for killed child");
    thread::sleep(Duration::from_millis(150));

    let reopened = LeaseStore::open(&root).expect("reopen store");
    let lease = reopened
        .acquire(test_key(), Duration::from_secs(30))
        .expect("reopen acquire")
        .expect("crashed expired lease can be taken over");
    assert!(lease.owns());
    drop(lease);
    let _ = fs::remove_dir_all(root);
}
