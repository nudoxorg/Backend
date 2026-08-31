//! Bounded request geometry and transport policy constants.

use std::{num::NonZeroU8, time::Duration};

pub(super) const DEFAULT_MAX_ATTEMPTS: NonZeroU8 = match NonZeroU8::new(3) {
    Some(value) => value,
    None => NonZeroU8::MIN,
};
pub(super) const MAX_BATCH_POINTS: usize = 16;
pub(super) const QUERY_SCAN_LIMIT: usize = MAX_BATCH_POINTS + 1;
pub(super) const MAX_QUERY_SEGMENTS: usize = 4;
pub(super) const MAX_VECTOR_DIMENSION: usize = 16;
pub(super) const MAX_RESPONSE_BYTES: usize = 1_048_576;
pub(super) const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
pub(super) const PHYSICAL_ID_ZERO_REPLACEMENT: u64 = 1;

pub(super) const READ_POINTS_PATH: &str = "/points?consistency=all";
pub(super) const QUERY_POINTS_PATH: &str = "/points/query?consistency=all";
pub(super) const UPSERT_POINTS_PATH: &str = "/points?wait=true&ordering=strong";
pub(super) const DELETE_POINTS_PATH: &str = "/points/delete?wait=true&ordering=strong";
pub(super) const CREATE_PAYLOAD_INDEX_PATH: &str = "/index?wait=true&ordering=strong";
pub(super) const CREATE_COLLECTION_PATH: &str = "?timeout=15";
