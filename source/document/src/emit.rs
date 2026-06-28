use std::collections::HashSet;

use ir::entry::{Entry, NudoxPath};
use ir::kind::Visibility;
use serde_json::{Map, Value, json};
use tracing::{debug, instrument, warn};

use crate::{
	schema::{DocCtx, DocSink, DocStore, EmitJsonLD, URI, UriOps},
	ld::LDKind,
};
use identity::path::{fq_name, path_segments};

impl EmitJsonLD for Entry {
	fn emit(self, ctx: &mut DocCtx, sink: &mut dyn DocSink) -> URI {
		let path = self.path().clone();
		let entry_uri: URI = ctx.entry_uri(&path);

		// Borrow every field straight out of `&self`; the only owning copies are
		// the `Value`s actually placed in the document below. Nothing is cloned
		// into an intermediate tuple.
		type Fields<'a> = (
			Option<&'a Vec<NudoxPath>>,
			Option<&'a Vec<NudoxPath>>,
			Option<&'a Visibility>,
			Option<&'a str>,
			Option<&'a HashSet<Vec<String>, rustc_hash::FxBuildHasher>>,
			&'a str,
		);
		let (
			entry_members,
			implemented_protocols,
			entry_visibility,
			entry_documentation,
			aliases,
			name,
		): Fields = match &self {
			Entry::Module(s) => (
				s.inner.members.as_ref(),
				None,
				Some(&s.visibility),
				s.documentation.as_deref(),
				s.aliases.as_ref(),
				&s.name,
			),
			Entry::RecordType(s) => (
				s.inner.members.as_ref(),
				s.inner.implemented_protocols.as_ref(),
				Some(&s.visibility),
				s.documentation.as_deref(),
				s.aliases.as_ref(),
				&s.name,
			),
			Entry::Function(s) => (
				s.inner.members.as_ref(),
				s.inner.implemented_protocols.as_ref(),
				Some(&s.visibility),
				s.documentation.as_deref(),
				s.aliases.as_ref(),
				&s.name,
			),
			Entry::TraitDef(s) => (
				s.inner.members.as_ref(),
				None,
				Some(&s.visibility),
				s.documentation.as_deref(),
				s.aliases.as_ref(),
				&s.name,
			),
			Entry::TraitImpl(s) => (
				s.inner.members.as_ref(),
				None,
				Some(&s.visibility),
				s.documentation.as_deref(),
				s.aliases.as_ref(),
				&s.name,
			),
			Entry::Constant(s)
			| Entry::Variable(s)
			| Entry::Macro(s)
			| Entry::PrimitiveType(s)
			| Entry::Field(s)
			| Entry::Event(s) => (
				None,
				None,
				Some(&s.visibility),
				s.documentation.as_deref(),
				s.aliases.as_ref(),
				&s.name,
			),
			Entry::Info(s) => (
				None,
				None,
				Some(&s.visibility),
				s.documentation.as_deref(),
				s.aliases.as_ref(),
				&s.name,
			),
			Entry::UnionType(s) => (
				None,
				None,
				Some(&s.visibility),
				s.documentation.as_deref(),
				s.aliases.as_ref(),
				&s.name,
			),
			Entry::TypeAlias(s) => (
				None,
				None,
				Some(&s.visibility),
				s.documentation.as_deref(),
				s.aliases.as_ref(),
				&s.name,
			),
			Entry::SumType(s) => (
				None,
				None,
				Some(&s.visibility),
				s.documentation.as_deref(),
				s.aliases.as_ref(),
				&s.name,
			),
		};

		let members: Vec<String> = match entry_members {
			Some(members) => {
				members.iter().map(|member| ctx.entry_uri(member).as_str().to_owned()).collect()
			}
			None => Vec::default(),
		};
		let implemented_protocols: Vec<String> = match implemented_protocols {
			Some(protocols) => {
				protocols.iter().map(|protocol| ctx.entry_uri(protocol).as_str().to_owned()).collect()
			}
			None => Vec::default(),
		};

		let aliases_fq: Vec<String> = match aliases {
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
		obj.insert("name".into(), Value::String(name.to_owned()));
		obj.insert("members".into(), json!(members));
		obj.insert("implemented_protocols".into(), json!(implemented_protocols));

		if let Some(vis) = entry_visibility {
			obj.insert("visibility".into(), json!(vis));
		}

		if let Some(doc) = entry_documentation {
			obj.insert("documentation".into(), Value::String(doc.to_owned()));
		}

		let value = Value::Object(obj);

		sink.accept(entry_uri.clone(), value);
		ctx.update_path(&path);
		self.emit_kind(ctx, sink);

		entry_uri
	}
}

trait EntryOps {
	fn emit_kind(&self, ctx: &mut DocCtx, sink: &mut dyn DocSink) -> URI;
}

impl EntryOps for Entry {
	fn emit_kind(&self, ctx: &mut DocCtx, sink: &mut dyn DocSink) -> URI {
		let to_emit = LDKind::new(ctx.current_path.as_ref().unwrap(), self, ctx);
		let emitted_uri = to_emit.uri.clone();
		if let Ok(mut emitted_value) = serde_json::to_value(to_emit) {
			let obj = emitted_value.as_object_mut().expect("serde_json::to_value produced a non-object");
			obj.insert("@context".into(), ctx.context().clone());

			sink.accept(emitted_uri.clone(), emitted_value);
		} else {
			warn!(uri = %emitted_uri, "failed to serialize kind");
		}

		emitted_uri
	}
}

/// Emit every entry in `items` into `sink`, in iteration order.
///
/// This is the production driver: pair it with a streaming [`DocSink`] to emit a
/// whole crate without ever materializing the full corpus as `Value` trees.
/// [`Runner`] is the equivalent for the in-memory [`DocStore`] collection path.
pub fn emit_all<I>(ctx: &mut DocCtx, items: I, sink: &mut dyn DocSink)
where
	I: IntoIterator<Item = Entry>,
{
	for entry in items {
		debug!(name = %entry.name(), "emitting entry");
		let _ = entry.emit(ctx, sink);
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
