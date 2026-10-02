//! Adverse-condition tests for the handle-relative workspace filesystem.
//!
//! Each test names one hostile or unlucky condition a private workspace meets
//! on Windows: another handle holding an object open, a name swapped for a
//! link between the checks and the kernel call, names that differ only in
//! case, files in the delete-pending state, paths beyond `MAX_PATH`, and
//! writers racing to replace the same name. The plain happy paths live in the
//! parent module's tests.

use super::{
    BACKOFF, EntryKind, EnumeratedEntry, ExistingName, FileId128, IfUnlinked, NewName,
    RenameInformation, Sharing, WorkspaceRoot, decode_directory_records, ensure_private_handle,
    ensure_regular_file_handle_with, enumerate_names, file_from_handle, flush_handle,
    is_full_control, open_admitted_file, open_relative, rename_information_length, sid_text,
};
use std::fs;
use std::io::{self, Read as _, Write as _};
use std::os::windows::io::AsRawHandle as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES;

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
            let mut opened = match reader_root.open_file_read_checked(&["state"]) {
                Ok(opened) => opened,
                // The replace unlinks the old name before it links the new one, so a lookup
                // between the two can report the name missing (see `rename_into` for the
                // measurement). Every successful open must still be one whole generation.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => panic!("a published name opens or is briefly absent: {error}"),
            };
            opened
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
fn existing_names_obey_only_the_syntax_rules() {
    // Names that creation refuses are ordinary entries to look up: source trees hold `aux.c`.
    for name in [
        "NUL",
        "aux",
        "aux.c",
        "CON.txt",
        "NUL.tar.gz",
        "COM1.x",
        "prn.rs",
        "com0",
        "COM\u{b9}",
        "CONIN$",
        "console",
        "a.nul",
    ] {
        assert!(
            ExistingName::parse(name).is_ok(),
            "{name:?} is a valid existing name"
        );
    }
    for name in [
        "",
        ".",
        "..",
        "trailing.",
        "trailing ",
        "a/b",
        "a\\b",
        "a:b",
        "a*b",
        "a?b",
        "a\"b",
        "a<b",
        "a>b",
        "a|b",
        "a\u{1}b",
        "a\0b",
    ] {
        assert_eq!(
            ExistingName::parse(name)
                .expect_err("syntax violation")
                .kind(),
            std::io::ErrorKind::InvalidInput,
            "{name:?}"
        );
    }

    assert!(ExistingName::parse(&"w".repeat(255)).is_ok());
    assert!(ExistingName::parse(&"w".repeat(256)).is_err());
    // Surrogate pairs count as two UTF-16 units each: 128 of them is 256.
    assert!(ExistingName::parse(&"\u{1f980}".repeat(127)).is_ok());
    assert!(ExistingName::parse(&"\u{1f980}".repeat(128)).is_err());
}

