use super::*;
use std::fs;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);
fn scratch() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "nudox-stable-read-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&root).expect("scratch");
    root
}

#[test]
fn unchanged_regular_file_is_admitted_at_exact_bound() {
    let root = scratch();
    let path = root.join("lockfile");
    fs::write(&path, b"original").unwrap();
    assert_eq!(read_regular_bounded_stable(&path, 8).unwrap(), b"original");
    assert_eq!(
        read_regular_bounded_stable(&path, 7).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn same_size_held_file_mutation_is_refused_after_read() {
    let root = scratch();
    let path = root.join("lockfile");
    fs::write(&path, b"original").unwrap();
    let file = open_regular(&path).unwrap();
    let error = read_stable_with(file, 8, |file, length, maximum| {
        let bytes = read_bounded(file, length, maximum)?;
        let mut writer = fs::OpenOptions::new().write(true).open(&path)?;
        writer.write_all(b"modified")?;
        writer.set_times(
            fs::FileTimes::new()
                .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(946684800)),
        )?;
        Ok(bytes)
    })
    .expect_err("same byte length must not conceal changed revision");
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert_eq!(fs::read(&path).unwrap(), b"modified");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn truncation_and_growth_are_refused_on_the_same_descriptor() {
    for length in [3, 12] {
        let root = scratch();
        let path = root.join("lockfile");
        fs::write(&path, b"original").unwrap();
        let file = open_regular(&path).unwrap();
        let error = read_stable_with(file, 16, |file, length_before, maximum| {
            let bytes = read_bounded(file, length_before, maximum)?;
            fs::OpenOptions::new()
                .write(true)
                .open(&path)?
                .set_len(length)?;
            Ok(bytes)
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        fs::remove_dir_all(root).unwrap();
    }
}

#[cfg(unix)]
#[test]
fn stable_reader_rejects_fifo_and_symlink_without_waiting_for_a_writer() {
    let root = scratch();
    fs::write(root.join("regular"), b"x").unwrap();
    std::os::unix::fs::symlink("regular", root.join("link")).unwrap();
    assert!(read_regular_bounded_stable(&root.join("link"), 8).is_err());
    #[cfg(not(target_os = "macos"))]
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        root.join("fifo"),
        rustix::fs::Mode::from_bits_truncate(0o600),
    )
    .unwrap();
    #[cfg(target_os = "macos")]
    assert!(
        std::process::Command::new("mkfifo")
            .args(["-m", "600"])
            .arg(root.join("fifo"))
            .status()
            .unwrap()
            .success()
    );
    let start = std::time::Instant::now();
    assert!(read_regular_bounded_stable(&root.join("fifo"), 8).is_err());
    assert!(start.elapsed() < std::time::Duration::from_secs(1));
    fs::remove_dir_all(root).unwrap();
}
