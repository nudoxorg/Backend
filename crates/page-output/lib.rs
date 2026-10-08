//! Pure canonical query-page projections, output credit and framing.
//! No client, socket, owner, filesystem, or process dependency belongs here.
#![deny(unsafe_code)]
mod assemble;
pub mod budget;
pub mod coverage;
pub mod dto;
pub mod envelope;
pub mod identity;
pub mod language;
pub mod packed_search;
pub mod packing;
pub mod record;
mod records_projection;
pub mod signature;
pub use assemble::{record_list_from_rows, row_summary};
pub use budget::*;
pub use coverage::*;
pub use dto::*;
pub use envelope::*;
pub use identity::*;
pub use language::*;
pub use packed_search::*;
pub use packing::*;
pub use record::*;
pub use records_projection::{RecordsProjection, encode_records};
pub use signature::*;