#[test]
fn a_new_name_is_refused_only_when_the_system_routes_it_to_a_device() {
    // `NUL` is a device on every Windows release, and the standard library, which goes through
    // Win32 path normalization, shows it: the bytes written to it do not come back.
    let probe = fresh_path("nul-probe");
    fs::create_dir(&probe).expect("create probe directory");
    fs::write(probe.join("NUL"), b"x").expect("writing to the NUL device succeeds");
    assert_ne!(
        fs::read(probe.join("NUL")).expect("reading the NUL device succeeds"),
        b"x",
        "Win32 routes NUL to the device"
    );
    fs::remove_dir_all(&probe).expect("cleanup");
    for device in ["NUL", "nul"] {
        assert_eq!(
            NewName::parse(device)
                .expect_err("a device name is never created")
                .kind(),
            std::io::ErrorKind::InvalidInput,
            "{device:?}"
        );
    }

    // On Windows 11 only bare device names are special; these are ordinary files that Win32
    // reaches, so a source tree or workspace may hold them.
    let (root, path) = private_root("ordinary-device-like");
    for name in [
        "aux.c",
        "CON.txt",
        "NUL.tar.gz",
        "COM1.x",
        "prn.rs",
        "nul.txt",
        "com0",
        "com10",
        "console",
        "a.nul",
    ] {
        assert!(NewName::parse(name).is_ok(), "{name:?} is an ordinary name");
        put(&root, &[name], name.as_bytes());
        assert_eq!(
            fs::read(path.join(name)).expect("Win32 reaches the file that was created"),
            name.as_bytes(),
            "{name:?}"
        );
    }
    assert_eq!(root.read_dir_checked(&[]).expect("list").len(), 10);

    // Neither creation nor a rename destination may be a device name; the source stays put.
    let error = root
        .create_file_exclusive(&["NUL"])
        .expect_err("a device name is never created");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    let error = root
        .rename_relative(&["aux.c"], &["NUL"], false)
        .expect_err("a device name is never a rename destination");
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    assert_eq!(get(&root, &["aux.c"]), b"aux.c");
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn entries_whose_names_creation_would_refuse_are_still_opened_renamed_and_removed() {
    let (root, path) = private_root("existing-device-like");
    // Bare `aux` and `con` are ordinary files to Win32 on this system. They exist already (made
    // here with the standard library and then protected like any workspace entry), so the
    // workspace must treat them like any other name.
    for name in ["aux", "con"] {
        fs::write(path.join(name), name.as_bytes()).expect("create an ordinary file");
        crate::win32::security::restrict_to_current_user(&path.join(name))
            .expect("protect the entry");
    }
    assert_eq!(get(&root, &["aux"]), b"aux");
    assert_eq!(
        root.read_dir_checked(&[])
            .expect("list entries named like devices")
            .len(),
        2
    );
    root.rename_relative(&["con"], &["moved"], false)
        .expect("an existing name is a valid rename source");
    root.remove_file_relative(&["aux"])
        .expect("an existing name is removable");
    assert_eq!(get(&root, &["moved"]), b"con");
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn source_listing_accepts_ordinary_files_named_like_dos_devices() {
    let dir = fresh_path("source-list");
    fs::create_dir(&dir).expect("create dir");
    for name in ["aux.c", "con.rs", "nul.txt", "com1.log", "ok.txt"] {
        fs::write(dir.join(name), b"x").expect("std creates it as an ordinary file");
    }
    let root = WorkspaceRoot::open_read_only_source(&dir).expect("open source root");
    let listing = root
        .read_dir_source_checked_limited(&[], 100)
        .expect("a source directory containing aux.c must still list");
    assert_eq!(listing.len(), 5);
    drop(root);
    fs::remove_dir_all(dir).expect("cleanup");
}

#[test]
fn a_source_root_beneath_an_ancestor_named_like_a_device_opens() {
    let dir = fresh_path("source-ancestor");
    let project = dir.join("aux").join("proj");
    fs::create_dir_all(&project).expect("std creates a directory named aux");
    WorkspaceRoot::open_read_only_source(&project)
        .expect("an ancestor directory named aux is an ordinary directory on this OS");
    fs::remove_dir_all(dir).expect("cleanup");
}

#[test]
fn removing_a_tree_with_device_like_entries_does_not_stop_half_way() {
    let (root, path) = private_root("tree-device-like");
    root.create_child_dir_exclusive("tree")
        .expect("create the tree");
    for name in ["aux.c", "con.rs", "a", "z"] {
        put(&root, &["tree", name], b"x");
    }
    root.remove_dir_tree(&["tree"])
        .expect("every entry of the tree is removed");
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

/// A virus scanner or indexer that has opened a file without sharing delete access and keeps it
/// until released, the way real-time protection does while it inspects what was just written.
///
/// Holding and releasing are ordered by handshake, never by a sleep: a test that lets a scanner
/// "hold for 60 ms" fails whenever a loaded machine delays the scanner thread past the bounded
/// wait it is exercising, which proves nothing about the code under test.
struct Scanner {
    release: std::sync::mpsc::Sender<()>,
    released: std::sync::mpsc::Receiver<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Scanner {
    /// Opens `path` and returns once the hold is in place.
    fn hold(path: PathBuf) -> Self {
        use std::os::windows::fs::OpenOptionsExt as _;
        use std::sync::mpsc;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

        let (held, ready) = mpsc::channel();
        let (release, release_requested) = mpsc::channel::<()>();
        let (closed, released) = mpsc::channel();
        let thread = std::thread::spawn(move || {
            let handle = fs::OpenOptions::new()
                .read(true)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
                .open(&path)
                .expect("scanner opens the file");
            held.send(()).expect("announce the hold");
            // A dropped sender also ends the hold, so a failing test cannot leak it.
            let _ = release_requested.recv();
            drop(handle);
            let _ = closed.send(());
        });
        ready.recv().expect("scanner holds the file");
        Self {
            release,
            released,
            thread,
        }
    }

    /// Lets go of the file and returns once the scanner's handle is closed.
    fn release(&self) {
        self.release.send(()).expect("scanner is still holding");
        self.released.recv().expect("scanner closed its handle");
    }

    fn finish(self) {
        drop(self.release);
        self.thread.join().expect("join scanner");
    }
}

/// The pause an operation takes after a refused attempt, wired to release a [`Scanner`]: the
/// first refusal proves the hold was real, and the very next attempt must therefore succeed.
struct ReleaseOnFirstRefusal<'a> {
    scanner: &'a Scanner,
    refusals: usize,
}

impl<'a> ReleaseOnFirstRefusal<'a> {
    fn new(scanner: &'a Scanner) -> Self {
        Self {
            scanner,
            refusals: 0,
        }
    }

    fn pause(&mut self, _delay: std::time::Duration) {
        self.refusals += 1;
        if self.refusals == 1 {
            self.scanner.release();
        }
    }
}

/// Renames with the scanner released at the first refusal, returning how many were seen.
fn rename_through_scanner(
    root: &WorkspaceRoot,
    scanner: &Scanner,
    source: &[&str],
    destination: &[&str],
    replace: bool,
) -> usize {
    let mut waiter = ReleaseOnFirstRefusal::new(scanner);
    root.rename_checked_entry_pausing(
        source,
        destination,
        replace,
        false,
        || {},
        flush_handle,
        &mut |delay| waiter.pause(delay),
    )
    .expect("the rename waits for the scanner and then succeeds");
    waiter.refusals
}

#[test]
fn a_scanner_holding_the_destination_is_waited_out_by_a_replacing_rename() {
    let (root, path) = private_root("scan-destination");
    put(&root, &["state"], b"old generation");
    put(&root, &["next"], b"new generation");

    let scanner = Scanner::hold(path.join("state"));
    let refusals = rename_through_scanner(&root, &scanner, &["next"], &["state"], true);
    scanner.finish();

    assert_eq!(refusals, 1, "refused once while held, then admitted");
    assert_eq!(get(&root, &["state"]), b"new generation");
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn a_scanner_holding_the_source_is_waited_out_by_rename_and_remove() {
    let (root, path) = private_root("scan-source");
    put(&root, &["next"], b"new generation");

    let scanner = Scanner::hold(path.join("next"));
    let refusals = rename_through_scanner(&root, &scanner, &["next"], &["state"], false);
    scanner.finish();
    assert_eq!(refusals, 1, "the source open was refused once while held");

    let scanner = Scanner::hold(path.join("state"));
    let mut waiter = ReleaseOnFirstRefusal::new(&scanner);
    root.remove_file_pausing(&["state"], &mut |delay| waiter.pause(delay))
        .expect("removal waits out the scanner");
    let refusals = waiter.refusals;
    scanner.finish();
    assert_eq!(refusals, 1, "the delete open was refused once while held");

    assert!(root.read_dir_checked(&[]).expect("list").is_empty());
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn a_holder_that_never_lets_go_fails_with_its_own_error_and_changes_nothing() {
    let (root, path) = private_root("scan-forever");
    put(&root, &["state"], b"old generation");
    put(&root, &["next"], b"new generation");

    let scanner = Scanner::hold(path.join("state"));
    // The pauses are recorded instead of slept: the wait is bounded by the schedule, and a
    // wall-clock bound would only measure how loaded the machine is.
    let mut pauses = Vec::new();
    let error = root
        .rename_checked_entry_pausing(
            &["next"],
            &["state"],
            true,
            false,
            || {},
            flush_handle,
            &mut |delay| pauses.push(delay),
        )
        .expect_err("a holder outlasting the wait is an error")
        .into_io_error();
    assert!(
        matches!(error.raw_os_error(), Some(5 | ERROR_SHARING_VIOLATION)),
        "the holder's own error is reported, got {error}"
    );
    assert_eq!(
        pauses, BACKOFF,
        "the wait follows the bounded schedule exactly"
    );
    assert_eq!(get(&root, &["state"]), b"old generation");
    assert_eq!(get(&root, &["next"]), b"new generation");

    scanner.finish();
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn republishing_beside_a_scanner_that_inspects_every_generation_never_fails() {
    let (root, path) = private_root("scan-loop");
    put(&root, &["state"], b"generation 0");

    // Real-time protection opens each file the moment it changes and holds it without sharing
    // delete while it scans, which is exactly when the next publication arrives. Every
    // generation here is therefore scanned before it is replaced, and each replacement must be
    // refused exactly once and then succeed once the scan ends.
    for generation in 1..=100_u32 {
        let scanner = Scanner::hold(path.join("state"));
        let temporary = format!(".state.{generation}.tmp");
        put(
            &root,
            &[temporary.as_str()],
            format!("generation {generation}").as_bytes(),
        );
        let refusals =
            rename_through_scanner(&root, &scanner, &[temporary.as_str()], &["state"], true);
        scanner.finish();
        assert_eq!(refusals, 1, "generation {generation}");
    }

    assert_eq!(get(&root, &["state"]), b"generation 100");
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

/// Opens `state` through [`open_admitted_file`] while a publisher replaces the name `replacements`
/// times, each time between the open and the admission checks, returning the outcome, how many
/// times the checks ran, and the bytes of whatever generation was opened.
fn open_state_while_published(
    root: &WorkspaceRoot,
    if_unlinked: IfUnlinked,
    replacements: usize,
) -> (io::Result<Vec<u8>>, usize) {
    use std::cell::Cell;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_READ_ATTRIBUTES, FILE_READ_DATA, READ_CONTROL, SYNCHRONIZE,
    };

    let admissions = Cell::new(0_usize);
    let opened = open_admitted_file(
        root.handle(),
        ExistingName::parse("state").expect("a valid name"),
        FILE_READ_DATA | FILE_READ_ATTRIBUTES | READ_CONTROL | SYNCHRONIZE,
        if_unlinked,
        |handle, linkage| {
            let ordinal = admissions.get();
            admissions.set(ordinal + 1);
            if ordinal < replacements {
                let next = format!(".next.{ordinal}.tmp");
                put(
                    root,
                    &[next.as_str()],
                    format!("generation {}", ordinal + 1).as_bytes(),
                );
                root.rename_relative(&[next.as_str()], &["state"], true)
                    .expect("the publisher replaces the name between open and admission");
            }
            ensure_regular_file_handle_with(handle, linkage)?;
            ensure_private_handle(handle)
        },
    );
    let bytes = opened.and_then(|handle| {
        let mut bytes = Vec::new();
        file_from_handle(handle)?.read_to_end(&mut bytes)?;
        Ok(bytes)
    });
    (bytes, admissions.get())
}

#[test]
fn a_reader_keeps_the_generation_it_opened_when_a_publisher_unlinks_it_before_the_checks() {
    let (root, path) = private_root("keep-generation");
    put(&root, &["state"], b"generation 0");

    // The publisher replaces the name at every admission, so a reader that insisted on the
    // current generation could never settle. Keeping the opened one settles at once.
    let (outcome, admissions) = open_state_while_published(&root, IfUnlinked::Keep, usize::MAX);
    assert_eq!(
        outcome.expect("the opened generation is admitted"),
        b"generation 0"
    );
    assert_eq!(admissions, 1, "no second open was needed");

    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn a_writer_reopens_the_name_after_a_pause_and_gives_up_on_a_name_that_never_settles() {
    let (root, path) = private_root("reopen-generation");
    put(&root, &["state"], b"generation 0");

    let (outcome, admissions) = open_state_while_published(&root, IfUnlinked::Reopen, 3);
    assert_eq!(
        outcome.expect("the writer settles on the current generation"),
        b"generation 3"
    );
    assert_eq!(admissions, 4, "three replaced opens, then the settled one");

    let (outcome, admissions) = open_state_while_published(&root, IfUnlinked::Reopen, usize::MAX);
    let error = outcome.expect_err("a name replaced at every open never settles");
    assert_eq!(error.kind(), std::io::ErrorKind::ResourceBusy);
    assert_eq!(
        admissions,
        BACKOFF.len() + 1,
        "the retries follow the bounded schedule"
    );

    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn a_reader_is_never_starved_by_a_hot_publisher() {
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::thread;

    const PAYLOAD: usize = 4096;
    const READS: usize = 2_000;
    let (root, path) = private_root("hot-publisher");
    put(&root, &["state"], &[0xAA; PAYLOAD]);
    let finished = Arc::new(AtomicBool::new(false));
    let writer_root = root.clone();
    let writer_flag = Arc::clone(&finished);
    let writer = thread::spawn(move || {
        let mut round = 0_usize;
        while !writer_flag.load(Ordering::Relaxed) {
            let temporary = format!(".state.{round}.tmp");
            put(&writer_root, &[temporary.as_str()], &[0xBB; PAYLOAD]);
            writer_root
                .rename_relative(&[temporary.as_str()], &["state"], true)
                .expect("publish");
            round += 1;
        }
    });
    let mut vanished = 0_usize;
    for read in 0..READS {
        let mut bytes = Vec::new();
        let mut opened = match root.open_file_read_checked(&["state"]) {
            Ok(opened) => opened,
            // A POSIX-semantics replace unlinks the old name before it links the new one, and a
            // lookup that lands between the two reports the name missing. That platform gap is
            // not what this test is about; starvation would surface as `ResourceBusy` below.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                vanished += 1;
                continue;
            }
            Err(error) => panic!("read {read} of a published name failed: {error}"),
        };
        opened
            .read_to_end(&mut bytes)
            .expect("read a published generation");
        assert_eq!(bytes.len(), PAYLOAD);
        assert!(bytes.iter().all(|byte| *byte == bytes[0]));
    }
    eprintln!("{vanished} of {READS} lookups landed in the replace gap");
    finished.store(true, Ordering::Relaxed);
    writer.join().expect("join writer");
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

/// A destination carrying `FILE_ATTRIBUTE_READONLY` (set by a user or a backup tool) is replaced
/// like any other: Unix `rename` consults the directory's permissions and never the replaced
/// file's mode. Before the fix this was refused with `ACCESS_DENIED`, which the busy-file retry
/// mistook for a scanner and waited out for its whole schedule.
#[test]
fn replacing_a_read_only_destination_behaves_like_unix_rename() {
    let (root, path) = private_root("read-only-destination");
    put(&root, &["state"], b"old");
    let mut permissions = fs::metadata(path.join("state"))
        .expect("stat")
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path.join("state"), permissions).expect("mark read-only");
    put(&root, &["next"], b"new");

    let mut pauses = Vec::new();
    root.rename_checked_entry_pausing(
        &["next"],
        &["state"],
        true,
        false,
        || {},
        flush_handle,
        &mut |delay| pauses.push(delay),
    )
    .expect("replace over a read-only destination");

    assert!(
        pauses.is_empty(),
        "a permanent refusal was never retried: {pauses:?}"
    );
    assert_eq!(get(&root, &["state"]), b"new");
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

// ---- end-to-end coverage of the private-DACL rules -------------------------------------------
//
// `only_full_control_masks_count_as_a_private_grant` tests one pure function. These drive
// `ensure_private_handle` through the public checked opens with real security descriptors, so
// every clause of "one protected, non-inheritable allow ACE granting full control to the current
// user" is needed by some test: weakening any clause, or accepting `FILE_ALL_ACCESS` in a way
// that widens access, makes one of them fail.

/// How the replacement DACL relates to the parent's.
#[derive(Clone, Copy)]
enum Inheritance {
    /// Blocks inherited entries (`SE_DACL_PROTECTED`), as the workspace requires.
    Protected,
    /// Leaves the DACL open to inheritance.
    Open,
}

fn current_sid_text() -> String {
    sid_text(
        crate::win32::identity::current_user()
            .expect("current user")
            .as_bytes(),
    )
    .expect("SID text")
}

/// A SID of the same shape as `sid` whose last relative identifier is one higher.
fn neighbour_sid_text(sid: &str) -> String {
    let (prefix, rid) = sid.rsplit_once('-').expect("a SID has several parts");
    let rid: u32 = rid.parse().expect("the last part is numeric");
    format!("{prefix}-{}", rid + 1)
}

/// Replaces the DACL of `path` with the one `sddl` describes.
fn set_dacl(path: &Path, sddl: &str, inheritance: Inheritance) {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
    };
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
        UNPROTECTED_DACL_SECURITY_INFORMATION,
    };

    let wide_sddl = sddl.encode_utf16().chain([0]).collect::<Vec<_>>();
    let wide_path = path
        .as_os_str()
        .encode_wide()
        .chain([0])
        .collect::<Vec<_>>();
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: both buffers are NUL-terminated and outlive the call; the output pointer is a
    // writable local, and the allocation it receives is released below.
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide_sddl.as_ptr(),
            1,
            &raw mut descriptor,
            std::ptr::null_mut(),
        )
    };
    assert_ne!(converted, 0, "convert {sddl}");
    let (mut present, mut defaulted) = (0, 0);
    let mut dacl = std::ptr::null_mut();
    // SAFETY: `descriptor` is the live descriptor returned above and the outputs are locals.
    let read = unsafe {
        GetSecurityDescriptorDacl(
            descriptor,
            &raw mut present,
            &raw mut dacl,
            &raw mut defaulted,
        )
    };
    assert_ne!(read, 0, "read the DACL of {sddl}");
    let protection = match inheritance {
        Inheritance::Protected => PROTECTED_DACL_SECURITY_INFORMATION,
        Inheritance::Open => UNPROTECTED_DACL_SECURITY_INFORMATION,
    };
    // SAFETY: the path is NUL-terminated, and `dacl` points into `descriptor`, which is alive
    // until the `LocalFree` below.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide_path.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | protection,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            dacl,
            std::ptr::null_mut(),
        )
    };
    // SAFETY: the descriptor was allocated by the convert call with `LocalAlloc`.
    unsafe { LocalFree(descriptor) };
    assert_eq!(status, 0, "set DACL {sddl}");
}

/// Runs the private-DACL admission on `path` through a handle that asks for nothing but
/// `READ_CONTROL`, which the owner always holds. Admission is judged by the rules themselves:
/// the checked opens also need `SYNCHRONIZE`, so a DACL that withholds it from the owner would
/// fail the open and never reach them.
fn admits_as_private(path: &Path) -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, READ_CONTROL,
    };

    let handle = fs::OpenOptions::new()
        .access_mode(READ_CONTROL)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)?;
    ensure_private_handle(handle.as_raw_handle())
}

