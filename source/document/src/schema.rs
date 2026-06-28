//! This module magically transforms compiler IR into JSON-LD to be dumped into
//! a RDF triple based DB (**TerminusDB**)
//!
//! **Core goal**: *flatten nested Rust structs into RDF triple-based nodes*
//! with stable `@id` values and minimal nesting and explicit edges (`@id` ref)
//! instead of nested objects
//!
//! # Concepts
//!
//! ## Entry
//! An **Entry** is the symbol for the "demographics"/metadata of an API symbol
//! Think name, path, fq_name, documentation, visibility and edges to related
//! symbols.
//!
//! Example
//! ```json
//! {
//!   "@id": "Entry/rust/jsonld/<symbol_id>",
//!   "@type": "Entry",
//!   "fq_name": "jsonld::convert::testing",
//!   "path": ["jsonld", "convert", "testing"],
//!   "visibility": "public",
//!   "kind_tag": "Function",
//!   "kind": { "@id": "Kind/rust/<symbol_id>" },
//!   "members": [
//!     { "@id": "Entry/rust/jsonld/<member_symbol_id>" }
//!   ]
//! }
//! ```
//!
//! ## Kind
//! A **Kind** structurally represents a symbol. Think Function, Struct, Enum,
//! Module, etc. Kinds hold payload fields that descrie the symbol's
//! behavior/shape (params/return types for functions). Kind will also store a
//! Lexical Hash of important fields that ca be used for future similarity
//! checks within the evnetual database instance.
//!
//! There are two main categories as of now.
//! - **Hub-like kinds**: Skeleton like kind (think `Module`) that contains
//!   little to no information. The Entry would contain the significant
//!   information for a Module (members, documentation, etc.).
//! - **Authority kinds**: Lots of data within this kind (`Function`, `Struct`,
//!   `Enum`) where Kind describes the shape of this symbol
//!
//! For Shape Lookup, Kind will have some @key - hashed field, derived from
//! contents of kind that can be used to find similar Kinds. Ignore for now, but
//! *a clever solution could allow us to query different languages for crates
//! that have similar solutions to a problem*.
//!
//! # Identity + URI Rules
//!
//! Requirement -> **any referencing node must be able to compute the URI of
//! what it references** without relying on discovery order. An entry that is a
//! `Function` that has an input parameter of Type `MyType` should be able to
//! have the link to the Kind associated to `MyType`'s Entry.
//!
//! ## Symbol identity (`symbol_id`)
//! The symbol_id will be derived from the path / fq_name of the symbol. It is
//! important that this can be used to track minor changes (a param added, Kind
//! changes, etc.) until the point when the name/path changes (that is deemed a
//! breaking change, which will be refeleceted by the URI)
//!
//! URI @ids:
//! - **Entry** `{"@id": "Entry/{lang}/{crate_name}/{symbol_id}}"`
//! - **Kind**  `{"@id": "Kind/{lang}/{crate_name}/{symbol_id}}"`
//!
//!
//! # Architecture
//!
//! The core idea is to store a Map of URI -> Json Values, representing the
//! symbol/kind/etc URI and the corresponding values. Some context will be
//! passed to contain URIs, metadata, etc. to include on various types/enum
//! variants for Kind.
use std::{borrow::Cow, collections::{BTreeMap, btree_map::Entry as BTreeEntry}};

use serde::Serialize;
use serde_json::{Value, json};
use tracing::warn;

use identity::{EntryUri, KindUri, path::nudox_path_to_str};

pub type URI = String;

/// Store Mapping of URI -> Documents in JsonLD form, ready for insertion
#[derive(Serialize)]
pub struct DocStore {
	// can switch to Async type in the future
	pub docs: BTreeMap<URI, Value>,
}

impl Default for DocStore {
	fn default() -> Self { Self::new() }
}

impl DocStore {
	pub fn new() -> Self { DocStore { docs: BTreeMap::default() } }

	pub fn insert(&mut self, uri: URI, value: Value) -> Result<URI, URI> {
		// Insert-by-move on the common (vacant) path: the value is never cloned,
		// and an existing slot is compared by reference. Only the (small) URI key
		// is cloned for the return value.
		match self.docs.entry(uri) {
			BTreeEntry::Vacant(slot) => {
				let uri = slot.key().clone();
				slot.insert(value);
				Ok(uri)
			}
			BTreeEntry::Occupied(slot) => {
				if *slot.get() == value {
					Ok(slot.key().clone())
				} else {
					warn!(uri = %slot.key(), "non-identical values inserted at same URI");
					// Collision! Keep the existing value; report the conflict.
					// todo!("Handle URI collision: decide if we should merge or
					// disambiguate.")
					Err(slot.key().clone())
				}
			}
		}
	}

