//! Adverse-condition tests for the handle-relative workspace filesystem.
//!
//! Each test names one hostile or unlucky condition a private workspace meets
//! on Windows: another handle holding an object open, a name swapped for a
//! link between the checks and the kernel call, names that differ only in
//! case, files in the delete-pending state, paths beyond `MAX_PATH`, and
//! writers racing to replace the same name. The plain happy paths live in the
//! parent module's tests.

use super::{
    EntryKind, RenameInformation, WorkspaceRoot, flush_handle, is_full_control,
    is_reserved_device_name, rename_information_length, validate_component,
};
use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_PRIVILEGE_NOT_HELD: i32 = 1314;

fn fresh_path(label: &str) -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let tick = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "backend-ws-adv-{label}-{}-{tick}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ))
}

/// A private workspace root and the path it was created at.
fn private_root(label: &str) -> (WorkspaceRoot, PathBuf) {
    let path = fresh_path(label);
    let root = WorkspaceRoot::create(&path).expect("create private root");
    (root, path)
}

fn put(root: &WorkspaceRoot, name: &[&str], bytes: &[u8]) {
    let mut file = root.create_file_exclusive(name).expect("create file");
    file.write_all(bytes).expect("write file");
    file.sync_all().expect("flush file");
}

fn get(root: &WorkspaceRoot, name: &[&str]) -> Vec<u8> {
    let mut bytes = Vec::new();
    root.open_file_read_checked(name)
        .expect("open checked file")
        .read_to_end(&mut bytes)
        .expect("read checked file");
    bytes
}

fn junction(link: &Path, target: &Path) {
    let output = Command::new("cmd.exe")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .expect("run mklink");
    assert!(output.status.success(), "mklink /J failed: {output:?}");
}

/// Creates a file symlink, or reports that the OS refuses to (creating one
/// needs Developer Mode or `SeCreateSymbolicLinkPrivilege`).
fn symlink_file(original: &Path, link: &Path) -> bool {
    match std::os::windows::fs::symlink_file(original, link) {
        Ok(()) => true,
        Err(error) if error.raw_os_error() == Some(ERROR_PRIVILEGE_NOT_HELD) => {
            eprintln!("skipping symlink case: this account may not create symbolic links");
            false
        }
        Err(error) => panic!("create file symlink: {error}"),
    }
}

