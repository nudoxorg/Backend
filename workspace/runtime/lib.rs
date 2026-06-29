//! Runtime — the data layer that backs all search functionality, either
//! directly or through a thin abstraction. The canonical, always-on serving
//! store; persistence and coordination live elsewhere.

pub mod graph;
pub mod session;
pub mod text;
pub mod vector;
