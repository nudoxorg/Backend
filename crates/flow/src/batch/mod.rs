//! Immutable batch facade.
//!
//! Batch construction, borrowed cursors, and immutable trace runs share one
//! ownership boundary. The private implementation module keeps that boundary
//! explicit for future column and codec specializations.

mod contracts;
mod cursor;
mod microbatch;
mod run;
mod schema;

pub use contracts::*;
pub use cursor::*;
pub use microbatch::*;
pub use run::*;
pub use schema::*;
pub(crate) use schema::{
    append_len_prefixed, batch_root, canonical_bytes, lower_bound, row_cmp, upper_bound,
};
