//! JSX type-checking subsystem.
//!
//! Split by responsibility:
//! - `orchestration`: main entry points, element resolution, namespace lookups
//! - `children`: child normalization, shape validation, contextual typing
//! - `extraction`: props extraction from components, component validation
//! - `overloads`: overloaded SFC resolution (TS2769)
//! - `props`: attribute checking, spread/union validation, missing props (TS2741)
//! - `runtime`: factory scope, import source, fragment factory
//! - `diagnostics`: display target building, error message rendering

/// Exact pragma owners shared with captured native program construction.
pub use runtime::{
    extract_jsx_import_source_pragma_text as program_import_source_pragma,
    extract_jsx_runtime_pragma as program_runtime_pragma,
};

mod children;
mod diagnostics;
mod extraction;
mod extraction_class_props;
mod extraction_factory_arity;
mod extraction_react_alias;
mod extraction_render_fallback;
mod orchestration;
mod overloads;
mod props;
pub(crate) mod runtime;
pub(crate) mod spread;

#[cfg(test)]
mod contextual_children_tests;
#[cfg(test)]
mod duplicate_attribute_grammar_tests;
#[cfg(test)]
mod optional_prop_display_tests;
#[cfg(test)]
mod ref_callback_tests;
#[cfg(test)]
mod spread_assignability_tests;
#[cfg(test)]
mod target_display_tests;
#[cfg(test)]
mod tests;
