//! `TSType` → `ir::ty::Type` lowering (OXC-PLAN §5.3), plus the record/field,
//! index-signature, and generic-parameter helpers built on top of it.
//!
//! LEAF FILE — fill the `todo!()` bodies. Bind to the `Extractor` state and the
//! `ir::*` target shapes; do not change signatures (the storm depends on them).

use oxc_ast::ast::{
	TSIndexSignature, TSLiteral, TSType, TSTypeLiteral, TSTypeParameterDeclaration,
};

use ir::{
	generics::{Generics, TraitRef, TypeExpr},
	kind::Visibility,
	primitives::Primitive,
	record::{Field, IndexSignature, Record},
	ty::Type,
};

use super::{Extractor, Result};

/// Metadata carried alongside a property when lowering it to an `ir` [`Field`]
/// (shared by class/interface/type-literal lowering).
pub struct PropertyFieldMetadata<'m> {
	pub optional: bool,
	pub readonly: bool,
	pub is_static: bool,
	pub visibility: Option<Visibility>,
	pub documentation: Option<String>,
	pub decorators: &'m [String],
}

impl<'a> Extractor<'a> {
	/// Lower a `TSType` node into `ir::ty::Type`. Dispatches over all ~37 oxc
	/// `TSType` variants per OXC-PLAN §5.3. Resolves `TSTypeReference` symbol
	/// ids into `type_ref_scratch` for later `type_links` (Phase 3.4).
	pub(crate) fn lower_ts_type(&mut self, ty: &TSType<'a>) -> Result<Type> {
		let _ = ty;
		todo!("types.rs: TSType dispatch (§5.3)")
	}

	/// Map a keyword type spelling to its `ir` primitive/reference (helper for
	/// the keyword `TSType` arms; see OXC-PLAN §5.3 keyword rows).
	pub(crate) fn keyword_type(&self, keyword: &str) -> Type {
		let _ = keyword;
		todo!("types.rs: keyword → Primitive/TypeReference")
	}

	/// Lower a `TSLiteralType`'s literal into `ir::ty::Type` (Tier A: kind →
	/// primitive; value carried in Tier B).
	pub(crate) fn literal_type(&mut self, lit: &TSLiteral<'a>) -> Type {
		let _ = lit;
		let _ = Primitive::String;
		todo!("types.rs: TSLiteral → primitive")
	}

	/// Lower a `TSTypeParameterDeclaration` into `ir::generics::Generics`
	/// (name, constraint → `TraitBound`, default → `TypeExpr`).
	pub(crate) fn lower_type_params(
		&mut self,
		params: &TSTypeParameterDeclaration<'a>,
	) -> Result<Option<Generics>> {
		let _ = params;
		todo!("types.rs: type parameters → Generics")
	}

	/// Flatten an `ir::ty::Type` into a `TypeExpr` (name + args) for use as a
	/// generic default / trait-ref argument.
	pub(crate) fn type_to_expr(&self, ty: &Type) -> TypeExpr {
		let _ = ty;
		todo!("types.rs: Type → TypeExpr")
	}

	/// Lower a type used in an `extends` / heritage position into a `TraitRef`.
	pub(crate) fn ts_type_to_trait_ref(&mut self, ty: &TSType<'a>) -> Result<TraitRef> {
		let _ = ty;
		todo!("types.rs: TSType → TraitRef")
	}

	/// Lower a `TSIndexSignature` into an `ir::record::IndexSignature`.
	pub(crate) fn index_signature(&mut self, sig: &TSIndexSignature<'a>) -> Result<IndexSignature> {
		let _ = sig;
		todo!("types.rs: TSIndexSignature → IndexSignature")
	}

	/// Lower a `TSTypeLiteral` (`{ k: V; m(): R }`) into an anonymous `Record`
	/// with its five member kinds (props / methods / call / construct / index).
	pub(crate) fn type_literal_record(
		&mut self,
		name: Option<String>,
		lit: &TSTypeLiteral<'a>,
	) -> Result<Record> {
		let _ = (name, lit);
		todo!("types.rs: TSTypeLiteral → Record")
	}

	/// Lower a single property into an `ir` [`Field::Known`].
	pub(crate) fn property_field(
		&mut self,
		name: &str,
		ty: Option<&TSType<'a>>,
		metadata: PropertyFieldMetadata<'_>,
	) -> Result<Field> {
		let _ = (name, ty, metadata.optional, metadata.readonly, metadata.is_static);
		todo!("types.rs: property → Field::Known")
	}
}
