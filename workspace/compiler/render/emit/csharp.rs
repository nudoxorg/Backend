//! IR → C# surface syntax.
//!
//! Records map to `class`es, sum types to an `abstract record` plus a
//! `sealed record` per variant (or a plain `enum` when every variant is
//! unit-only), traits to `interface`s. C# has no unsigned integers beyond
//! `byte`, no raw references, and no built-in tuples in older targets, so
//! those collapse to their nearest analogue as documented inline.

use ir::function::{Attribute, Function};
use ir::generics::{GenericArg, Generics};
use ir::kind::Visibility;
use ir::parameter::Parameter;
use ir::primitives::{Primitive, Width};
use ir::protocols::{ReceiverKind, TraitDef, TraitMethod};
use ir::record::{Field, FieldKey, Record, SumField, SumVariant};
use ir::ty::{FunctionPointer, Type, TypeReference};

use super::super::backend::*;
use super::super::doc::Doc;
use super::{analyze_generics, GenericInfo};

pub struct CSharp;

impl Backend for CSharp {
	fn doc_comment(&self, text: &str) -> Rendered {
		let body = text.lines().map(|l| txt("/// ").annotate(Annotation::Comment) + txt(l));
		Doc::join(Doc::hardline(), body).annotate(Annotation::Comment)
	}

	fn ty(&self, t: &Type, cx: &RenderCtx) -> Rendered {
		ty(t, cx)
	}

	fn record(&self, name: &str, vis: &Visibility, rec: &Record, cx: &RenderCtx) -> Rendered {
		let info = analyze_generics(rec.generics.as_ref());
		let header = vis_prefix(vis) + kw("class") + sp() + tyname(name) + generics_decl(&info);

		// Base / implemented list
		let bases: Vec<Rendered> = rec
			.super_types
			.iter()
			.flatten()
			.map(|t| ty(t, cx))
			.collect();
		let header = if bases.is_empty() {
			header
		} else {
			header + sp() + punct(":") + sp() + Doc::join(punct(", "), bases)
		};

		let mut items: Vec<Rendered> = rec.fields.iter().filter_map(|f| field(f, cx)).collect();

		// Methods
		for m in rec.methods.iter().flatten() {
			items.push(method_decl(m, cx));
		}

		header + sp() + block("{", items, "}")
	}

	fn sum(
		&self,
		name: &str,
		generics: Option<&Generics>,
		variants: &[SumVariant],
		vis: &Visibility,
		cx: &RenderCtx,
	) -> Rendered {
		// If every variant is unit (no data), emit a C# enum.
		if variants.iter().all(|v| v.data.is_none()) {
			let items: Vec<Rendered> = variants.iter().map(|v| ident(&v.name) + punct(",")).collect();
			let header = vis_prefix(vis) + kw("enum") + sp() + tyname(name);
			return header + sp() + block("{", items, "}");
		}

		let info = super::analyze_sum_generics(generics, variants);
		let decl = generics_decl(&info);

		// `public abstract record Name<T>;`
		let base_header = vis_prefix(vis)
			+ kw("abstract")
			+ sp()
			+ kw("record")
			+ sp()
			+ tyname(name)
			+ decl.clone()
			+ punct(";");

		let mut out = base_header;
		for v in variants {
			let components = record_components(v, cx);
			// `public sealed record Variant<T>(T field0) : Base<T>;`
			let rec = kw("public")
				+ sp()
				+ kw("sealed")
				+ sp()
				+ kw("record")
				+ sp()
				+ tyname(&v.name)
				+ decl.clone()
				+ arglist("(", components, ")")
				+ sp()
				+ punct(":")
				+ sp()
				+ tyname(name)
				+ decl.clone()
				+ punct(";");
			out = out + Doc::hardline() + rec;
		}
		out
	}

	fn function(&self, name: &str, f: &Function, vis: &Visibility, cx: &RenderCtx) -> Rendered {
		let info = analyze_generics(f.generics.as_ref());
		let gd = generics_decl(&info);
		let type_params = if info.is_empty() { Doc::nil() } else { gd + sp() };

		let modifiers = function_modifiers(f, vis);
		let ret = return_type(f.output_parameters.as_deref(), cx);
		let params = value_params(f.input_parameters.as_deref(), cx);
		let method_name = super::to_pascal_case(name);

		modifiers + type_params + ret + sp() + ident(&method_name) + arglist("(", params, ")") + punct(";")
	}

