pub mod embedding_service;
pub mod ld;
pub mod qdrant_upload;
pub mod termdb;
pub mod upload;

use ir::entry::Entry;
use serde_json::{Map, Value, json};
use termdb::{DocCtx, DocStore, EmitJsonLD, URI};
use tracing::{debug, instrument, warn};

use crate::terminusdb::{ld::LDKind, termdb::UriOps};

fn path_segments(path: &ir::entry::NudoxPath) -> Vec<String> {
	match path {
		ir::entry::NudoxPath::Local(local_path) => {
			local_path.iter().map(|segment| segment.to_string_lossy().into_owned()).collect()
		}
		ir::entry::NudoxPath::External { path, dependency } => std::iter::once(dependency.clone())
			.chain(path.iter().map(|segment| segment.to_string_lossy().into_owned()))
			.collect(),
	}
}

fn fq_name(path: &ir::entry::NudoxPath) -> String { path_segments(path).join("::") }

impl EmitJsonLD for Entry {
	fn emit(self, ctx: &mut DocCtx, docs: &mut DocStore) -> URI {
		let path = self.path().clone();
		let entry_uri: URI = ctx.entry_uri(&path);

		let (
			entry_members,
			implemented_protocols,
			entry_visibility,
			entry_documentation,
			aliases,
			name,
		) = match &self {
			Entry::Module(s) => (
				s.inner.members.clone(),
				None,
				Some(s.visibility.clone()),
				s.documentation.clone(),
				s.aliases.clone(),
				s.name.clone(),
			),
			Entry::RecordType(s) => (
				s.inner.members.clone(),
				s.inner.implemented_protocols.clone(),
				Some(s.visibility.clone()),
				s.documentation.clone(),
				s.aliases.clone(),
				s.name.clone(),
			),
			Entry::Function(s) => (
				s.inner.members.clone(),
				s.inner.implemented_protocols.clone(),
				Some(s.visibility.clone()),
				s.documentation.clone(),
				s.aliases.clone(),
				s.name.clone(),
			),
			Entry::TraitDef(s) => (
				s.inner.members.clone(),
				None,
				Some(s.visibility.clone()),
				s.documentation.clone(),
				s.aliases.clone(),
				s.name.clone(),
			),
			Entry::TraitImpl(s) => (
				s.inner.members.clone(),
				None,
				Some(s.visibility.clone()),
				s.documentation.clone(),
				s.aliases.clone(),
				s.name.clone(),
			),
			Entry::Constant(s)
			| Entry::Variable(s)
			| Entry::Macro(s)
			| Entry::PrimitiveType(s)
			| Entry::Field(s)
			| Entry::Event(s) => (
				None,
				None,
				Some(s.visibility.clone()),
				s.documentation.clone(),
				s.aliases.clone(),
				s.name.clone(),
			),
			Entry::Info(s) => (
				None,
				None,
				Some(s.visibility.clone()),
				s.documentation.clone(),
				s.aliases.clone(),
				s.name.clone(),
			),
			Entry::UnionType(s) => (
				None,
				None,
				Some(s.visibility.clone()),
				s.documentation.clone(),
				s.aliases.clone(),
				s.name.clone(),
			),
			Entry::TypeAlias(s) => (
				None,
				None,
				Some(s.visibility.clone()),
				s.documentation.clone(),
				s.aliases.clone(),
				s.name.clone(),
			),
			Entry::SumType(s) => (
				None,
				None,
				Some(s.visibility.clone()),
				s.documentation.clone(),
				s.aliases.clone(),
				s.name.clone(),
			),
		};

		let members: Vec<String> = match entry_members.as_ref() {
			Some(members) => {
				members.iter().map(|member| ctx.entry_uri(member).as_str().to_owned()).collect()
			}
			None => Vec::default(),
		};
		let implemented_protocols: Vec<String> = match implemented_protocols.as_ref() {
			Some(protocols) => {
				protocols.iter().map(|protocol| ctx.entry_uri(protocol).as_str().to_owned()).collect()
			}
			None => Vec::default(),
		};

		let aliases_fq: Vec<String> = match aliases.as_ref() {
			Some(aliaie) => aliaie.iter().map(|p| p.join("::")).collect(),
			None => vec![],
		};

		let kind_ref = ctx.kind_uri(&self, &path);

		let mut obj = Map::<String, Value>::new();
		obj.insert("@context".into(), json!(ctx.context()));

		obj.insert("@type".into(), Value::String("Entry".into()));
		obj.insert("@id".into(), Value::String(entry_uri.as_str().to_owned()));
		obj.insert("kind".into(), Value::String(kind_ref.as_str().to_owned()));
		obj.insert("path".into(), json!(path_segments(&path)));
		obj.insert("fq_name".into(), Value::String(fq_name(&path)));
		obj.insert("aliases".into(), json!(aliases_fq));
		obj.insert("name".into(), Value::String(name));
		obj.insert("members".into(), json!(members));
		obj.insert("implemented_protocols".into(), json!(implemented_protocols));

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
			debug!(name = %entry.name(), "emitting entry");
			let _ = entry.emit(&mut self.ctx, &mut self.docs);
		}
	}

	pub fn into_docs(self) -> DocStore { self.docs }
}
