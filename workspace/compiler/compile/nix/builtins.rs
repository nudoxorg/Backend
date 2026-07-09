//! A standing synthetic `builtins` package, synthesised from snix's
//! `pure_builtins()`. Each builtin carries a `.name()` and a
//! `.documentation()` (collected from the Rust `///` docs by snix's
//! `#[builtins]` macro), so this is a free, high-quality builtins reference —
//! immediately the best available anywhere, and it needs no evaluation.
//!
//! NOTE on the snix API: `snix_eval::builtins::pure_builtins()` returns a
//! `Vec<(&str, Value)>` (or `Vec<Builtin>` depending on the pinned revision);
//! the accessors used here (`.name()`, `.documentation()`) match the
//! documented surface. If a snix bump changes these, only this file and
//! `eval.rs`/`walker.rs` touch the snix API.

use ir::entry::NudoxPath;
use ir::function::Function;
use ir::kind::{Entry, Symbol, Visibility};
use ir::module::Module;

use super::item::path_of;

/// The attrpath root under which builtins live (`builtins.<name>`).
const ROOT: &str = "builtins";

/// Build the synthetic builtins module plus one `Entry::Function` per builtin.
/// Returns `(entries, root_path)`.
pub fn synthesize() -> (Vec<(NudoxPath, Entry)>, Option<NudoxPath>) {
    let mut entries: Vec<(NudoxPath, Entry)> = Vec::new();

    let root_path = path_of(&[ROOT.to_string()]);

    // One Function entry per builtin.
    for (name, documentation) in collect_builtins() {
        let path = path_of(&[ROOT.to_string(), name.clone()]);
        let symbol = Symbol {
            name:          name,
            path:          path.clone(),
            aliases:       None,
            visibility:    Visibility::Public,
            documentation,
            inner:         builtin_function(),
        };
        entries.push((path, Entry::Function(symbol)));
    }

    // The module container.
    let module = Symbol {
        name:          ROOT.to_string(),
        path:          root_path.clone(),
        aliases:       None,
        visibility:    Visibility::Public,
        documentation: Some("Nix built-in functions (the `builtins` set).".to_string()),
        inner:         Module { members: None }, // members wired by context
    };
    entries.push((root_path.clone(), Entry::Module(module)));

    (entries, Some(root_path))
}

/// A minimal `Function` shell for a builtin (arity is not exposed uniformly by
/// snix, so parameters are left unspecified; the documentation carries the
/// signature where snix's docs include one).
fn builtin_function() -> Function {
    Function {
        input_parameters:      None,
        output_parameters:     None,
        type_links:            None,
        attributes:            None,
        generics:              None,
        receiver:              None,
        overloads:             None,
        implemented:           true,
        members:               None,
        implemented_protocols: None,
        body:                  None,
    }
}

/// Collect `(name, documentation)` for every pure builtin.
///
/// Isolated here so the snix API touch-point is a single function: if a snix
/// bump changes the `pure_builtins()` shape, only this body changes.
///
/// `pure_builtins()` yields `(&'static str, Value)` where the `Value` is a
/// `Value::Builtin(Builtin)`; `.documentation()` is the `///`-sourced help
/// text collected by snix's `#[builtins]` macro.
fn collect_builtins() -> Vec<(String, Option<String>)> {
    // `pure_builtins()` panics on some host triples when snix's
    // `SNIX_CURRENT_SYSTEM` env is mis-set (it also injects pure *values*
    // like `null`/`true`/`nixVersion`). Catch that so a platform glitch
    // never aborts static lowering, and keep only real `Builtin`s.
    let Ok(builtins) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        snix_eval::builtins::pure_builtins()
    })) else {
        tracing::warn!("nix: pure_builtins() panicked; synthesizing empty builtins set");
        return Vec::new();
    };

    builtins
        .into_iter()
        .filter_map(|(name, value)| match value {
            snix_eval::Value::Builtin(b) => {
                Some((name.to_string(), b.documentation().map(str::to_string)))
            }
            _ => None,
        })
        .collect()
}
