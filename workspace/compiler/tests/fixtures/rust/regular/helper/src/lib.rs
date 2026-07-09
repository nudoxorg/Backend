//! External-dependency stand-in for the regular fixture: a trait whose
//! implementors must reference it as an external path (the offline analogue of
//! `serde::Serialize`).

pub trait Marker {
	fn marked(&self) -> bool { true }
}
