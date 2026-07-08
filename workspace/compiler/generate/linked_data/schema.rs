//! The JSON-LD schema/context for the emitted linked data.
//!
//! The document *shapes* are derived from the graph model (`crate::graph::model`)
//! via `terminusdb-schema-derive`, so this module does not hand-write a schema.
//! It provides the JSON-LD `@context` that binds the model's field names to the
//! graph store's vocabulary, and the base IRI prefixes documents resolve against.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use terminusdb_schema::ToTDBSchema;

use crate::graph::model;

/// The base IRI every emitted document's `@id` is minted under.
pub const BASE_IRI: &str = "https://nudox.org/ir/";

/// The JSON-LD `@context` for the emitted graph documents — the field→predicate
/// bindings the graph store ingests against. Field names resolve against
/// `@schema`; the class vocabulary rides along in `@metadata`, walked from the
/// model's derived schema trees so it can never drift from what [`super::emit`]
/// serializes.
pub fn context() -> Value {
	// Every emitted document type roots one schema tree; the union (each tree
	// pulls in its transitively reachable subdocument classes) is the whole
	// vocabulary.
	let mut classes = BTreeSet::new();
	classes.extend(model::Package::to_schema_tree().iter().map(|s| s.class_name().clone()));
	classes.extend(model::PackageVersion::to_schema_tree().iter().map(|s| s.class_name().clone()));
	classes.extend(model::Symbol::to_schema_tree().iter().map(|s| s.class_name().clone()));
	classes.extend(model::Implementation::to_schema_tree().iter().map(|s| s.class_name().clone()));
	classes.extend(model::Reference::to_schema_tree().iter().map(|s| s.class_name().clone()));

	json!({
		"@type": "@context",
		"@base": BASE_IRI,
		"@schema": format!("{BASE_IRI}schema#"),
		"@metadata": {
			"model_classes": classes.into_iter().collect::<Vec<_>>(),
		},
	})
}
