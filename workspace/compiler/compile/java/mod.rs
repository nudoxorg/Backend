//! Lowering Java into the surface IR via a vendored javadoc doclet oracle.
//!
//! The Java side is a deliberately thin, dumb extractor: a custom doclet
//! (`oracle/nudox/oracle/Extractor.java`, run inside the `javadoc` tool over
//! `javax.lang.model` + `com.sun.source`) that walks every included module,
//! package, and type and prints ONE exhaustive JSON document — recursive
//! structural type mirrors, raw doc-comment text verbatim, modifiers,
//! positions, constant values, enclosing relationships. All lowering,
//! javadoc parsing, and IR construction happens here in Rust.
//!
//! Resolution order per package:
//!   1. Discover the project layout — Maven `pom.xml`, Gradle, or a plain
//!      source tree — and assemble source roots (`package`).
//!   2. Materialize + compile the vendored oracle with `javac`, then run
//!      `javadoc -doclet nudox.oracle.Extractor` over the sources (`oracle`),
//!      capturing the JSON document (`schema`).
//!   3. Lower the extraction into `ir::kind::Entry`s: types (`item`),
//!      methods/constructors (`function`), structural type mirrors (`types`),
//!      wired together by the lowering context (`context`).
//!   4. Parse raw javadoc — block tags, inline tags, HTML, JEP 467 Markdown —
//!      into structured docs with resolved `{@link}` targets (`javadoc`).
//!
//! Version resolution over git tags / Maven coordinates lives in `traversal`.

pub mod context;
pub mod function;
pub mod item;
pub mod javadoc;
pub mod oracle;
pub mod package;
pub mod schema;
pub mod traversal;
pub mod types;
