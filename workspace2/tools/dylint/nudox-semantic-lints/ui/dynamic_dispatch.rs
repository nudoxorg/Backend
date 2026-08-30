#![allow(dead_code)]

trait Source {}

fn direct(source: &dyn Source) {
    let _ = source;
}

macro_rules! local_dynamic_boundary {
    ($name:ident) => {
        fn $name(source: &dyn Source) {
            let _ = source;
        }
    };
}

local_dynamic_boundary!(expanded);

#[allow(
    nudox_dynamic_dispatch,
    reason = "the alias declaration names the reviewed plugin capability"
)]
type PluginSource = dyn Source;

#[allow(
    nudox_dynamic_dispatch,
    reason = "the alias declaration names a reviewed owned plugin capability"
)]
type OwnedPluginSource = Box<dyn Source>;

type Bytes = Vec<u8>;

fn laundered(source: &PluginSource) {
    let _ = source;
}

fn nested_laundered(source: OwnedPluginSource) {
    let _ = source;
}

fn ordinary_alias(bytes: Bytes) {
    let _ = bytes;
}

#[allow(
    nudox_dynamic_dispatch,
    reason = "the process-local plugin adapter is an intentionally erased cold boundary"
)]
fn earned(source: &PluginSource) {
    let _ = source;
}

fn generic(source: &impl Source) {
    let _ = source;
}

fn main() {}
