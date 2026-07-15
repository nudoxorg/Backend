//! A standing synthetic `builtins` package, synthesised from snix's
//! `pure_builtins()`. Each builtin carries a `.name()` and a
//! `.documentation()` (collected from the Rust `///` docs by snix's
//! `#[builtins]` macro), so this is a free, high-quality builtins reference —
//! immediately the best available anywhere, and it needs no evaluation.
//!
//! When the documentation text is parseable, we also recover a minimal
//! parameter list (arity + coarse types from a `builtins.foo :: a -> b`
//! style signature, or a `Takes N arguments` note).
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
use ir::parameter::{LiteralParameter, Parameter};
use ir::ty::Type;

use super::item::path_of;
use super::sig;

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
        let inner = builtin_function(documentation.as_deref());
        let symbol = Symbol {
            name:          name,
            path:          path.clone(),
            aliases:       None,
            visibility:    Visibility::Public,
            documentation,
            deprecation:   None,
            doc_links:     None,
            inner,
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
        deprecation:   None,
        doc_links:     None,
        inner:         Module { members: None }, // members wired by context
    };
    entries.push((root_path.clone(), Entry::Module(module)));

    (entries, Some(root_path))
}

/// Build a `Function` for a builtin, attaching arity / types when the
/// documentation is parseable.
fn builtin_function(documentation: Option<&str>) -> Function {
    let (inputs, outputs) = documentation
        .map(parse_builtin_sig)
        .unwrap_or((None, None));

    Function {
        input_parameters:      inputs,
        output_parameters:     outputs,
        type_links:            None,
        attributes:            None,
        generics:              None,
        receiver:              None,
        overloads:             None,
        implemented:           true,
        members:               None,
        implemented_protocols: None,
    }
}

/// Best-effort recovery of arity + types from a builtin doc string.
///
/// Strategies, in order:
/// 1. A `::` signature line (`map :: (a -> b) -> [a] -> [b]`) via [`sig::parse`].
/// 2. A leading `Takes N arguments.` / `Arity: N` note → N untyped formals.
/// 3. Nothing parseable → empty shell.
fn parse_builtin_sig(
    doc: &str,
) -> (Option<Vec<Parameter>>, Option<Vec<Parameter>>) {
    // 1. Look for a `name :: type` or bare `a -> b` signature in the doc.
    for line in doc.lines() {
        let trimmed = line.trim();
        // Strip a leading backticks fence content.
        let candidate = trimmed
            .trim_start_matches('`')
            .trim_end_matches('`')
            .trim();
        if candidate.contains("->") || candidate.contains("::") {
            // Prefer the portion after `::` when present.
            let sig_src = candidate
                .split_once("::")
                .map(|(_, rest)| rest.trim())
                .unwrap_or(candidate);
            if let Some(sig) = sig::parse(sig_src) {
                let inputs = if sig.params.is_empty() {
                    None
                } else {
                    Some(
                        sig.params
                            .iter()
                            .enumerate()
                            .map(|(i, ty)| {
                                Parameter::Literal(LiteralParameter {
                                    name:          format!("arg{i}"),
                                    r#type:        Some(ty.clone()),
                                    attributes:    None,
                                    default_value: None,
                                    description:   None,
                                })
                            })
                            .collect(),
                    )
                };
                let outputs = Some(vec![Parameter::Literal(LiteralParameter {
                    name:          String::new(),
                    r#type:        Some(sig.ret),
                    attributes:    None,
                    default_value: None,
                    description:   None,
                })]);
                return (inputs, outputs);
            }
        }
    }

    // 2. Arity notes.
    if let Some(n) = parse_arity_note(doc) {
        if n > 0 {
            let inputs = (0..n)
                .map(|i| {
                    Parameter::Literal(LiteralParameter {
                        name:          format!("arg{i}"),
                        r#type:        Some(Type::Any),
                        attributes:    None,
                        default_value: None,
                        description:   None,
                    })
                })
                .collect();
            return (Some(inputs), None);
        }
    }

    (None, None)
}

/// Parse `Takes N arguments` / `N arguments` / `Arity: N` from docs.
fn parse_arity_note(doc: &str) -> Option<usize> {
    let lower = doc.to_ascii_lowercase();
    // "takes 2 arguments" / "takes two arguments" (numeric only for fidelity).
    if let Some(idx) = lower.find("takes ") {
        let rest = &lower[idx + "takes ".len()..];
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num.is_empty() {
            return num.parse().ok();
        }
    }
    if let Some(idx) = lower.find("arity:") {
        let rest = lower[idx + "arity:".len()..].trim_start();
        let num: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        if !num.is_empty() {
            return num.parse().ok();
        }
    }
    None
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
