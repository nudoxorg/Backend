//! Ranking post-processor for registry search.
//!
//! Pure and deterministic: no RNG, no I/O, no timestamps. Ties break by name.

mod candidate;
mod config;
mod pipeline;

pub use super::popularity::DEPENDENT_DOWNLOAD_EQUIV;
pub use candidate::Candidate;
pub use config::RankingConfig;
pub use pipeline::{
    rank, rank_full, rank_full_with, rank_full_with_intent, rank_with, rank_with_intent,
};

#[cfg(test)]
mod tests;