#[test]
fn two_pins_of_one_directory_coexist_and_list_beside_an_open_writer() {
    let (root, path) = private_root("pins");
    let writer = root.create_file_exclusive(&["live"]).expect("open writer");

    // A pinned handle must not request delete access, or a second pin of the
    // same directory (and the independent handle enumeration opens) would hit a
    // sharing violation against the first.
    let second = WorkspaceRoot::open(&path).expect("second pin of the same directory");
    let first_listing = root.read_dir_checked(&[]).expect("list through creator");
    let second_listing = second
        .read_dir_checked(&[])
        .expect("list through second pin");
    assert_eq!(first_listing, second_listing);
    assert_eq!(first_listing.len(), 1);
    assert_eq!(first_listing[0].name, "live");
    assert_eq!(first_listing[0].kind, EntryKind::File);

    drop(writer);
    drop(second);
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn a_pinned_directory_and_its_ancestors_cannot_be_renamed_or_deleted_from_outside() {
    let (root, path) = private_root("pinned");
    let inner = root
        .create_child_dir_exclusive("inner")
        .expect("create pinned child");
    let moved_inner = path.join("inner-moved");
    let moved_root = fresh_path("pinned-moved");

    assert!(fs::rename(path.join("inner"), &moved_inner).is_err());
    assert!(fs::remove_dir(path.join("inner")).is_err());
    assert!(
        fs::rename(&path, &moved_root).is_err(),
        "an open descendant pins every ancestor"
    );
    assert!(
        inner
            .read_dir_checked(&[])
            .expect("still reachable")
            .is_empty()
    );

    drop(inner);
    drop(root);
    fs::rename(&path, &moved_root).expect("once nothing is pinned the directory moves");
    fs::remove_dir_all(moved_root).expect("cleanup");
}

#[test]
fn replacing_rename_publishes_while_a_reader_holds_the_old_file_open() {
    let (root, path) = private_root("replace-open");
    put(&root, &["state"], b"old generation");
    put(&root, &["next"], b"new generation");
    let mut reader = root
        .open_file_read_checked(&["state"])
        .expect("open the old generation");

    root.rename_relative(&["next"], &["state"], true)
        .expect("POSIX-semantics replace succeeds beside a delete-sharing reader");

    let mut held = Vec::new();
    reader
        .read_to_end(&mut held)
        .expect("read through old handle");
    assert_eq!(held, b"old generation", "the reader keeps its object");
    assert_eq!(get(&root, &["state"]), b"new generation");
    let names = root
        .read_dir_checked(&[])
        .expect("list")
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["state"], "no temporary or displaced file survives");

    drop(reader);
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn rename_without_replace_refuses_any_case_variant_of_an_existing_name() {
    let (root, path) = private_root("case-rename");
    put(&root, &["Report.TXT"], b"first");
    put(&root, &["staged"], b"second");

    let error = root
        .rename_relative(&["staged"], &["report.txt"], false)
        .expect_err("NTFS names are case-insensitive, so this collides");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(get(&root, &["Report.TXT"]), b"first");
    assert_eq!(get(&root, &["staged"]), b"second");

    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn case_variants_collide_on_exclusive_creation_of_files_and_directories() {
    let (root, path) = private_root("case-create");
    put(&root, &["Data"], b"file");
    let file_error = root
        .create_file_exclusive(&["DATA"])
        .expect_err("same file name in another case");
    assert_eq!(file_error.kind(), std::io::ErrorKind::AlreadyExists);
    let directory_error = root
        .create_child_dir_exclusive("data")
        .map(|_| ())
        .expect_err("a directory cannot take a file's name in another case");
    assert_eq!(directory_error.kind(), std::io::ErrorKind::AlreadyExists);

    let held = root.create_child_dir_exclusive("Dir").expect("create Dir");
    let again = root
        .create_child_dir_exclusive("dIR")
        .map(|_| ())
        .expect_err("same directory name in another case");
    assert_eq!(again.kind(), std::io::ErrorKind::AlreadyExists);

    drop(held);
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn rename_refuses_hard_linked_and_reparse_point_endpoints() {
    let (root, path) = private_root("endpoints");
    let outside = fresh_path("endpoints-victim");
    fs::write(&outside, b"victim").expect("create victim");

    put(&root, &["linked"], b"two names");
    fs::hard_link(path.join("linked"), path.join("alias")).expect("hard link");
    assert!(
        root.rename_relative(&["linked"], &["moved"], false)
            .is_err()
    );
    assert!(root.rename_relative(&["alias"], &["moved"], false).is_err());

    put(&root, &["plain"], b"plain");
    if symlink_file(&outside, &path.join("link")) {
        assert!(
            root.rename_relative(&["link"], &["moved"], false).is_err(),
            "a reparse point is never a rename source"
        );
        assert!(
            root.rename_relative(&["plain"], &["link"], true).is_err(),
            "a reparse point is never silently replaced"
        );
        assert_eq!(get(&root, &["plain"]), b"plain");
        fs::remove_file(path.join("link")).expect("remove symlink");
    }
    assert_eq!(fs::read(&outside).expect("victim"), b"victim");

    drop(root);
    fs::remove_file(path.join("alias")).expect("remove alias");
    fs::remove_dir_all(path).expect("cleanup");
    fs::remove_file(outside).expect("remove victim");
}

#[test]
fn a_name_swapped_to_a_symlink_after_the_checks_cannot_redirect_the_rename() {
    let (root, path) = private_root("swap-file");
    let outside = fresh_path("swap-file-victim");
    fs::write(&outside, b"victim").expect("create victim");
    put(&root, &["source"], b"workspace bytes");

    let attacker_source = path.join("source");
    let attacker_parked = path.join("parked");
    let mut swapped = false;
    let outcome = root.rename_checked_entry_with_flush(
        &["source"],
        &["published"],
        true,
        false,
        || {
            // Between the last check and the kernel call the attacker parks the
            // checked file and plants a link where its name was.
            fs::rename(&attacker_source, &attacker_parked).expect("park checked file");
            swapped = symlink_file(&outside, &attacker_source);
        },
        flush_handle,
    );
    outcome.expect("the rename moves the object that was checked");

    assert_eq!(get(&root, &["published"]), b"workspace bytes");
    assert_eq!(fs::read(&outside).expect("victim"), b"victim");
    assert!(
        !attacker_parked.exists(),
        "the checked object left its parked name"
    );
    if swapped {
        assert!(
            fs::symlink_metadata(&attacker_source)
                .expect("planted link")
                .file_type()
                .is_symlink(),
            "the planted link was neither followed nor replaced"
        );
        fs::remove_file(&attacker_source).expect("remove planted link");
    }

    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
    fs::remove_file(outside).expect("remove victim");
}

#[test]
fn a_directory_swapped_to_a_junction_after_the_checks_cannot_redirect_the_rename() {
    let (root, path) = private_root("swap-directory");
    let outside = fresh_path("swap-directory-victim");
    fs::create_dir(&outside).expect("create victim directory");
    fs::write(outside.join("must-survive"), b"victim").expect("create victim marker");
    root.create_child_dir_exclusive("source")
        .expect("create source directory");
    put(&root, &["source", "inside"], b"workspace bytes");

    let attacker_source = path.join("source");
    let attacker_parked = path.join("parked");
    let outcome = root.rename_checked_entry_with_flush(
        &["source"],
        &["published"],
        false,
        true,
        || {
            fs::rename(&attacker_source, &attacker_parked).expect("park checked directory");
            junction(&attacker_source, &outside);
        },
        flush_handle,
    );
    outcome.expect("the rename moves the directory that was checked");

    assert_eq!(get(&root, &["published", "inside"]), b"workspace bytes");
    assert_eq!(
        fs::read(outside.join("must-survive")).expect("victim marker"),
        b"victim"
    );
    assert!(
        fs::symlink_metadata(&attacker_source)
            .expect("planted junction")
            .file_type()
            .is_symlink(),
        "the planted junction stayed in place"
    );

    drop(root);
    fs::remove_dir(&attacker_source).expect("remove junction itself");
    fs::remove_dir_all(path).expect("cleanup");
    fs::remove_dir_all(outside).expect("remove victim");
}

#[test]
fn remove_refuses_a_file_a_writer_still_holds_and_succeeds_once_it_is_released() {
    let (root, path) = private_root("remove-held");
    let writer = root.create_file_exclusive(&["busy"]).expect("open writer");

    let error = root
        .remove_file_relative(&["busy"])
        .expect_err("a writer that does not share delete blocks removal");
    assert_eq!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION));
    assert!(root.open_file_read_checked(&["busy"]).is_ok());

    drop(writer);
    root.remove_file_relative(&["busy"])
        .expect("remove after the writer is released");
    let missing = root
        .open_file_read_checked(&["busy"])
        .expect_err("the file is gone");
    assert_eq!(missing.kind(), std::io::ErrorKind::NotFound);

    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn a_delete_pending_file_is_not_admitted() {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::os::windows::io::AsRawHandle as _;
    use windows_sys::Win32::Storage::FileSystem::{
        DELETE, FILE_DISPOSITION_INFO, FILE_READ_ATTRIBUTES, FileDispositionInfo,
        SetFileInformationByHandle,
    };

    let (root, path) = private_root("delete-pending");
    put(&root, &["doomed"], b"going away");
    let holder = fs::OpenOptions::new()
        .access_mode(DELETE | FILE_READ_ATTRIBUTES)
        .share_mode(7)
        .open(path.join("doomed"))
        .expect("open a delete-sharing handle");
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `holder` is a live handle opened with DELETE access, and
    // `disposition` is readable storage of the exact size the class requires.
    // The legacy class (not the POSIX one) leaves the file delete-pending until
    // the last handle closes, which is the state under test.
    let marked = unsafe {
        SetFileInformationByHandle(
            holder.as_raw_handle().cast(),
            FileDispositionInfo,
            (&raw const disposition).cast(),
            u32::try_from(size_of::<FILE_DISPOSITION_INFO>()).expect("small structure"),
        )
    };
    assert_ne!(
        marked,
        0,
        "mark delete pending: {}",
        std::io::Error::last_os_error()
    );

    let refused = root
        .open_file_read_checked(&["doomed"])
        .expect_err("a delete-pending file must not be admitted");
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        root.rename_relative(&["doomed"], &["revived"], false)
            .is_err()
    );

    drop(holder);
    let gone = root
        .open_file_read_checked(&["doomed"])
        .expect_err("closing the last handle completes the delete");
    assert_eq!(gone.kind(), std::io::ErrorKind::NotFound);

    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn paths_beyond_max_path_are_created_listed_reopened_and_removed_by_handle() {
    const SEGMENT: usize = 40;
    const DEPTH: usize = 8;

    let (root, path) = private_root("long");
    let names = (0..DEPTH)
        .map(|index| format!("{index:02}{}", "d".repeat(SEGMENT - 2)))
        .collect::<Vec<_>>();
    let mut deepest = root.clone();
    let mut deep_path = path.clone();
    for name in &names {
        deepest = deepest
            .create_child_dir_exclusive(name)
            .expect("create nested directory");
        deep_path.push(name);
    }
    assert!(
        deep_path.as_os_str().len() > 300,
        "fixture must exceed MAX_PATH, got {}",
        deep_path.as_os_str().len()
    );
    put(&deepest, &["deep.bin"], b"beyond MAX_PATH");
    drop(deepest);
    drop(root);

    // Cold reopen by the over-long path: each component is resolved against
    // the previous handle, so no single call sees the whole path.
    let reopened = WorkspaceRoot::open(&deep_path).expect("reopen over-long path");
    assert_eq!(get(&reopened, &["deep.bin"]), b"beyond MAX_PATH");
    let listing = reopened.read_dir_checked(&[]).expect("list over-long path");
    assert_eq!(listing.len(), 1);
    assert_eq!(listing[0].name, "deep.bin");
    drop(reopened);

    let top = WorkspaceRoot::open(&path).expect("reopen top");
    top.remove_dir_tree(&[names[0].as_str()])
        .expect("remove the over-long tree by handle");
    assert!(top.read_dir_checked(&[]).expect("list top").is_empty());
    drop(top);
    fs::remove_dir(path).expect("cleanup");
}

#[test]
fn remove_dir_tree_removes_nested_trees_and_refuses_a_junction_without_following_it() {
    let (root, path) = private_root("tree");
    let outside = fresh_path("tree-victim");
    fs::create_dir(&outside).expect("create victim directory");
    fs::write(outside.join("must-survive"), b"victim").expect("create victim marker");

    let plain = root.create_child_dir_exclusive("plain").expect("plain dir");
    let nested = plain
        .create_child_dir_exclusive("nested")
        .expect("nested dir");
    put(&nested, &["leaf"], b"leaf");
    put(&plain, &["top"], b"top");
    drop(nested);
    drop(plain);
    root.remove_dir_tree(&["plain"])
        .expect("remove nested tree");
    assert_eq!(
        root.child_is_directory(&["plain"])
            .expect_err("tree is gone")
            .kind(),
        std::io::ErrorKind::NotFound
    );

    let trapped = root.create_child_dir_exclusive("trapped").expect("trapped");
    put(&trapped, &["sibling"], b"sibling");
    drop(trapped);
    junction(&path.join("trapped").join("link"), &outside);
    assert!(
        root.remove_dir_tree(&["trapped"]).is_err(),
        "a reparse point inside the tree fails the removal closed"
    );
    assert_eq!(
        fs::read(outside.join("must-survive")).expect("victim marker"),
        b"victim",
        "the junction target was never entered"
    );

    drop(root);
    fs::remove_dir(path.join("trapped").join("link")).expect("remove junction itself");
    fs::remove_dir_all(path).expect("cleanup");
    fs::remove_dir_all(outside).expect("remove victim");
}

#[test]
fn an_entry_with_a_foreign_dacl_is_rejected_by_open_and_by_listing() {
    let (root, path) = private_root("foreign");
    // Created through the standard library, the file carries an ordinary
    // multi-ACE DACL rather than the single current-user entry.
    fs::write(path.join("foreign.txt"), b"not ours").expect("foreign file");

    let error = root
        .open_file_read_checked(&["foreign.txt"])
        .expect_err("a non-private file is not admitted");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(root.read_dir_checked(&[]).is_err());

    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn concurrent_replacing_renames_publish_one_complete_generation_to_readers() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::thread;

    const WRITERS: u8 = 6;
    const ROUNDS: usize = 40;
    const PAYLOAD: usize = 4096;

    let (root, path) = private_root("publish");
    put(&root, &["state"], &[0xAA; PAYLOAD]);

    let finished = Arc::new(AtomicBool::new(false));
    let reader_root = root.clone();
    let reader_flag = Arc::clone(&finished);
    let reader = thread::spawn(move || {
        let mut observed = 0_usize;
        while !reader_flag.load(Ordering::Relaxed) {
            let mut bytes = Vec::new();
            reader_root
                .open_file_read_checked(&["state"])
                .expect("a published name always opens")
                .read_to_end(&mut bytes)
                .expect("read a published generation");
            assert_eq!(
                bytes.len(),
                PAYLOAD,
                "a torn or empty generation was visible"
            );
            assert!(
                bytes.iter().all(|byte| *byte == bytes[0]),
                "a mixed generation was visible"
            );
            observed += 1;
        }
        observed
    });

    let writers = (0..WRITERS)
        .map(|writer| {
            let root = root.clone();
            thread::spawn(move || {
                let mut refused = 0_usize;
                for round in 0..ROUNDS {
                    let temporary = format!(".state.{writer}.{round}.tmp");
                    put(&root, &[temporary.as_str()], &[writer; PAYLOAD]);
                    loop {
                        match root.rename_relative(&[temporary.as_str()], &["state"], true) {
                            Ok(()) => break,
                            // Two replacers can collide inside the kernel; the
                            // loser must be told, never half-publish.
                            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
                                refused += 1;
                                thread::yield_now();
                            }
                            Err(error) => panic!("replacing rename failed: {error}"),
                        }
                    }
                }
                refused
            })
        })
        .collect::<Vec<_>>();
    let refusals: usize = writers
        .into_iter()
        .map(|writer| writer.join().expect("join writer"))
        .sum();
    finished.store(true, Ordering::Relaxed);
    let observed = reader.join().expect("join reader");
    eprintln!("reader observed {observed} generations; writers retried {refusals} refusals");

    let names = root
        .read_dir_checked(&[])
        .expect("list after publication")
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["state"], "no temporary file survived");
    let last = get(&root, &["state"]);
    assert_eq!(last.len(), PAYLOAD);
    assert!(last.iter().all(|byte| *byte == last[0]));

    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn reserved_device_names_and_oversized_components_are_refused() {
    for reserved in [
        "NUL",
        "nul",
        "Con",
        "PRN",
        "aux",
        "AUX.txt",
        "com1",
        "COM9.log",
        "LPT1",
        "lpt9.",
        "CONIN$",
        "conout$",
        "NUL .txt",
        "COM\u{b9}",
        "LPT\u{b2}.dat",
    ] {
        assert!(
            validate_component(reserved).is_err(),
            "{reserved:?} must be refused"
        );
    }
    for allowed in [
        "console",
        "com10",
        "COM0",
        "nullable",
        "auxiliary",
        "LPT",
        "COM",
        "conn",
        "a.nul",
    ] {
        assert!(
            validate_component(allowed).is_ok(),
            "{allowed:?} must be accepted"
        );
        assert!(!is_reserved_device_name(allowed));
    }

    let widest = "w".repeat(255);
    assert!(validate_component(&widest).is_ok());
    assert!(validate_component(&"w".repeat(256)).is_err());
    // Surrogate pairs count as two UTF-16 units each: 128 of them is 256.
    assert!(validate_component(&"\u{1f980}".repeat(127)).is_ok());
    assert!(validate_component(&"\u{1f980}".repeat(128)).is_err());

    let (root, path) = private_root("reserved");
    for reserved in ["NUL", "com1.txt"] {
        let error = root
            .create_file_exclusive(&[reserved])
            .expect_err("reserved device names are never created");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }
    assert!(root.read_dir_checked(&[]).expect("list").is_empty());
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn only_full_control_masks_count_as_a_private_grant() {
    use windows_sys::Win32::Foundation::GENERIC_ALL;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ALL_ACCESS, FILE_GENERIC_READ, FILE_GENERIC_WRITE,
    };

    assert!(is_full_control(FILE_ALL_ACCESS), "the mapped form");
    assert!(is_full_control(GENERIC_ALL), "the unmapped form");
    assert!(!is_full_control(0));
    assert!(!is_full_control(FILE_GENERIC_READ));
    assert!(!is_full_control(FILE_GENERIC_WRITE));
    assert!(!is_full_control(FILE_GENERIC_READ | FILE_GENERIC_WRITE));
    assert!(!is_full_control(FILE_ALL_ACCESS & !1));
}

/// Holds `path` open for `hold` without sharing delete access, the way a virus
/// scanner or indexer does, and reports when the hold began.
fn scanner_holds(path: PathBuf, hold: std::time::Duration) -> std::thread::JoinHandle<()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    let (held, ready) = std::sync::mpsc::channel();
    let scanner = std::thread::spawn(move || {
        let handle = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(&path)
            .expect("scanner opens the file");
        held.send(()).expect("announce the hold");
        std::thread::sleep(hold);
        drop(handle);
    });
    ready.recv().expect("scanner holds the file");
    scanner
}

