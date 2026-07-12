//! Regular rust fixture for pipeline tests.

pub fn add(left: i32, right: i32) -> i32 { left + right }

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
