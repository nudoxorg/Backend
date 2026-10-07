//! Executes the production desktop capture module against the shared local-service API.
//! This proves host selection composition; it does not launch or test a GUI renderer.
#![cfg(unix)]

// Import the product implementation itself, so this gate cannot drift into a second
// desktop discoverer. Its fixtures need only the desktop host's scratch-root seam.
#[path = "../../../apps/desktop/src/host/toolchain.rs"]
mod toolchain;

mod host {
    pub(crate) fn scratch_base() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "nudox-desktop-toolchain-composition-{}",
            std::process::id()
        ))
    }
}

// The product's onboarding/settings consumers use this sibling-visible alias.
#[test]
fn rust_report_type_remains_visible_to_desktop_sibling_consumers() {
    let selection: Option<toolchain::Rust> = toolchain::report();
    match selection {
        Some(toolchain::Rust::Found { .. }) | Some(toolchain::Rust::Missing { .. }) | None => {}
    }
}