#[test]
fn a_scanner_briefly_holding_the_destination_is_waited_out_by_a_replacing_rename() {
    let (root, path) = private_root("scan-destination");
    put(&root, &["state"], b"old generation");
    put(&root, &["next"], b"new generation");

    let scanner = scanner_holds(path.join("state"), std::time::Duration::from_millis(60));
    root.rename_relative(&["next"], &["state"], true)
        .expect("the publication waits for the scanner and then succeeds");
    scanner.join().expect("join scanner");

    assert_eq!(get(&root, &["state"]), b"new generation");
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn a_scanner_briefly_holding_the_source_is_waited_out_by_rename_and_remove() {
    let (root, path) = private_root("scan-source");
    put(&root, &["next"], b"new generation");

    let scanner = scanner_holds(path.join("next"), std::time::Duration::from_millis(60));
    root.rename_relative(&["next"], &["state"], false)
        .expect("opening the source for rename waits out the scanner");
    scanner.join().expect("join scanner");

    let scanner = scanner_holds(path.join("state"), std::time::Duration::from_millis(60));
    root.remove_file_relative(&["state"])
        .expect("removal waits out the scanner");
    scanner.join().expect("join scanner");

    assert!(root.read_dir_checked(&[]).expect("list").is_empty());
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn a_holder_that_never_lets_go_fails_with_its_own_error_and_changes_nothing() {
    let (root, path) = private_root("scan-forever");
    put(&root, &["state"], b"old generation");
    put(&root, &["next"], b"new generation");

    let scanner = scanner_holds(path.join("state"), std::time::Duration::from_secs(3));
    let started = std::time::Instant::now();
    let error = root
        .rename_relative(&["next"], &["state"], true)
        .expect_err("a holder outlasting the wait is an error");
    let waited = started.elapsed();
    assert!(
        matches!(error.raw_os_error(), Some(5 | ERROR_SHARING_VIOLATION)),
        "the holder's own error is reported, got {error}"
    );
    assert!(
        waited < std::time::Duration::from_millis(2_500),
        "the wait is bounded, took {waited:?}"
    );
    assert_eq!(get(&root, &["state"]), b"old generation");
    assert_eq!(get(&root, &["next"]), b"new generation");

    scanner.join().expect("join scanner");
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn republishing_beside_a_scanner_that_inspects_every_generation_never_fails() {
    use std::os::windows::fs::OpenOptionsExt as _;
    use std::sync::mpsc;

    let (root, path) = private_root("scan-loop");
    put(&root, &["state"], b"generation 0");

    // Real-time protection opens each file the moment it changes and holds it
    // without sharing delete while it scans, which is exactly when the next
    // publication arrives.
    let (changed, notifications) = mpsc::channel::<()>();
    let scanned = path.join("state");
    let scanner = std::thread::spawn(move || {
        while notifications.recv().is_ok() {
            if let Ok(handle) = fs::OpenOptions::new()
                .read(true)
                .share_mode(1 | 2)
                .open(&scanned)
            {
                std::thread::sleep(std::time::Duration::from_millis(3));
                drop(handle);
            }
        }
    });

    for generation in 1..=100_u32 {
        let temporary = format!(".state.{generation}.tmp");
        put(
            &root,
            &[temporary.as_str()],
            format!("generation {generation}").as_bytes(),
        );
        root.rename_relative(&[temporary.as_str()], &["state"], true)
            .expect("republish beside a scanner");
        changed.send(()).expect("notify the scanner");
    }
    drop(changed);
    scanner.join().expect("join scanner");

    assert_eq!(get(&root, &["state"]), b"generation 100");
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

/// The kernel rejects a `FILE_RENAME_INFORMATION` request shorter than the structure with
/// `STATUS_INFO_LENGTH_MISMATCH` (os error 24). The header plus a one-character name is 22 bytes,
/// two short, so the sizing must pad to the structure and grow linearly beyond it.
#[test]
fn rename_information_is_never_shorter_than_its_structure() {
    let structure = size_of::<RenameInformation>();
    assert_eq!(rename_information_length(0), Some(structure));
    assert_eq!(rename_information_length(2), Some(structure));
    assert_eq!(rename_information_length(4), Some(structure));
    assert_eq!(rename_information_length(6), Some(26));
    assert_eq!(rename_information_length(usize::MAX), None);
}

/// A one-character destination is an ordinary Windows name; renaming a file and a directory to
/// one must work, both creating the name and replacing it.
#[test]
fn a_one_character_destination_name_renames_files_and_directories() {
    let (root, path) = private_root("one-char-name");
    put(&root, &["staged"], b"x");
    root.rename_relative(&["staged"], &["a"], false)
        .expect("a one-character destination name is a valid Windows name");
    assert_eq!(get(&root, &["a"]), b"x");
    put(&root, &["next"], b"y");
    root.rename_relative(&["next"], &["a"], true)
        .expect("a one-character destination is replaceable");
    assert_eq!(get(&root, &["a"]), b"y");

    root.create_child_dir_exclusive("dir")
        .expect("create directory");
    root.rename_directory_relative(&["dir"], &["d"])
        .expect("a directory renames to a one-character name");
    assert!(root.child_is_directory(&["d"]).expect("renamed directory"));
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}
