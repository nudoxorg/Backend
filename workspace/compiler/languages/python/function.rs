//! Lowering Python functions/methods into `ir::function::Function`.
//!
//! Uses pyrefly's binding and answer queries to extract parameter names, types,
//! defaults, decorators, and the return type for each callable.
//!
//! IMPLEMENT HERE:
//!   - `fn lower_function(…, binding: &pyrefly::Binding) -> ir::function::Function`
//!   - Self/cls receiver detection (first param named `self`/`cls`)
//!     → set `receiver: Some(ReceiverKind::Self_)`
//!   - Parameter lowering (name + type from pyrefly's `get_answers`)
//!     → `ir::parameter::LiteralParameter`
//!   - Default values (from the AST, not pyrefly) → `ir::generics::ConstExpr`
//!   - Decorator detection:
//!     - `@staticmethod`, `@classmethod` → receiver adjustment
//!     - `@property` → special attribute
//!     - `@abstractmethod` → attribute
//!     - `@overload` → push as overload variant
//!   - Return type from pyrefly's answer → `output_parameters`
//!   - Async detection (`async def`) → `Attribute::Async`
//!   - Generator detection (`yield`) → `Attribute::Generator`
//!   - Variadic params (`*args`, `**kwargs`) → `ParameterAttribute::Variadic`
//!   - Type parameters from pyrefly's `TypeVar`/`ParamSpec` on generic functions
//!     → `generics.params`