/// Restores the private DACL so the owner can remove a probe.
fn restore_private(path: &Path, me: &str) {
    set_dacl(path, &format!("D:P(A;;FA;;;{me})"), Inheritance::Protected);
}

#[test]
fn only_one_protected_full_control_allow_for_the_current_user_is_a_private_file() {
    let (root, path) = private_root("private-file-acl");
    let me = current_sid_text();
    for (label, sddl, inheritance, admitted) in [
        (
            "owner full control",
            format!("D:P(A;;FA;;;{me})"),
            Inheritance::Protected,
            true,
        ),
        (
            "everyone full control",
            "D:P(A;;FA;;;WD)".to_owned(),
            Inheritance::Protected,
            false,
        ),
        (
            // The same SID shape (so the ACE is exactly as long as the owner's), one RID off.
            "another principal with full control",
            format!("D:P(A;;FA;;;{})", neighbour_sid_text(&me)),
            Inheritance::Protected,
            false,
        ),
        (
            // An allow ACE of another type (callback) with the owner's SID and a mask that
            // grants everything, whose condition holds, so the open itself succeeds.
            "owner full control through a conditional ACE",
            format!("D:P(XA;;FA;;;{me};(1==1))"),
            Inheritance::Protected,
            false,
        ),
        (
            "owner read only",
            format!("D:P(A;;FR;;;{me})"),
            Inheritance::Protected,
            false,
        ),
        (
            "owner write only",
            format!("D:P(A;;FW;;;{me})"),
            Inheritance::Protected,
            false,
        ),
        (
            "owner denied",
            format!("D:P(D;;FA;;;{me})"),
            Inheritance::Protected,
            false,
        ),
        (
            "owner full control plus everyone read",
            format!("D:P(A;;FA;;;{me})(A;;FR;;;WD)"),
            Inheritance::Protected,
            false,
        ),
        (
            "owner full control but open to inheritance",
            format!("D:(A;;FA;;;{me})"),
            Inheritance::Open,
            false,
        ),
    ] {
        put(&root, &["probe"], b"x");
        set_dacl(&path.join("probe"), &sddl, inheritance);
        let outcome = admits_as_private(&path.join("probe"));
        assert_eq!(outcome.is_ok(), admitted, "{label}: {sddl}: {outcome:?}");
        restore_private(&path.join("probe"), &me);
        root.remove_file_relative(&["probe"]).expect("remove probe");
    }
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

#[test]
fn an_inheritable_allow_entry_does_not_make_a_directory_private() {
    let (root, path) = private_root("private-directory-acl");
    let me = current_sid_text();
    for (label, sddl, admitted) in [
        (
            "owner full, not inheritable",
            format!("D:P(A;;FA;;;{me})"),
            true,
        ),
        (
            "owner full, inheritable",
            format!("D:P(A;OICI;FA;;;{me})"),
            false,
        ),
        (
            "owner full, inherit-only",
            format!("D:P(A;OICIIO;FA;;;{me})"),
            false,
        ),
    ] {
        root.create_child_dir_exclusive("probe").expect("probe dir");
        set_dacl(&path.join("probe"), &sddl, Inheritance::Protected);
        let outcome = admits_as_private(&path.join("probe"));
        assert_eq!(outcome.is_ok(), admitted, "{label}: {sddl}: {outcome:?}");
        restore_private(&path.join("probe"), &me);
        root.remove_empty_dir(&["probe"]).expect("remove probe");
    }
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

/// This layer resolves exactly the name it is given and does no canonicalization, so on a volume
/// that generates 8.3 aliases the short name of an entry opens that entry. Callers that decide
/// anything from a name must compare the entry's identity (or the long names an enumeration
/// returns), never the string they were handed: `LONGFI~1.TXT` and `longfilename_sample.txt`
/// are one file.
#[test]
fn a_short_name_alias_opens_the_entry_it_aliases() {
    let (root, path) = private_root("short-name-alias");
    put(&root, &["longfilename_sample.txt"], b"long name");
    if fs::metadata(path.join("LONGFI~1.TXT")).is_err() {
        eprintln!("skipping: this volume does not generate 8.3 aliases");
        drop(root);
        fs::remove_dir_all(path).expect("cleanup");
        return;
    }
    assert_eq!(get(&root, &["LONGFI~1.TXT"]), b"long name");
    let listed = root.read_dir_checked(&[]).expect("list");
    assert_eq!(
        listed
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["longfilename_sample.txt"],
        "enumeration reports the long name only"
    );
    // The alias also collides on exclusive creation instead of creating a second entry.
    let error = root
        .create_file_exclusive(&["LONGFI~1.TXT"])
        .expect_err("the alias is taken");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}

// ---- directory enumeration identity ------------------------------------------------------------

/// Lays out `FILE_ID_EXTD_DIR_INFO` records the way `GetFileInformationByHandleEx` does: each
/// padded to the record alignment and chained by `NextEntryOffset`, the last one ending the chain.
fn forged_directory_buffer(records: &[(&str, [u8; 16])], capacity_words: usize) -> Vec<u64> {
    use std::mem::offset_of;
    use windows_sys::Win32::Storage::FileSystem::FILE_ID_EXTD_DIR_INFO;

    let name_offset = offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
    let alignment = align_of::<FILE_ID_EXTD_DIR_INFO>();
    let mut bytes = Vec::<u8>::new();
    for (index, (name, id)) in records.iter().enumerate() {
        let wide = name.encode_utf16().collect::<Vec<_>>();
        let name_bytes = wide.len() * 2;
        let length = (name_offset + name_bytes).next_multiple_of(alignment);
        let mut record = vec![0_u8; length];
        let last = index + 1 == records.len();
        let next = if last {
            0
        } else {
            u32::try_from(length).expect("record fits")
        };
        record[..4].copy_from_slice(&next.to_le_bytes());
        let id_offset = offset_of!(FILE_ID_EXTD_DIR_INFO, FileId);
        record[id_offset..id_offset + 16].copy_from_slice(id);
        let length_offset = offset_of!(FILE_ID_EXTD_DIR_INFO, FileNameLength);
        record[length_offset..length_offset + 4]
            .copy_from_slice(&u32::try_from(name_bytes).expect("name fits").to_le_bytes());
        for (unit, chunk) in wide.iter().zip(record[name_offset..].chunks_exact_mut(2)) {
            chunk.copy_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&record);
    }
    let mut buffer = vec![0_u64; capacity_words.max(bytes.len().div_ceil(8))];
    for (word, chunk) in buffer.iter_mut().zip(bytes.chunks(8)) {
        let mut padded = [0_u8; 8];
        padded[..chunk.len()].copy_from_slice(chunk);
        *word = u64::from_le_bytes(padded);
    }
    buffer
}

fn decode(buffer: &[u64], maximum: usize) -> io::Result<Vec<EnumeratedEntry>> {
    let mut names = Vec::new();
    decode_directory_records(buffer, &mut names, maximum)?;
    Ok(names)
}

#[test]
fn enumerated_ids_keep_all_128_bits_so_objects_that_differ_above_the_low_half_stay_distinct() {
    // ReFS ids use the whole 128 bits. These two differ only above the low eight bytes, which is
    // all an eight-byte comparison looks at.
    let mut first = [0_u8; 16];
    first[0] = 7;
    let mut second = first;
    second[15] = 0x40;
    assert_eq!(
        first[..8],
        second[..8],
        "the fixture hides the difference from a low-half test"
    );

    let buffer = forged_directory_buffer(&[("first", first), ("second", second)], 64);
    let entries = decode(&buffer, 10).expect("well-formed records decode");
    assert_eq!(
        entries,
        [
            EnumeratedEntry {
                name: "first".to_owned(),
                id: FileId128(first)
            },
            EnumeratedEntry {
                name: "second".to_owned(),
                id: FileId128(second)
            },
        ]
    );
    assert_ne!(entries[0].id, entries[1].id);
}

#[test]
fn dot_entries_are_skipped_and_the_entry_limit_is_enforced() {
    let id = [3_u8; 16];
    let buffer = forged_directory_buffer(&[(".", id), ("..", id), ("a", id), ("b", id)], 64);
    let names = decode(&buffer, 2)
        .expect("two real entries fit a limit of two")
        .into_iter()
        .map(|entry| entry.name)
        .collect::<Vec<_>>();
    assert_eq!(names, ["a", "b"]);
    let error = decode(&buffer, 1).expect_err("a third entry exceeds the limit");
    assert_eq!(error.kind(), std::io::ErrorKind::FileTooLarge);
}

#[test]
fn malformed_enumeration_buffers_are_refused_without_reading_out_of_bounds() {
    use std::mem::offset_of;
    use windows_sys::Win32::Storage::FileSystem::FILE_ID_EXTD_DIR_INFO;

    let id = [1_u8; 16];
    let name_offset = offset_of!(FILE_ID_EXTD_DIR_INFO, FileName);
    let length_offset = offset_of!(FILE_ID_EXTD_DIR_INFO, FileNameLength);
    let word = |buffer: &mut Vec<u64>, byte: usize, value: u32| {
        let index = byte / 8;
        let shift = (byte % 8) * 8;
        buffer[index] = (buffer[index] & !(0xffff_ffff_u64 << shift)) | (u64::from(value) << shift);
    };

    // A name that claims to run past the end of the buffer.
    let mut buffer = forged_directory_buffer(&[("a", id)], 8);
    word(&mut buffer, length_offset, 4096);
    assert_eq!(
        decode(&buffer, 10).expect_err("oversized name").kind(),
        std::io::ErrorKind::InvalidData
    );

    // A name length that is not a whole number of UTF-16 units.
    let mut buffer = forged_directory_buffer(&[("ab", id)], 8);
    word(&mut buffer, length_offset, 3);
    assert_eq!(
        decode(&buffer, 10).expect_err("odd name length").kind(),
        std::io::ErrorKind::InvalidData
    );

    // A name that is not valid UTF-16 (a lone surrogate).
    let mut buffer = forged_directory_buffer(&[("a", id)], 8);
    buffer[name_offset / 8] |= 0xD800_u64 << ((name_offset % 8) * 8);
    assert_eq!(
        decode(&buffer, 10).expect_err("lone surrogate").kind(),
        std::io::ErrorKind::InvalidData
    );

    // Chain offsets that loop, are not aligned, or leave the buffer.
    for next in [0x1_u32, 0x3, 0x10_0000, u32::MAX] {
        let mut buffer = forged_directory_buffer(&[("a", id), ("b", id)], 8);
        word(&mut buffer, 0, next);
        assert_eq!(
            decode(&buffer, 10).expect_err("bad chain offset").kind(),
            std::io::ErrorKind::InvalidData,
            "next entry offset {next:#x}"
        );
    }

    // A buffer too small to hold even one record header.
    assert_eq!(
        decode(&[0_u64; 2], 10)
            .expect_err("a truncated header")
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}

#[test]
fn a_real_enumeration_reports_the_ids_its_handles_report() {
    let (root, path) = private_root("enumeration-ids");
    for name in ["alpha", "beta", "gamma.txt"] {
        put(&root, &[name], name.as_bytes());
    }
    root.create_child_dir_exclusive("delta")
        .expect("create a directory");

    let entries = enumerate_names(root.handle(), 100).expect("enumerate");
    assert_eq!(entries.len(), 4);
    for entry in entries {
        let handle = open_relative(
            root.handle(),
            ExistingName::parse(&entry.name).expect("a valid name"),
            FILE_READ_ATTRIBUTES,
            0,
            Sharing::Transient,
        )
        .expect("open an enumerated child");
        assert_eq!(
            FileId128::of_handle(handle.as_raw_handle()).expect("handle id"),
            entry.id,
            "{}",
            entry.name
        );
    }
    drop(root);
    fs::remove_dir_all(path).expect("cleanup");
}
