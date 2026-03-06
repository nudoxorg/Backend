pub mod ld;
pub mod termdb;
pub mod upload;

use ir::{entry::Entry, kind::Kind};
use serde_json::{Map, Value, json};
use termdb::{DocCtx, DocStore, EmitJsonLD, URI};
use tracing::{debug, instrument, warn};

use crate::terminusdb::{ld::LDKind, termdb::UriOps};

impl EmitJsonLD for Entry {
	fn emit(self, ctx: &mut DocCtx, docs: &mut DocStore) -> URI {
		let entry_uri: URI = ctx.entry_uri(&self.path);

		let fq_name: String = self.path.join("::");
		// No longer {"@id": "Ref"} -> ["ref1", .. "refn"] now
		let members: Vec<String> = match self.members.as_ref() {
			Some(members) => members.iter().map(|m| ctx.entry_uri(&m.path)).collect(),
			None => Vec::default(),
		};

		let aliases_fq: Vec<String> = match self.aliases.as_ref() {
			Some(aliaie) => aliaie.iter().map(|p| p.join("::")).collect(),
			None => vec![],
		};

		// construct kind reference
		// This needs to propogate through to the kind emit
		let prefix = self.kind.to_string();
		let kind_ref = prefix + &ctx.uri_path(&self.path);

		// output json object
		let mut obj = Map::<String, Value>::new();
		// inject context type ?? HINT Maybe refactor into the BtreeMap
		// iteartion in the runner to write them with the file there
		obj.insert("@context".into(), json!(ctx.context()));

		// required fields, always present!
		obj.insert("@type".into(), Value::String("Entry".into()));
		obj.insert("@id".into(), Value::String(entry_uri.as_str().to_owned()));
		obj.insert("kind".into(), Value::String(kind_ref.to_owned()));
		obj.insert("path".into(), json!(self.path));
		obj.insert("aliases".into(), json!(aliases_fq));
		obj.insert("name".into(), Value::String(self.name.clone()));
		obj.insert("fq_name".into(), Value::String(fq_name));
		obj.insert("members".into(), json!(members));

		if let Some(vis) = self.visibility.as_ref() {
			obj.insert("visibility".into(), json!(vis));
		}

		if let Some(doc) = self.documentation.as_deref() {
			obj.insert("documentation".into(), Value::String(doc.to_owned()));
		}

		let value = Value::Object(obj);

		match docs.insert(entry_uri.clone(), value) {
			Ok(_uri) => {}
			Err(uri) => {
				warn!(uri = %uri, "duplicate entry insertion with differing value");
			}
		}
		// call EmitJsonLD for the Kind Associated with this entry
		// first update context
		ctx.update_path(&self.path);
		// emit kind
		self.kind.emit(ctx, docs);

		entry_uri
	}
}

impl EmitJsonLD for Kind {
	fn emit(self, ctx: &mut DocCtx, docs: &mut DocStore) -> URI {
		// transform into LDKind
		let to_emit = LDKind::new(&ctx.current_path, self, ctx);
		let emitted_uri = to_emit.uri.clone();
		// Shouldnt fail to serialize
		if let Ok(mut emitted_value) = serde_json::to_value(to_emit) {
			let obj = emitted_value.as_object_mut().expect("serde_json::to_value produced a non-object");
			// TODO store the context at the DocStore root level.. Not on each document,
			// only when writing (not when being stored in the BtreeMap)
			obj.insert("@context".into(), ctx.context().clone());

			match docs.insert(emitted_uri.clone(), emitted_value) {
				Ok(_uri) => {}
				Err(uri) => {
					warn!(uri = %uri, "duplicate kind insertion with differing value");
				}
			}
		} else {
			warn!(uri = %emitted_uri, "failed to serialize kind");
		}

		emitted_uri
	}
}

pub struct Runner {
	ctx:  DocCtx,
	docs: DocStore,
}

impl Runner {
	pub fn new(ctx: DocCtx) -> Self { Self { ctx, docs: DocStore::new() } }

	#[instrument(skip_all, name = "runner")]
	pub fn run<I>(&mut self, items: I)
	where
		I: IntoIterator<Item = Entry>,
	{
		for entry in items {
			debug!(name = %entry.name, "emitting entry");
			let _ = entry.emit(&mut self.ctx, &mut self.docs);
		}
	}

	#[allow(dead_code)]
	pub fn docs(&self) -> &DocStore { &self.docs }

	#[allow(dead_code)]
	pub fn docs_mut(&mut self) -> &mut DocStore { &mut self.docs }

	pub fn into_docs(self) -> DocStore { self.docs }
}
