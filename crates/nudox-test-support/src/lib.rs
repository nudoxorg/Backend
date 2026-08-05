//! Small, dependency-free helpers for measuring integration tests.
//!
//! The measurements are deliberately best-effort. A missing platform counter
//! must not turn a functional test into a platform test, while wall time and
//! disk deltas remain available everywhere.
//!
//! This crate has zero production dependencies — RSS is read from the OS via
//! thin FFI bindings (getrusage on Unix, /proc/self/status fallback on Linux).

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

/// Cost metadata collected around one test case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RunCost {
    /// Wall-clock duration of the measured closure.
    pub wall: Duration,
    /// Best-effort process high-water RSS. `None` when the host does not expose
    /// a supported counter.
    pub peak_rss_bytes: Option<u64>,
    /// Recursive byte-size change under the case directory.
    pub disk_delta_bytes: i64,
}

/// Measure one case and print a stable line suitable for nextest/CI parsers.
pub fn measured<T>(case: &str, directory: &Path, run: impl FnOnce() -> T) -> (T, RunCost) {
    let before_disk = disk_bytes(directory);
    let before_rss = process_rss_bytes();
    let started = Instant::now();
    let value = run();
    let after_rss = process_rss_bytes();
    let cost = RunCost {
        wall: started.elapsed(),
        peak_rss_bytes: match (before_rss, after_rss) {
            (Some(before), Some(after)) => Some(before.max(after)),
            (Some(rss), None) | (None, Some(rss)) => Some(rss),
            (None, None) => None,
        },
        disk_delta_bytes: (disk_bytes(directory) as i128)
            .saturating_sub(before_disk as i128)
            .clamp(i64::MIN as i128, i64::MAX as i128) as i64,
    };

    println!(
        "cost case={case} wall_ms={} rss_bytes={} disk_delta_bytes={}",
        cost.wall.as_secs_f64() * 1_000.0,
        cost.peak_rss_bytes
            .map_or_else(|| "unknown".to_owned(), |rss| rss.to_string()),
        cost.disk_delta_bytes,
    );
    (value, cost)
}

/// Recursively count regular-file bytes. Errors and disappearing files are
/// ignored because a concurrent cleanup must not make a test measurement panic.
pub fn disk_bytes(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    if !metadata.is_dir() {
        return 0;
    }

    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| disk_bytes(&entry.path()))
        .sum()
}

fn process_rss_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let status = fs::read_to_string("/proc/self/status").ok()?;
        let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
        let kib = line.split_whitespace().nth(1)?.parse::<u64>().ok()?;
        return kib.checked_mul(1024);
    }

    #[cfg(target_os = "macos")]
    {
        rusage_maxrss_bytes()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
mod macos_rss {
    use std::mem::MaybeUninit;

    #[repr(C)]
    struct Timeval {
        tv_sec: isize,
        tv_usize: isize,
    }

    #[repr(C)]
    struct Rusage {
        ru_utime: Timeval,
        ru_stime: Timeval,
        ru_maxrss: isize,
        ru_ixrss: isize,
        ru_idrss: isize,
        ru_isrss: isize,
        ru_minflt: isize,
        ru_majflt: isize,
        ru_nswap: isize,
        ru_inblock: isize,
        ru_oublock: isize,
        ru_msgsnd: isize,
        ru_msgrcv: isize,
        ru_nsignals: isize,
        ru_nvcsw: isize,
        ru_nivcsw: isize,
    }

    const RUSAGE_SELF: i32 = 0;

    unsafe extern "C" {
        fn getrusage(who: i32, usage: *mut Rusage) -> i32;
    }

    pub fn maxrss_bytes() -> Option<u64> {
        let mut usage = MaybeUninit::<Rusage>::uninit();
        let rc = unsafe { getrusage(RUSAGE_SELF, usage.as_mut_ptr()) };
        if rc != 0 {
            return None;
        }
        let r = unsafe { usage.assume_init() };
        // On macOS (and Apple Silicon) ru_maxrss is in bytes.
        u64::try_from(r.ru_maxrss).ok()
    }
}

#[cfg(target_os = "macos")]
fn rusage_maxrss_bytes() -> Option<u64> {
    macos_rss::maxrss_bytes()
}

#[cfg(test)]
mod tests {
    use super::{disk_bytes, measured};
    use std::fs;

    #[test]
    fn disk_bytes_counts_nested_regular_files() {
        let directory = tempfile_dir();
        fs::write(directory.join("one"), [0_u8; 3]).expect("write one");
        fs::create_dir(directory.join("nested")).expect("create nested");
        fs::write(directory.join("nested/two"), [0_u8; 7]).expect("write two");
        assert_eq!(disk_bytes(&directory), 10);
        let _ = fs::remove_dir_all(directory);
    }

    #[test]
    fn measured_returns_value_and_disk_delta() {
        let directory = tempfile_dir();
        let (value, cost) = measured("test-support-delta", &directory, || {
            fs::write(directory.join("payload"), [0_u8; 11]).expect("write payload");
            42
        });
        assert_eq!(value, 42);
        assert_eq!(cost.disk_delta_bytes, 11);
        let _ = fs::remove_dir_all(directory);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "nudox-test-support-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("create test directory");
        path
    }
}
