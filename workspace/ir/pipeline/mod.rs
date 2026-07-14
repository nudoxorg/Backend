pub mod parse_common;
#[allow(clippy::module_inception)]
pub mod pipeline;

pub use parse_common::{output_parameters_from_type, parameter_link_key};
pub use pipeline::{Collected, Indexed, Ir, Stage};
