// Types moved into heart::package. Re-export everything so existing
// `registry::package::*` references continue to compile unchanged.
pub use heart::package::*;

/// Re-export of `heart::package::coordinates` under the legacy path.
pub mod coordinates {
	pub use heart::package::coordinates::*;
}
