//! Python producer for the nudox-ir pipeline (ruff syntactic tier, default;
//! pyrefly semantic tier, gated and not yet reachable — see below).
//!
//! # Architecture
//!
//! ```text
//! invoke():                                          (default: no feature needed)
//!   syntax::build_oracle()                  → PythonOracle { modules: Vec<ModuleData> }
//!     └─ ruff_python_parser::parse_module per .py file → owned data, all arenas dropped
//!
//! lower():
//!   emit::emit_package(&oracle, out)        → one-pass into Lowering<PythonId>
//! ```
//!
//! `PythonOracle` (`oracle.rs`) and the lowering layer (`emit/`, `types.rs`,
//! `docstring.rs`) were built oracle-agnostic from the start: no
//! `pyrefly::`/`ruff_python_ast::` type appears in their signatures. That is
//! what let `syntax.rs` become a genuine second front end without touching
//! any of that ~2,100 lines — the same "extract to owned data, drop the
//! arena" shape `nudox-producer-typescript`'s OXC front end uses.
//!
//! # pyrefly feature gate (off by default — now a choice, not a breakage)
//!
//! ```text
//! invoke() with --features pyrefly:
//!   context::invoke_oracle()
//!     ├─ syntax::build_oracle()  → structure, docstrings, written annotations
//!     └─ pyrefly State/Bindings  → fills the type slots the syntax tier cannot
//! ```
//!
//! Both reasons this comment used to give for the feature being unreachable
//! were retired on 2026-08-08. `src/context.rs` exists and is the semantic
//! tier above. pyrefly's `blake3` pin has moved to `=1.8.6`, which every
//! blake3 requirement in this workspace (`^1.8`) already admits, so
//! `cargo metadata --locked --features nudox-producer-python/pyrefly`
//! resolves. `pyrefly_bundled` never downloaded anything either — its typeshed
//! is committed in the pyrefly repo and therefore inside cargo's own git
//! checkout; see the accounting in `Cargo.toml`.
//!
//! It stays off by default because of what it costs versus what it adds: 77
//! extra lock entries for 4,607 of 16,025 corpus annotation positions gaining a
//! resolvable type — and roughly a third of that gain is also reachable by
//! matching bare type names against same-package ids, with no dependency at
//! all. `context.rs`'s module doc carries the full split.
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
pub mod syntax;
pub mod types;

#[cfg(feature = "pyrefly")]
pub mod context;

pub use oracle::PythonId;
pub use producer::PythonProducer;
