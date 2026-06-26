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
use std::{borrow::Cow, collections::BTreeMap};

use serde::Serialize;
use serde_json::{Value, json};
use tracing::warn;

use crate::identity::{EntryUri, path::nudox_path_to_str};

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
		if let Some(v) = self.docs.insert(uri.clone(), value.clone()) {
			if v == value {
				Ok(uri)
			} else {
				warn!(uri = %uri, "non-identical values inserted at same URI");
				// Collision! One value overwrites the other.
				// todo!("Handle URI collision for {}: decide if we should merge or
				// disambiguate.", uri);
				Err(uri)
			}
		} else {
			Ok(uri)
		}
	}

	/// Return all documents as sorted borrowed references.
	///
	/// This is the non-consuming projection path for:
	/// - embedding preparation
	/// - inspection/logging
	/// - future parallel upload prep
	pub fn documents_sorted(&self) -> Vec<(&URI, &Value)> {
		let mut keys: Vec<&URI> = self.docs.keys().collect();
		keys.sort();
		keys.into_iter().map(|k| (k, &self.docs[k])).collect()
	}

	/// Return all documents cloned into a sorted Vec<Value>.
	///
	/// This keeps the original store intact while matching the same ordering
	/// semantics as `into_documents`.
	pub fn documents_cloned(&self) -> Vec<Value> {
		let mut keys: Vec<&URI> = self.docs.keys().collect();
		keys.sort();
		keys.into_iter().map(|k| self.docs[k].clone()).collect()
	}

	/// Consume the store and return all documents as a sorted `Vec<Value>`,
	/// suitable for bulk upload via the TerminusDB client.
	pub fn into_documents(self) -> Vec<Value> {
		let mut keys: Vec<&URI> = self.docs.keys().collect();
		keys.sort();
		keys.into_iter().map(|k| self.docs[k].clone()).collect()
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
		let path_str = nudox_path_to_str(path);
		let prefix = kind.schema_class();
		format!("{}/{}/{}/{}", prefix, self.crate_info.lang(), self.crate_info.crate_name(), path_str)
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
	/// Takes some type, context, and document store. Returns a URI if succesfull,
	/// or some String for now if unsuccesfull.
	// TODO -> Look at Legitamate Error Handling, for now just return URI inserted
	// into HashMap of entry If already exists should not throw an error, but maybe
	// if the URI exists and points to a value that does NOT match, throw an error
	fn emit(self, ctx: &mut DocCtx, doc_store: &mut DocStore) -> URI;
}
