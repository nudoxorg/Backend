//! Lowering Pyrefly's `Type` into `ir::ty::Type`.
//!
//! Maps pyrefly's rich Python type algebra (Union, Intersect, Literal, Callable,
//! TypedDict, TypeVar, ParamSpec, …) to the language-agnostic IR type
//! representation.
//!
//! IMPLEMENT HERE:
//!   - `fn lower_type(py_type: &pyrefly_types::Type) -> ir::ty::Type`
//!   - Map `pyrefly_types::Type::Union` → `ir::ty::Type::Union`
//!   - Map `pyrefly_types::Type::Literal` → `ir::ty::Type::TypeReference`
//!   - Map `pyrefly_types::Type::Callable` → `ir::ty::Type::FunctionPointer`
//!   - Map `pyrefly_types::Type::ClassType` → `ir::ty::Type::TypeReference`
//!   - Map `pyrefly_types::Type::Any` → `ir::ty::Type::Any`
//!   - Map `pyrefly_types::Type::None` → `ir::ty::Type::Primitive(Primitive::Null)`
//!     (or a dedicated unit type — TBD in IR)
//!   - Map `pyrefly_types::Type::Tuple` → `ir::ty::Type::Tuple`
//!   - Map `pyrefly_types::Type::TypedDict` → `ir::ty::Type::RecordLiteral`
//!   - Map `pyrefly_types::Type::TypeVar` → `ir::ty::Type::GenericParam`
//!   - Handle `Intersect`, `TypeGuard`, `TypeIs`, `ParamSpec`, etc.
//!   - Resolve fully-qualified module paths for class types.
