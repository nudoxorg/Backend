//! Python module resolution: import graph, `__init__.py` merging, namespace
//! packages, and the bridge between pyrefly's module handles and the IR.
//!
//! IMPLEMENT HERE:
//!   - Resolving `from X import Y` and `import X` into pyrefly `Handle`s.
//!   - Flattening `__init__.py` re-exports into the parent module's entry list.
//!   - Mapping pyrefly `ModuleName` / `ModulePath` → `ir::entry::NudoxPath`.
//!   - Building the `ir::module::Module` with resolved member paths.
//!   - Handling namespace packages (PEP 420) — multiple directories, one module.
//!   - Native/built-in module stubs (via `pyrefly_bundled` or typeshed).
