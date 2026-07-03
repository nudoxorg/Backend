//! The blobs — the parsed code (CST + IR + tarred source) that everything else
//! is derived from, and therefore the root of reproducibility: given the blobs,
//! every downstream store can be regenerated.
//!
//! IMPLEMENT HERE: durable blob persistence + the rebuild-from-blobs entry point.
