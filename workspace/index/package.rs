//! Package vocabulary re-exports (heart::package).
pub use heart::package::*;

/// Re-export of `heart::package::coordinates` under the legacy path.
pub mod coordinates {
    pub use heart::package::coordinates::*;
}
