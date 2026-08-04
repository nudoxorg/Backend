//! High-fidelity IR fixture: method paths, widths, consts, aliases, dual impls.

/// Recoverable free constant with fixed-width integer type.
pub const N: i32 = 4;

/// Structured arithmetic const — must not collapse to a bare `Var` string.
pub const SHIFTED: i32 = (1 + 2) * 4;

/// Mutable static for Variable / TypedBinding mutability.
pub static mut GLOBAL: i32 = 0;

/// Generic type alias — generics must survive on `TypeAliasBody`.
pub type Pair<T, U> = (T, U);

/// Product type with fixed-width field and inherent methods.
pub struct Counter {
	/// Must lower as `Primitive::Int(W32)`, not Arch.
	value: i32,
}

impl Counter {
	pub fn new(value: i32) -> Self {
		Self { value }
	}

	pub fn value(&self) -> i32 {
		self.value
	}

	/// Return type is `&'static str` — must not become owned String primitive.
	pub fn label(&self) -> &'static str {
		"counter"
	}
}

/// Shared trait implemented by two concrete types (TraitImpl path uniqueness).
pub trait Drawable {
	fn draw(&self);
}

pub struct Circle;
pub struct Square;

impl Drawable for Circle {
	fn draw(&self) {}
}

impl Drawable for Square {
	fn draw(&self) {}
}

/// Enum with inherent methods — dual-emit even without Record.methods field.
pub enum Mode {
	On,
	Off,
}

impl Mode {
	pub fn is_on(&self) -> bool {
		matches!(self, Mode::On)
	}
}

/// Trait with associated type bounds and typed constant (C8).
pub trait WithAssoc {
	type Item: Clone;
	const MAX: i32;
	fn get(&self) -> Self::Item;
}

mod private_mod {
	pub struct Hidden;
}
