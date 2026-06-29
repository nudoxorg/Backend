//! The route table.
//!
//! Splits a read plane (text/semantic/symbol search, expand, sessions) from a
//! write/admin plane (add/list/get/sync packages) so a query never holds a
//! handle the ingest path writes through.
