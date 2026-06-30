//! Lowering Python items (classes, functions, type aliases, constants, …) into
//! `ir::kind::Entry`.
//!
//! Uses pyrefly's per-module query results (`get_answers`, `get_bindings`,
//! `get_class_fields`) to discover the public API surface of a module, then
//! dispatches each named definition to the corresponding `ir::kind::Entry`
//! variant.
//!
//! IMPLEMENT HERE:
//!   - `fn lower_module(handle: &pyrefly::Handle, …) -> Vec<(NudoxPath, Entry)>`
//!   - Class definitions → `ir::kind::Entry::RecordType`
//!     - Fields via `query.get_class_fields()`
//!     - Methods via `query.get_bindings()` filtered by `is_function()`
//!     - Base classes → `ir::kind::Entry::RecordType.super_types`
//!   - Function definitions → `ir::kind::Entry::Function`
//!     - Delegate to `super::function::lower_function()`
//!   - Type aliases (`TypeAlias`) → `ir::kind::Entry::TypeAlias`
//!   - Module-level constants → `ir::kind::Entry::Constant`
//!   - Module-level variables → `ir::kind::Entry::Variable`
//!   - Nested classes → members on the parent RecordType
