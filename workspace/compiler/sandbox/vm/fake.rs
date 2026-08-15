//! Recording, non-booting [`VmRuntime`] for projection tests.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::spec::Output;
use crate::vm::{GoldenId, RunSpec, VmConfig, VmError, VmHandle, VmRuntime};

/// A recording, non-booting [`VmRuntime`] for tests.
///
/// Records every launched [`VmConfig`], every [`RunSpec`] exec'd, and every
/// golden fork; execs pop scripted results (default: empty success output).
#[derive(Debug, Default, Clone)]
pub struct FakeVmRuntime {
    inner: Arc<FakeState>,
}

#[derive(Debug, Default)]
struct FakeState {
    launched: Mutex<Vec<VmConfig>>,
    forked: Mutex<Vec<GoldenId>>,
    execs: Mutex<Vec<RunSpec>>,
    scripted: Mutex<VecDeque<Result<Output, VmError>>>,
}

impl FakeVmRuntime {
    /// Fresh empty fake.
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue a scripted exec result; execs consume the queue front-first.
    pub fn script_exec(&self, result: Result<Output, VmError>) {
        self.inner
            .scripted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push_back(result);
    }

    /// Every config launched so far.
    pub fn launched(&self) -> Vec<VmConfig> {
        self.inner
            .launched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Every golden fork requested so far.
    pub fn forked(&self) -> Vec<GoldenId> {
        self.inner
            .forked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Every [`RunSpec`] exec'd so far (across all handles).
    pub fn execs(&self) -> Vec<RunSpec> {
        self.inner
            .execs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

/// Handle produced by [`FakeVmRuntime`].
#[derive(Debug)]
pub struct FakeVmHandle {
    inner: Arc<FakeState>,
    killed: AtomicBool,
}

impl FakeVmHandle {
    /// Whether [`VmHandle::kill`] was called on this handle.
    pub fn is_killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }
}

/// A zero exit status for fabricated fake outputs.
#[cfg(unix)]
fn exit_ok() -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(0)
}

impl VmHandle for FakeVmHandle {
    fn exec(&mut self, spec: &RunSpec) -> Result<Output, VmError> {
        self.inner
            .execs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(spec.clone());
        if let Some(result) = self
            .inner
            .scripted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
        {
            return result;
        }
        #[cfg(unix)]
        {
            Ok(Output {
                stdout: Vec::new(),
                stderr: Vec::new(),
                end: crate::spec::ProcessEnd::Exited(exit_ok()),
                wall: Duration::ZERO,
                peak_mem: None,
            })
        }
        #[cfg(not(unix))]
        {
            Err(VmError::Unsupported {
                reason: "FakeVmRuntime default output requires unix".into(),
            })
        }
    }

    fn kill(&mut self) {
        self.killed.store(true, Ordering::SeqCst);
    }
}

impl VmRuntime for FakeVmRuntime {
    type Handle = FakeVmHandle;

    fn launch(&self, cfg: &VmConfig) -> Result<Self::Handle, VmError> {
        self.inner
            .launched
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(cfg.clone());
        Ok(FakeVmHandle {
            inner: Arc::clone(&self.inner),
            killed: AtomicBool::new(false),
        })
    }

    fn fork_golden(&self, golden: &GoldenId) -> Result<Self::Handle, VmError> {
        self.inner
            .forked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(golden.clone());
        Ok(FakeVmHandle {
            inner: Arc::clone(&self.inner),
            killed: AtomicBool::new(false),
        })
    }

    fn checkpoint(&self, _handle: Self::Handle) -> Result<GoldenId, VmError> {
        Ok(GoldenId::new(format!(
            "fake-golden-{}",
            self.inner
                .launched
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::{NonZeroU32, NonZeroU64};
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::vm::GuestRlimits;

    #[test]
    fn fake_runtime_records_launch_and_exec() {
        let rt = FakeVmRuntime::new();
        let cfg = VmConfig::builder().build();
        let mut handle = rt.launch(&cfg).expect("launch");
        let spec = RunSpec {
            command: PathBuf::from("/bin/true"),
            args: vec![],
            env: vec![("K".into(), "V".into())],
            cwd: None,
            timeout: Duration::from_secs(1),
            rlimits: GuestRlimits {
                cpu_secs: NonZeroU64::MIN,
                pids: NonZeroU32::MIN,
                nofile: NonZeroU64::MIN,
                fsize_bytes: NonZeroU64::MIN,
                mem_bytes: NonZeroU64::MIN,
            },
        };
        let out = handle.exec(&spec).expect("exec");
        assert!(out.success());
        assert_eq!(rt.launched(), vec![cfg]);
        assert_eq!(rt.execs(), vec![spec]);
        handle.kill();
        assert!(handle.is_killed());
    }
}
