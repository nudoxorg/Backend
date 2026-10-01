//! Journey helper that reports the real production compiler execution grant.
//!
//! The result is used to provision the shipped owner and worker CLIs with the exact invocation
//! identities they independently recompute. It does not create trust or substitute an executor.

use backend_engine::application::{
    CompilerPackageTargetV2, LocalCompilerCapabilityState, LocalCompilerHost,
};
use backend_library::interface::PackageUrl;
use backend_semantic::vocabulary::{LanguageProfile, RustEdition, Stage};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

fn main() -> ExitCode {
    match report() {
        Ok(value) => {
            println!("{value}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("compiler capability probe: {error}");
            ExitCode::FAILURE
        }
    }
}

fn report() -> Result<String, String> {
    let data_root = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| "expected the private probe data directory".to_owned())?;
    let compiler = LocalCompilerHost::production_at(data_root)
        .open()
        .map_err(|error| format!("open production compiler: {error}"))?;
    let profile = LanguageProfile::Rust(RustEdition::Rust2024);
    let package = PackageUrl::parse("pkg:cargo/cluster-invocation-probe@1.0.0")
        .map_err(|error| format!("probe package coordinate: {error:?}"))?;
    let target = CompilerPackageTargetV2::for_package(package).target();
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let capability = compiler.capabilities().for_profile(profile);
        if capability.state() == LocalCompilerCapabilityState::Ready {
            let identity = capability
                .execution_identity(target, profile, Stage::LowerIr)
                .ok_or_else(|| {
                    "ready compiler omitted its lower-ir execution identity".to_owned()
                })?;
            let invocation = identity.invocation_recipe();
            return Ok(serde_json::json!({
                "profile": hex(&<[u8; 2]>::from(profile)),
                "stage": u8::from(Stage::LowerIr),
                "recipe": hex(invocation.identity().as_ref()),
                "toolchain": hex(invocation.toolchain().as_ref()),
                "environment": hex(&invocation.environment()),
                "target_platform": hex(&invocation.target_platform()),
                "local_authority_fingerprint": hex(&identity.local_authority_fingerprint()),
            })
            .to_string());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "Rust compiler capability did not become ready ({:?})",
                capability.state()
            ));
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        output.push(char::from(b"0123456789abcdef"[usize::from(byte >> 4)]));
        output.push(char::from(b"0123456789abcdef"[usize::from(byte & 0x0f)]));
    }
    output
}
