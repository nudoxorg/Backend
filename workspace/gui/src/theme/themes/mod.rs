//! Concrete theme data modules.
//!
//! Each module is a function returning a fully-filled [`NudoxThemeExt`].
//! A third theme is a third file — no code changes required elsewhere.

pub mod dark;
pub mod light;

pub use dark::dark_theme;
pub use light::light_theme;
