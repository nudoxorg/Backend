//! Runtime — the data layer that backs all search functionality, either
//! directly or through a thin abstraction. The canonical, always-on serving
//! store; persistence and coordination live elsewhere.
#![feature(generic_const_exprs)]
#![feature(return_type_notation)]
#![allow(incomplete_features)]

pub mod graph;
pub mod session;
pub mod text;
pub mod vector;