	fn interface(&self, name: &str, def: &TraitDef, vis: &Visibility, cx: &RenderCtx) -> Rendered {
		let info = analyze_generics(def.generics.as_ref());
		let header =
			vis_prefix(vis) + kw("interface") + sp() + tyname(name) + generics_decl(&info);

		// Supertrait / interface extension
		let supers: Vec<Rendered> = def
			.super_traits
			.iter()
			.flatten()
			.map(|tr| tyname(&tr.name))
			.collect();
		let header = if supers.is_empty() {
			header
		} else {
			header + sp() + punct(":") + sp() + Doc::join(punct(", "), supers)
		};

		let mut methods: Vec<Rendered> = def
			.required_methods
			.iter()
			.flatten()
			.map(|m| method_sig(m, cx))
			.collect();

		// Provided methods get a default-interface-member stub.
		for m in def.provided_methods.iter().flatten() {
			methods.push(method_sig_with_body(m, cx));
		}

		header + sp() + block("{", methods, "}")
	}
}

// ---------------------------------------------------------------------------
// Visibility
// ---------------------------------------------------------------------------

fn vis_prefix(v: &Visibility) -> Rendered {
	match v {
		Visibility::Public => kw("public") + sp(),
		Visibility::Protected => kw("protected") + sp(),
		Visibility::Private => kw("private") + sp(),
		Visibility::Internal | Visibility::Package => kw("internal") + sp(),
	}
}

// ---------------------------------------------------------------------------
// Generics
// ---------------------------------------------------------------------------

fn generics_decl(info: &GenericInfo) -> Rendered {
	if info.types.is_empty() {
		return Doc::nil();
	}
	let params: Vec<Rendered> = info
		.types
		.iter()
		.map(|name| {
			let bounds: Vec<&str> = info.bounds_of(name).collect();
			if bounds.is_empty() {
				tyname(name)
			} else {
				// C# uses `where T : Bound` clauses outside the `<…>` list, but
				// for simplicity we inline the first bound as a comment and emit
				// just the naked type parameter here.
				tyname(name)
			}
		})
		.collect();
	generic_list("<", params, ">")
}

// ---------------------------------------------------------------------------
// Fields
// ---------------------------------------------------------------------------

fn field(f: &Field, cx: &RenderCtx) -> Option<Rendered> {
	let Field::Known(kf) = f else { return None };
	let name = super::to_pascal_case(&field_name(&kf.key));
	let base = kf
		.r#type
		.as_ref()
		.map(|t| ty(t, cx))
		.unwrap_or_else(|| tyname("object"));
	let t = if kf.attributes.is_optional {
		base + punct("?")
	} else {
		base
	};
	let vis_doc = kf
		.visibility
		.as_ref()
		.map(vis_prefix)
		.unwrap_or_else(|| kw("public") + sp());
	// Emit as a C# auto-property: `public T Name { get; set; }`
	Some(vis_doc + t + sp() + ident(&name) + sp() + txt("{ get; set; }"))
}

fn field_name(key: &FieldKey) -> String {
	match key {
		FieldKey::Ident(n) => n.clone(),
		FieldKey::Index(i) => format!("Field{i}"),
		FieldKey::Computed(_) => "Field".into(),
	}
}

// ---------------------------------------------------------------------------
// Sum-variant components
// ---------------------------------------------------------------------------

fn record_components(v: &SumVariant, cx: &RenderCtx) -> Vec<Rendered> {
	match &v.data {
		None => Vec::new(),
		Some(SumField::Tuple(tys)) => tys
			.iter()
			.enumerate()
			.map(|(i, t)| ty(t, cx) + sp() + ident(&format!("Field{i}")))
			.collect(),
		Some(SumField::StructLike(fields)) => fields
			.iter()
			.filter_map(|f| match f {
				Field::Known(kf) => {
					let t = kf.r#type.as_ref().map(|t| ty(t, cx))?;
					Some(t + sp() + ident(&super::to_pascal_case(&field_name(&kf.key))))
				}
				_ => None,
			})
			.collect(),
	}
}

// ---------------------------------------------------------------------------
// Parameters / return types
// ---------------------------------------------------------------------------

fn value_params(inputs: Option<&[Parameter]>, cx: &RenderCtx) -> Vec<Rendered> {
	inputs
		.into_iter()
		.flatten()
		.filter_map(|p| match p {
			Parameter::Literal(lp) => {
				let t =
					lp.r#type.as_ref().map(|t| ty(t, cx)).unwrap_or_else(|| tyname("object"));
				Some(t + sp() + ident(&lp.name))
			}
			_ => None,
		})
		.collect()
}

