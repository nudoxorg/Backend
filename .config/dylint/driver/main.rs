//! Mirrors the driver entry point Dylint writes for its generated package.
//! Gives the pinned manifest a target so Cargo can lock its graph.
//! Holds no behavior beyond handing arguments to `dylint_driver`.
#![feature(rustc_private)]

use anyhow::Result;
use std::env;

pub fn main() -> Result<()> {
    env_logger::init();

    let args: Vec<_> = env::args_os().collect();

    dylint_driver::dylint_driver(&args)
}
