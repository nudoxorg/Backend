//! Access-control decisions: given a [`tenant`](super::tenant) and a record's
//! [`visibility`](super::visibility) + [`source`](super::source), may this
//! principal read or write it?
//!
//! IMPLEMENT HERE: the single choke point every read/write is checked through,
//! so access control is never re-implemented per backend.
