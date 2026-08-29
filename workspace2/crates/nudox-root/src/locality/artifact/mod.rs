//! One-artifact sorted-row locality encoding selected by `LocalitySortedEncoding`.

mod errors;
mod header;
mod layout;
mod rank;
mod rows;
mod trusted_decode;
mod validate;
mod view;
mod write;

pub use errors::{LocalityError, LocalityReadError, LocalityWriteError};
pub(in crate::locality) use layout::LaneTable;
pub use layout::LocalityLayout;
pub use view::{LocalityValidator, ValidatedLocality, with_validated_locality};
pub(crate) use write::LocalityEncoder;
pub use write::PreparedLocality;