fn return_type(outputs: Option<&[Parameter]>, cx: &RenderCtx) -> Rendered {
	outputs
		.into_iter()
		.flatten()
		.find_map(|p| match p {
			Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| ty(t, cx)),
			_ => None,
		})
		.unwrap_or_else(|| kw("void"))
}

// ---------------------------------------------------------------------------
// Function modifiers
// ---------------------------------------------------------------------------

fn function_modifiers(f: &Function, vis: &Visibility) -> Rendered {
	let vp = vis_prefix(vis);
	let is_static = matches!(f.receiver, Some(ReceiverKind::Static) | None);
	let is_async = f.attributes.iter().flatten().any(|a| matches!(a, Attribute::Async));

	let mut mods = vp;
	if is_static {
		mods = mods + kw("static") + sp();
	}
	if is_async {
		mods = mods + kw("async") + sp();
	}
	mods
}

// ---------------------------------------------------------------------------
// Method rendering (records + interfaces)
// ---------------------------------------------------------------------------

fn method_decl(f: &Function, cx: &RenderCtx) -> Rendered {
	let info = analyze_generics(f.generics.as_ref());
	let gd = generics_decl(&info);
	let type_params = if info.is_empty() { Doc::nil() } else { gd + sp() };
	let is_static = matches!(f.receiver, Some(ReceiverKind::Static) | None);
	let is_async = f.attributes.iter().flatten().any(|a| matches!(a, Attribute::Async));

	let mut mods = kw("public") + sp();
	if is_static {
		mods = mods + kw("static") + sp();
	}
	if is_async {
		mods = mods + kw("async") + sp();
	}

	let ret = return_type(f.output_parameters.as_deref(), cx);
	let params = value_params(f.input_parameters.as_deref(), cx);
	mods + type_params + ret + sp() + arglist("(", params, ")") + punct(";")
}

fn method_sig(m: &TraitMethod, cx: &RenderCtx) -> Rendered {
	let ret = m
		.return_type
		.as_ref()
		.map(|t| ty(t, cx))
		.unwrap_or_else(|| kw("void"));
	let params = value_params(m.parameters.as_deref(), cx);
	let method_name = super::to_pascal_case(&m.name);
	ret + sp() + ident(&method_name) + arglist("(", params, ")") + punct(";")
}

/// Render a provided (default) interface method as a default interface member.
/// C# 8+ allows `=> throw new NotImplementedException();` as the body.
fn method_sig_with_body(m: &TraitMethod, cx: &RenderCtx) -> Rendered {
	let ret = m
		.return_type
		.as_ref()
		.map(|t| ty(t, cx))
		.unwrap_or_else(|| kw("void"));
	let params = value_params(m.parameters.as_deref(), cx);
	let method_name = super::to_pascal_case(&m.name);
	ret + sp()
		+ ident(&method_name)
		+ arglist("(", params, ")")
		+ sp()
		+ punct("=>")
		+ sp()
		+ kw("throw")
		+ sp()
		+ kw("new")
		+ sp()
		+ tyname("NotImplementedException")
		+ punct("();")
}

// ---------------------------------------------------------------------------
// Type rendering
// ---------------------------------------------------------------------------

