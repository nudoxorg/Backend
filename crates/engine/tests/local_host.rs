//! Ordinary lifecycle falsifiers for typed local-host compiler admission.

use std::{ffi::OsString, fs, path::PathBuf};

use backend_engine::application::{
    LocalCompilerHost, LocalCompilerHostError, LocalHostDiscovery, LocalHostEnvironment,
    LocalHostVariable,
};
use backend_library::interface::{CompilerCapability, CompilerReadiness};

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

/// Selects exactly the variables a user would export for an explicit Rust toolchain.
#[derive(Clone)]
struct ExplicitRust {
    root: PathBuf,
    rustc: PathBuf,
    cargo: PathBuf,
    cargo_home: PathBuf,
}

impl LocalHostEnvironment for ExplicitRust {
    fn value(&self, variable: LocalHostVariable) -> Option<OsString> {
        match variable {
            LocalHostVariable::NudoxDataRoot => Some(self.root.clone().into_os_string()),
            LocalHostVariable::NudoxRustc => Some(self.rustc.clone().into_os_string()),
            LocalHostVariable::NudoxCargo => Some(self.cargo.clone().into_os_string()),
            LocalHostVariable::NudoxCargoHome => Some(self.cargo_home.clone().into_os_string()),
            _ => None,
        }
    }
}

/// Finds `tool` the way a shell does: the first `PATH` entry holding it under the host
/// executable suffix.
fn on_path(tool: &str) -> Option<PathBuf> {
    let name = format!("{tool}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join(&name))
        .find(|candidate| candidate.is_file())
}

/// The owner used to refuse to start with "Rustc version probe exited with exit code: 1" when
/// `NUDOX_RUSTC` named a rustup proxy: the host resolved the proxy link to the `rustup` binary,
/// which rejects `--print sysroot`. Selecting the toolchain as a user exports it must start.
#[test]
fn explicit_rust_toolchain_selected_through_its_proxies_starts_a_ready_owner() {
    let rustc = on_path("rustc").expect("a cargo test environment has rustc on PATH");
    let cargo = std::env::var_os("CARGO")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_file())
        .or_else(|| on_path("cargo"))
        .expect("a cargo test environment provides cargo");
    let cargo_home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && path.is_dir())
        .or_else(|| {
            let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
            Some(PathBuf::from(home).join(".cargo")).filter(|path| path.is_dir())
        })
        .expect("a cargo test environment has a Cargo home");
    let root = fresh_root("rust-proxy");
    let host = LocalCompilerHost::new(
        ExplicitRust {
            root: root.clone(),
            rustc,
            cargo,
            cargo_home,
        },
        LocalHostDiscovery::ExplicitOnly,
    );
    let client = host
        .open()
        .unwrap_or_else(|error| panic!("an explicit Rust toolchain must start the owner: {error}"));
    assert_eq!(client.readiness(), CompilerReadiness::Ready);
    drop(client);
    fs::remove_dir_all(root).expect("the stopped owner releases its exact fixture root");
}
