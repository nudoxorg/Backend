//! Regular rust fixture for pipeline tests.

pub fn add(left: i32, right: i32) -> i32 { left + right }

/// Superseded arithmetic helper.
#[deprecated(since = "0.1.0", note = "use add instead")]
pub fn legacy_add(left: i32, right: i32) -> i32 { left + right }

mod sealed_marker {
	pub trait Sealed {}
}

/// A sealed trait: downstream crates cannot implement it because its `Sealed`
/// supertrait lives in a private module.
pub trait SealedTrait: sealed_marker::Sealed {
	fn describe(&self) -> &'static str;
}

/// An open trait: its only supertrait is public, so it is not sealed.
pub trait OpenTrait: BlanketView {
	fn kind(&self) -> u8;
}

mod internals {
	pub(crate) struct HiddenCounter {
		value: i32,
	}

	impl HiddenCounter {
		pub(crate) fn new(value: i32) -> Self { Self { value } }

		pub(crate) fn value(&self) -> i32 { self.value }
	}
}

pub trait BlanketView {
	type View;

	fn view(&self) -> &Self::View;
}

impl<T> BlanketView for T {
	type View = T;

	fn view(&self) -> &Self::View { self }
}

pub struct Counter {
	value: i32,
}

impl helper::Marker for Counter {}

impl Counter {
	pub fn new(value: i32) -> Self { Self { value } }

	pub fn increment(&mut self) { self.value += 1; }

	pub fn value(&self) -> i32 { self.value }
}
