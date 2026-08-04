//! Python producer for the nudox-ir pipeline (pyrefly tier).
//!
//! # Architecture
//!
//! ```text
//! invoke():                                         (pyrefly feature required)
//!   context::invoke_oracle()                → PythonOracle { modules: Vec<ModuleData> }
//!     └─ pyrefly State + transaction + extraction → owned data, all handles dropped
//!
//! lower():
//!   emit::emit_package(&oracle, out)        → one-pass into Lowering<PythonId>
//! ```
//!
//! # pyrefly feature gate
//!
//! The `pyrefly` Cargo feature is **off by default**. Without it:
//! - All lowering, type-mapping, and docstring code compiles and tests cleanly.
//! - `invoke()` returns an empty `PythonOracle`.
//! - Tests that need a live oracle are marked `#[ignore]` or gated behind
//!   `#[cfg(feature = "pyrefly")]`.
//!
//! See `Cargo.toml` for the full rationale on why the git dep is not declared
//! by default (pyrefly_bundled downloads a typeshed at build time; the crate
//! is not on crates.io at the needed version).
//!
//! # Id scheme: `PythonId`
//!
//! `PythonId` is a fully-qualified dotted Python path string. Examples:
//! - module:   `"my_pkg.utils"`
//! - class:    `"my_pkg.utils.MyClass"`
//! - method:   `"my_pkg.utils.MyClass.method"`
//! - overload: `"my_pkg.utils.fn_name#0"`, `"…#1"` (distinct per branch)
//! - param:    `"my_pkg.utils.MyClass.method.param_name"`
//!
//! This satisfies the `Self::Id: Eq + Hash + Clone + Debug` requirement and
//! distinguishes overloads from each other (the `#N` suffix).
//!
//! # Lifetime resolution
//!
//! pyrefly `Handle` / `Transaction` / `State` hold internal borrows and are
//! not `'static`. We follow the TypeScript producer's "extract to owned in
//! invoke()" pattern: `invoke()` runs the oracle, copies all data into owned
//! `ModuleData` / `ItemData` / etc., drops all pyrefly handles, and returns.
//! `lower()` therefore works on fully-owned data with no arena lifetime.
//!
//! # @property decision
//!
//! A `@property` method is emitted as a `Function` child of its class.
//! The `Symbol.attrs` list carries `AttrTok { token: "property", arg: None }`
//! so consumers can identify it. A dual `Field` entry is NOT emitted because:
//! 1. The `Field` would carry `FieldKey::Named` which clashes with the method.
//! 2. The type is available on the `Function`'s return param, which is the
//!    canonical place for property types.
//!    If a future consumer needs `Field` semantics, the emit layer can be extended.
//!
//! # @classmethod decision
//!
//! `@classmethod` methods have `receiver = None` (the `cls` parameter is a
//! regular Python convention but not an instance receiver). The method is
//! decorated with `AttrTok { token: "classmethod", arg: None }`.
//!
//! # Constructs not yet representable
//!
//! - `ParamSpec` / `TypeVarTuple` in generic positions — lowered to `TypeVar(name)`.
//! - `Annotated[T, meta...]` — lowered to `Type::Any` (no IR annotation slot).
//! - `TypeGuard` / `TypeIs` predicates — lowered to `Type::Any`.
//! - Keyword-only params after `*` without a name — no `ParamAttribute` yet.
//! - Default-value expressions — the `value` string is preserved on `Const`
//!   but param defaults are `ParamAttribute::Optional` (no const-expr system yet).

pub mod docstring;
pub mod emit;
pub mod oracle;
pub mod producer;
pub mod types;

#[cfg(feature = "pyrefly")]
pub mod context;

pub use oracle::PythonId;
pub use producer::PythonProducer;