	pub fn documents_sorted(&self) -> Vec<(&URI, &Value)> { self.docs.iter().collect() }
}

/// A destination for emitted JSON-LD documents.
///
/// Decouples document *production* (the `emit` walk over the IR) from
/// *materialization*. Tests collect into a [`DocStore`] (a `BTreeMap<URI, Value>`
/// they can inspect), while production streams each finished document into a
/// compact, upload-ready buffer — so the whole corpus never lives as
/// `serde_json::Value` trees at once.
pub trait DocSink {
	/// Accept one finished document by value: its `@id`/URI and the JSON-LD
	/// `Value`. The sink takes ownership; nothing downstream reads it again.
	fn accept(&mut self, uri: URI, doc: Value);
}

impl DocSink for DocStore {
	fn accept(&mut self, uri: URI, doc: Value) {
		// `insert` already warns on a same-URI/different-value collision and keeps
		// the first write; the return value is unused here.
		let _ = self.insert(uri, doc);
	}
}
/// Stores Global Info about the Crate
// this will live for the duration of the program, need cheap copies for
// insertion
pub struct CrateInfo {
	lang:       Cow<'static, str>,
	crate_name: Cow<'static, str>,
}

impl CrateInfo {
	pub fn new(
		lang: impl Into<Cow<'static, str>>,
		crate_name: impl Into<Cow<'static, str>>,
	) -> Self {
		CrateInfo { lang: lang.into(), crate_name: crate_name.into() }
	}

	// Getters
	pub fn lang(&self) -> &str { self.lang.as_ref() }

	pub fn crate_name(&self) -> &str { self.crate_name.as_ref() }

}

/// Context to be used including lib/crate info, schema context, uri rules, etc.
// TODO - ADD @context, built once inserted here
// maybe URI builder function
pub struct DocCtx {
	pub crate_info:   CrateInfo,
	pub current_path: Option<ir::entry::NudoxPath>,
	pub context_obj:  Value,
}

impl DocCtx {
	/// Initialize Must have the CrateInfo
	pub fn init(crate_info: CrateInfo, context_obj: Value) -> Self {
		Self { crate_info, current_path: None, context_obj }
	}

	/// Set path to current entry's path
	// TODO refactor, such that this is not a global path holder
	pub fn update_path(&mut self, path: &ir::entry::NudoxPath) {
		self.current_path = Some(path.clone())
	}

	pub fn context(&self) -> &Value { &self.context_obj }

}

/// Trait for edges and URI construction
pub trait UriOps {
	fn entry_uri(&self, path: &ir::entry::NudoxPath) -> URI;
	fn kind_uri(&self, kind: &ir::kind::Entry, path: &ir::entry::NudoxPath) -> URI;

	fn uri_path(&self, path: &ir::entry::NudoxPath) -> URI;
	// This returns a JsonLd Edge - {"@id": "<URI>"}
	fn build_edge(uri: &URI) -> Value;
}

/// Responsible for URI construction
impl UriOps for DocCtx {
	fn entry_uri(&self, path: &ir::entry::NudoxPath) -> URI {
		EntryUri::new(
			self.crate_info.lang(),
			self.crate_info.crate_name(),
			&nudox_path_to_str(path),
		)
		.to_string()
	}

	fn kind_uri(&self, kind: &ir::kind::Entry, path: &ir::entry::NudoxPath) -> URI {
		KindUri::new(
			kind.schema_class(),
			self.crate_info.lang(),
			self.crate_info.crate_name(),
			&nudox_path_to_str(path),
		)
		.to_string()
	}

	/// Builds the path + concat with / between; used for kind_tag.
	fn uri_path(&self, path: &ir::entry::NudoxPath) -> URI {
		format!(
			"/{}/{}/{}",
			self.crate_info.lang(),
			self.crate_info.crate_name(),
			nudox_path_to_str(path),
		)
	}

	fn build_edge(uri: &URI) -> Value { json!({"@id": uri}) }
}

pub trait EmitJsonLD {
	/// Emit `self` as one or more JSON-LD documents into `sink`, returning the
	/// primary entry URI. The sink decides how each document is materialized
	/// (collected for tests, streamed to bytes for production).
	fn emit(self, ctx: &mut DocCtx, sink: &mut dyn DocSink) -> URI;
}
