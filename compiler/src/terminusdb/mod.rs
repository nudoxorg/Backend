pub mod ld;
pub mod termdb;
pub mod upload;

use ir::entry::Entry;
use serde_json::{Map, Value, json};
use termdb::{DocCtx, DocStore, EmitJsonLD, URI};
use tracing::warn;

use crate::terminusdb::{ld::LDKind, termdb::UriOps};

impl EmitJsonLD for Entry {
	fn emit(self, ctx: &mut DocCtx, docs: &mut DocStore) -> URI {
		let path = self.path().clone();
		let entry_uri: URI = ctx.entry_uri(&path);

		let (entry_members, entry_visibility, entry_documentation, aliases, name) = match &self {
			Entry::Module(s) => {
				(s.inner.members.clone(), Some(s.visibility.clone()), s.documentation.clone(), s.aliases.clone(), s.name.clone())
			}
			Entry::RecordType(s) => {
				(s.inner.members.clone(), Some(s.visibility.clone()), s.documentation.clone(), s.aliases.clone(), s.name.clone())
			}
			Entry::Function(s) => {
				(s.inner.members.clone(), Some(s.visibility.clone()), s.documentation.clone(), s.aliases.clone(), s.name.clone())
			}
			Entry::TraitDef(s) => {
				(s.inner.members.clone(), Some(s.visibility.clone()), s.documentation.clone(), s.aliases.clone(), s.name.clone())
			}
			Entry::TraitImpl(s) => {
				(s.inner.members.clone(), Some(s.visibility.clone()), s.documentation.clone(), s.aliases.clone(), s.name.clone())
			}
			Entry::Constant(s) | Entry::Variable(s) | Entry::Macro(s) | Entry::PrimitiveType(s) | Entry::Field(s) | Entry::Event(s) => {
				(None, Some(s.visibility.clone()), s.documentation.clone(), s.aliases.clone(), s.name.clone())
			}
			Entry::Info(s) => {
				(None, Some(s.visibility.clone()), s.documentation.clone(), s.aliases.clone(), s.name.clone())
			}
			Entry::UnionType(s) => {
				(None, Some(s.visibility.clone()), s.documentation.clone(), s.aliases.clone(), s.name.clone())
			}
			Entry::TypeAlias(s) => {
				(None, Some(s.visibility.clone()), s.documentation.clone(), s.aliases.clone(), s.name.clone())
			}
			Entry::SumType(s) => {
				(None, Some(s.visibility.clone()), s.documentation.clone(), s.aliases.clone(), s.name.clone())
			}
		};

		let members: Vec<String> = match entry_members.as_ref() {
			Some(members) => members.iter().map(|m| ctx.entry_uri(m)).collect(),
			None => Vec::default(),
		};

		let aliases_fq: Vec<String> = match aliases.as_ref() {
			Some(aliaie) => aliaie.iter().map(|p| p.join("::")).collect(),
			None => vec![],
		};

		let prefix = self.to_string();
		let kind_ref: String = prefix + ctx.uri_path(&path).as_str();

		let mut obj = Map::<String, Value>::new();
		obj.insert("@context".into(), json!(ctx.context()));

		obj.insert("@type".into(), Value::String("Entry".into()));
		obj.insert("@id".into(), Value::String(entry_uri.as_str().to_owned()));
		obj.insert("kind".into(), Value::String(kind_ref.to_owned()));
		obj.insert("path".into(), json!(path));
		obj.insert("aliases".into(), json!(aliases_fq));
		obj.insert("name".into(), Value::String(name));
		obj.insert("members".into(), json!(members));

		if let Some(vis) = entry_visibility.as_ref() {
			obj.insert("visibility".into(), json!(vis));
		}

		if let Some(doc) = entry_documentation.as_deref() {
			obj.insert("documentation".into(), Value::String(doc.to_owned()));
		}

		let value = Value::Object(obj);

		match docs.insert(entry_uri.clone(), value) {
			Ok(_uri) => {}
			Err(uri) => {
				warn!(uri = %uri, "duplicate entry insertion with differing value");
			}
		}
		ctx.update_path(&path);
		self.emit_kind(ctx, docs);

		entry_uri
	}
}

trait EntryOps {
	fn emit_kind(&self, ctx: &mut DocCtx, docs: &mut DocStore) -> URI;
}

impl EntryOps for Entry {
	fn emit_kind(&self, ctx: &mut DocCtx, docs: &mut DocStore) -> URI {
		let to_emit = LDKind::new(ctx.current_path.as_ref().unwrap(), self, ctx);
		let emitted_uri = to_emit.uri.clone();
		if let Ok(mut emitted_value) = serde_json::to_value(to_emit) {
			let obj = emitted_value.as_object_mut().expect("serde_json::to_value produced a non-object");
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
