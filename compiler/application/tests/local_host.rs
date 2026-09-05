//! Ordinary lifecycle falsifiers for typed local-host compiler admission.

use std::{ffi::OsString, fs, path::PathBuf};

use compiler_application::{
    LocalCompilerHost, LocalCompilerHostError, LocalHostDiscovery, LocalHostEnvironment,
    LocalHostVariable,
};
use interface_core::{CompilerCapability, CompilerReadiness};

#[derive(Clone)]
struct ExplicitDataRoot {
    root: PathBuf,
}

impl LocalHostEnvironment for ExplicitDataRoot {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        (variable == LocalHostVariable::NudoxDataRoot).then(|| self.root.clone().into_os_string())
    }
}

fn fresh_root(label: &str) -> PathBuf {
    static NEXT_ROOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ordinal = NEXT_ROOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "nudox-local-host-{label}-{}-{ordinal}",
        std::process::id()
    ))
}

#[test]
fn explicit_only_host_starts_a_ready_owner_without_ambient_tool_lookup() {
    let root = fresh_root("ready");
    let host = LocalCompilerHost::new(
        ExplicitDataRoot { root: root.clone() },
        LocalHostDiscovery::ExplicitOnly,
    );
    let client = host
        .open()
        .expect("an explicit durable root admits an honestly tool-unavailable runtime");
    assert_eq!(client.readiness(), CompilerReadiness::Ready);
    drop(client);
    fs::remove_dir_all(root).expect("the stopped owner releases its exact fixture root");
}

#[test]
fn relative_data_root_is_rejected_with_its_typed_variable_and_path() {
    let root = PathBuf::from("relative-host-root");
    let host = LocalCompilerHost::new(
        ExplicitDataRoot { root: root.clone() },
        LocalHostDiscovery::ExplicitOnly,
    );
    let error = match host.open() {
        Ok(_client) => panic!("relative storage cannot become durable publication authority"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        LocalCompilerHostError::RelativeEnvironmentPath {
            variable: LocalHostVariable::NudoxDataRoot,
            path,
        } if path.as_ref() == root
    ));
}