fn ty(t: &Type, cx: &RenderCtx) -> Rendered {
	match t {
		Type::TypeReference(r) => type_reference(r, cx),
		Type::Primitive(p) => tyname(primitive(p)),
		Type::GenericParam(g) => tyname(&g.name),
		Type::SelfType => tyname("T"),

		// Slices and fixed arrays both map to T[]
		Type::Slice(inner) | Type::Array { r#type: inner, .. } => ty(inner, cx) + punct("[]"),

		// BorrowedRef: mutable → `ref T`, immutable → `in T` (call-site hint only)
		Type::BorrowedRef { is_mutable, r#type, .. } => {
			if *is_mutable {
				kw("ref") + sp() + ty(r#type, cx)
			} else {
				kw("in") + sp() + ty(r#type, cx)
			}
		}

		// Raw pointer: `T*` (unsafe context)
		Type::RawPointer { r#type, .. } => ty(r#type, cx) + punct("*"),

		// Unit tuple → void; value tuples → (T1, T2, …)
		Type::Tuple(ts) if ts.is_empty() => kw("void"),
		Type::Tuple(ts) => {
			let items: Vec<Rendered> = ts.iter().map(|t| ty(t, cx)).collect();
			arglist("(", items, ")")
		}

		// Named tuples: (int x, string y)
		Type::NamedTuple(members) => {
			let items: Vec<Rendered> = members
				.iter()
				.map(|m| {
					let t = ty(&m.r#type, cx);
					match &m.label {
						Some(label) => t + sp() + ident(label),
						None => t,
					}
				})
				.collect();
			arglist("(", items, ")")
		}

		// Union: C# has no native untagged union; collapse to `object`
		// and leave a comment so the reader knows information was lost.
		Type::Union(_) => {
			txt("/* union */ ").annotate(Annotation::Comment) + tyname("object")
		}

		Type::Any | Type::Infer => tyname("object"),
		Type::Never => tyname("void"), // closest approximation; C# uses `void` / exception

		// FunctionPointer → Func<…> / Action
		Type::FunctionPointer(fp) => function_pointer(fp, cx),

		// TypeOperator: handle `?` (nullable ref), `[,]`/`[,,]` (multidim arrays)
		Type::TypeOperator(op) => type_operator(&op.operator, &op.r#type, cx),

		_ => tyname("object"),
	}
}

fn type_reference(r: &TypeReference, cx: &RenderCtx) -> Rendered {
	let sn = short_name(&r.identifier, cx);
	let type_args: Vec<Rendered> = r
		.generic_args
		.as_deref()
		.unwrap_or(&[])
		.iter()
		.filter_map(|a| match a {
			GenericArg::Type(t) => Some(ty(t, cx)),
			_ => None,
		})
		.collect();
	if let Some(known) = super::known_type(sn, &type_args, Language::CSharp) {
		return known;
	}
	let base = tyname(sn);
	match &r.generic_args {
		Some(ga) if !ga.is_empty() => {
			let items: Vec<Rendered> = ga
				.iter()
				.filter_map(|a| match a {
					GenericArg::Type(t) => Some(ty(t, cx)),
					_ => None,
				})
				.collect();
			base + generic_list("<", items, ">")
		}
		_ => base,
	}
}

fn function_pointer(fp: &FunctionPointer, cx: &RenderCtx) -> Rendered {
	let inputs: Vec<Rendered> = fp
		.inputs
		.iter()
		.flatten()
		.filter_map(|p| match p {
			Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| ty(t, cx)),
			_ => None,
		})
		.collect();

	let ret_opt = fp
		.outputs
		.iter()
		.flatten()
		.find_map(|p| match p {
			Parameter::Literal(lp) => lp.r#type.as_ref().map(|t| ty(t, cx)),
			_ => None,
		});

	match ret_opt {
		None => {
			// Action / Action<T1,T2,…>
			if inputs.is_empty() {
				tyname("Action")
			} else {
				tyname("Action") + generic_list("<", inputs, ">")
			}
		}
		Some(ret) => {
			// Func<T1, T2, …, TReturn>
			let mut args = inputs;
			args.push(ret);
			tyname("Func") + generic_list("<", args, ">")
		}
	}
}

fn type_operator(op: &str, inner: &Type, cx: &RenderCtx) -> Rendered {
	match op {
		// Nullable reference: `T?`
		"?" => ty(inner, cx) + punct("?"),
		// NRT non-null annotation (IR fidelity marker) — surface is bare `T`.
		"!" => ty(inner, cx),
		// Oblivious nullability (no NRT context) — surface is bare `T`.
		"~" | "oblivious" => ty(inner, cx),
		// Multi-dimensional arrays
		"[,]" => ty(inner, cx) + punct("[,]"),
		"[,,]" => ty(inner, cx) + punct("[,,]"),
		"[,,,]" => ty(inner, cx) + punct("[,,,]"),
		// Fallback: render the inner type (unknown operators are doc-only).
		_ => ty(inner, cx),
	}
}

fn primitive(p: &Primitive) -> &'static str {
	match p {
		Primitive::Int(w) => match w {
			Width::W8 => "sbyte",
			Width::W16 => "short",
			Width::W32 => "int",
			Width::W64 => "long",
			Width::W128 => "Int128",
			Width::Arch => "nint",
		},
		Primitive::UInt(w) => match w {
			Width::W8 => "byte",
			Width::W16 => "ushort",
			Width::W32 => "uint",
			Width::W64 => "ulong",
			Width::W128 => "UInt128",
			Width::Arch => "nuint",
		},
		Primitive::Float(w) => match w {
			Width::W32 => "float",
			_ => "double",
		},
		Primitive::Bool => "bool",
		Primitive::String => "string",
		Primitive::Char => "char",
		Primitive::Bytes => "byte[]",
		Primitive::Date => "DateTimeOffset",
		Primitive::Address => "nuint",
	}
}
