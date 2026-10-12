//! The pinned F32 CPU recipe uses the invoking thread, without Rayon OS workers.
//!
//! Candle's F32 GEMM separately reads RAYON_NUM_THREADS. Both this exact launch
//! setting and the global pool must be admitted before tokenizer/model operations.
//! This recipe never calls Device::with_context or uses quantized barrier pools.

use std::{ffi::OsStr, fmt};

#[derive(Clone, Copy, Debug)]
pub(super) enum CpuExecutionPolicy {
    SingleCallingThread,
}

#[derive(Debug)]
pub(super) enum CpuExecutionError {
    MissingThreadPolicy,
    UnexpectedThreadPolicy,
    PoolInitialization(rayon::ThreadPoolBuildError),
}

impl fmt::Display for CpuExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingThreadPolicy => f.write_str(
                "CPU recipe requires RAYON_NUM_THREADS=1 in its admitted launch environment",
            ),
            Self::UnexpectedThreadPolicy => {
                f.write_str("CPU recipe refuses a RAYON_NUM_THREADS setting other than exactly 1")
            }
            Self::PoolInitialization(error) => {
                write!(f, "CPU calling-thread pool initialization refused: {error}")
            }
        }
    }
}

impl std::error::Error for CpuExecutionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::PoolInitialization(error) => Some(error),
            _ => None,
        }
    }
}

impl CpuExecutionPolicy {
    pub(super) fn from_environment() -> Result<Self, CpuExecutionError> {
        Self::admit(std::env::var_os("RAYON_NUM_THREADS").as_deref())
    }

    fn admit(value: Option<&OsStr>) -> Result<Self, CpuExecutionError> {
        match value {
            Some(value) if value == OsStr::new("1") => Ok(Self::SingleCallingThread),
            Some(_) => Err(CpuExecutionError::UnexpectedThreadPolicy),
            None => Err(CpuExecutionError::MissingThreadPolicy),
        }
    }

    pub(super) fn initialize(self) -> Result<(), CpuExecutionError> {
        match self {
            Self::SingleCallingThread => rayon::ThreadPoolBuilder::new()
                .num_threads(1)
                .use_current_thread()
                .build_global()
                .map_err(CpuExecutionError::PoolInitialization),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use rayon::prelude::*;

    #[test]
    fn cpu_policy_refuses_missing_or_noncanonical_thread_settings() {
        assert!(matches!(
            CpuExecutionPolicy::admit(None),
            Err(CpuExecutionError::MissingThreadPolicy)
        ));
        for value in ["", "0", "2", "01", " 1", "1 ", "default"] {
            assert!(matches!(
                CpuExecutionPolicy::admit(Some(OsStr::new(value))),
                Err(CpuExecutionError::UnexpectedThreadPolicy)
            ));
        }
        assert!(matches!(
            CpuExecutionPolicy::admit(Some(OsStr::new("1"))),
            Ok(CpuExecutionPolicy::SingleCallingThread)
        ));
    }

    #[test]
    fn cpu_current_thread_pool_has_no_extra_tasks_and_refuses_reinitialization() {
        // A subprocess gives the one-shot global registry its genuine fresh process.
        // This is a Rayon scheduling control, not model or inference proof.
        const CHILD: &str = "NUDOX_TEST_CPU_POLICY_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let result = std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args(["--exact", "cpu_execution::tests::cpu_current_thread_pool_has_no_extra_tasks_and_refuses_reinitialization", "--test-threads=1"])
                .env(CHILD, "1")
                .env("RAYON_NUM_THREADS", "1")
                .output()
                .expect("fresh policy test process");
            assert!(
                result.status.success(),
                "stdout={} stderr={}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(
                String::from_utf8_lossy(&result.stdout).contains("1 passed"),
                "child must execute its exact control, not select zero tests"
            );
            return;
        }
        #[cfg(target_os = "linux")]
        let tasks_before = std::fs::read_dir("/proc/self/task")
            .expect("tasks before")
            .count();
        CpuExecutionPolicy::from_environment()
            .expect("exact environment")
            .initialize()
            .expect("fresh global pool");
        assert_eq!(rayon::current_num_threads(), 1);
        assert_eq!(candle_core::utils::get_num_threads(), 1);
        let invoking = std::thread::current().id();
        let sum = (0_u64..4096)
            .into_par_iter()
            .map(|value| {
                assert_eq!(std::thread::current().id(), invoking);
                value
            })
            .sum::<u64>();
        assert_eq!(sum, (0_u64..4096).sum::<u64>());
        #[cfg(target_os = "linux")]
        assert_eq!(
            std::fs::read_dir("/proc/self/task")
                .expect("tasks after")
                .count(),
            tasks_before
        );
        assert!(matches!(
            CpuExecutionPolicy::SingleCallingThread.initialize(),
            Err(CpuExecutionError::PoolInitialization(_))
        ));
    }
}
