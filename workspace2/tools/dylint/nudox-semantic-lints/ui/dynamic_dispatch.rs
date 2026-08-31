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

type PluginSource = dyn Source;

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

// External APIs may necessarily erase private implementation details. Merely naming their public
// result alias does not introduce a new dynamic boundary in shipping code.
fn external_alias(result: std::thread::Result<()>) {
    let _ = result;
}

fn generic(source: &impl Source) {
    let _ = source;
}

fn main() {}
