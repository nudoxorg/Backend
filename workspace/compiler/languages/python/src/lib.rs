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
//! - `invoke()` ignores its `PackageSource` and returns an empty
//!   `PythonOracle`, and [`PythonProducer`] declares that through
//!   `Producer::yield_contract` so `nudox_producer::produce` cannot report the
//!   result as a successful lowering.
//! - Tests that need a live oracle are marked `#[ignore]` or gated behind
//!   `#[cfg(feature = "pyrefly")]`.
//!
//! **Turning the feature on does not work, and this is not a matter of adding a
//! `--features` flag.** Two blockers, both verified against this tree:
//!
//!  1. `context` is declared below as `#[cfg(feature = "pyrefly")] pub mod
//!     context;` and `src/context.rs` **does not exist**. With the feature on,
//!     the crate fails to compile at this declaration.
//!  2. Declaring the git dependency exactly as `Cargo.toml`'s "Pyrefly gate"
//!     comment prescribes fails Cargo *dependency resolution*: pyrefly pins
//!     `blake3 =1.8.2` against `workspace/index`'s `iroh` requirement of
//!     `^1.8.3` — disjoint ranges, no version in common.
//!
//! See `Cargo.toml` for the rest of the rationale on why the git dep is not
//! declared by default (pyrefly_bundled downloads a typeshed at build time; the
//! crate is not on crates.io at the needed version).
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
//! - `TypeGuard` / `TypeIs` predicates — lowered to `Type::Any`.
//! - Positional-only parameters (before Python's `/` marker) — no
//!   `ParamAttribute` exists in `nudox-ir` for this calling-convention
//!   restriction (it has `KeywordOnly` but no positional-only counterpart);
//!   `types.rs`/`emit/mod.rs` deliberately emit no attribute rather than
//!   reuse `Inout`, which would misreport it as pass-by-mutable-reference.
//!   Adding the counterpart variant is an `nudox-ir` change, out of this
//!   crate's scope.
//! - Default-value expressions — the `value` string is preserved on `Const`
//!   but param defaults are `ParamAttribute::Optional` (no const-expr system yet).
//!
//! Two items previously listed here are now representable and have been
//! removed from this list because the code already does what the list said
//! it could not: `Annotated[T, meta...]` lowers to `Type::Annotated` (see
//! `types.rs`), and keyword-only parameters lower to
//! `ParamAttribute::KeywordOnly` (see `emit/mod.rs`) — that variant exists in
//! `nudox-ir` specifically for this case; its own doc comment cites Python's
//! `def f(a, *, b)`.

pub mod docstring;
pub mod emit;
pub mod oracle;
pub mod producer;
pub mod types;

#[cfg(feature = "pyrefly")]
pub mod context;

pub use oracle::PythonId;
pub use producer::PythonProducer;
