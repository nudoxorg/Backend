//! Workspace rust fixture with a hyphenated package name.

pub trait Behavior {
	fn describe(&self) -> &'static str;
}

#[derive(Clone, Copy)]
pub enum Mode {
	Fast,
	Careful,
}

pub struct Widget<T> {
	inner: T,
}

impl<T> Widget<T> {
	pub fn new(inner: T) -> Self { Self { inner } }
}

impl<T: Clone> Widget<T> {
	pub fn echo(&self) -> T { self.inner.clone() }
}

impl Behavior for Mode {
	fn describe(&self) -> &'static str {
		match self {
			Mode::Fast => "fast",
			Mode::Careful => "careful",
		}
	}
}
