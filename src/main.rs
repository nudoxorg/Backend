use semver::Version;
use tokio::fs::write;

use crate::traits::{
    builder::{self, get_registry},
    registry::Registry,
};

mod core;
mod error;
mod ir;
mod traits;

use std::sync::Arc;

const TEST_PACKAGE: &str = "axum";

#[tokio::main]
async fn main() {
    let registry = get_registry(lang_types::Language::Rust);
    let packages = registry.get_packages_by_name(TEST_PACKAGE).await;

    let out = match packages {
        Ok(mut packages) => {
            let pkg = packages.remove(0); // Take ownership by removing from vec
            tokio::task::spawn_blocking(move || pkg.retrieve(Version::new(0, 8, 8), None))
                .await
                .unwrap()
        }
        Err(_other) => todo!(),
    }
    .unwrap();

    write("out.json", out).await;
}
