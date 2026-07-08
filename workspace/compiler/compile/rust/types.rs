//! Lowering `rustdoc_types::Type` into `ir::ty::Type`, plus visibility mapping
//! and type → entry-id resolution.

use ir::{generics::{GenericArg, Term, TraitRef, TypeExpr}, kind::Visibility, primitives::{Primitive, Width}, ty::{DynTrait, PolyTrait, QualifiedPath, Type, TypeReference}};
use rustdoc_types::{Id, ItemEnum};

use super::{Result, context::ParseContext, error::Parse};

impl ParseContext {
	/// Map rustdoc's visibility onto the IR's.
	pub(super) fn visibility(&self, raw: &rustdoc_types::Visibility) -> Visibility {
		match raw {
			rustdoc_types::Visibility::Public => Visibility::Public,
			rustdoc_types::Visibility::Default => Visibility::Private,
			rustdoc_types::Visibility::Crate => Visibility::Internal,
			rustdoc_types::Visibility::Restricted { .. } => Visibility::Package,
		}
	}

	pub(super) fn union_fields(&self, u: &rustdoc_types::Union) -> Result<Vec<Type>> {
		u.fields
			.iter()
			.map(|id| {
				let item = self.krate.index.get(id).ok_or(Parse::ItemNotFound(id.0))?;
				if let ItemEnum::StructField(ty) = &item.inner {
					self.type_(ty)
				} else {
					Err(Parse::InvalidItemKind {
						id:       id.0.to_string(),
						expected: "StructField".to_string(),
						actual:   format!("{:?}", item.inner),
					})
				}
			})
			.collect()
	}

	pub(super) fn resolve_path_to_id(&self, path: &str) -> Option<&Id> {
		if let Some(id) = self.path_to_id.get(path) {
			return Some(id);
		}

		for (key, id) in &self.path_to_id {
			if key.ends_with(&format!("::{}", path)) || key == path {
				return Some(id);
			}
		}

		None
	}

	pub(super) fn resolve_type_to_entry_id(&self, ty: &Type) -> Option<i64> {
		match ty {
			Type::TypeReference(tr) => {
				self.resolve_path_to_id(&tr.identifier).map(|id| self.id_to_number(id))
			}
			Type::Primitive(prim) => {
				let name = match prim {
					Primitive::Int(Width::W8) => "i8",
					Primitive::Int(Width::W16) => "i16",
					Primitive::Int(Width::Arch) => "isize",
					Primitive::Int(Width::W64) => "i64",
					Primitive::Int(Width::W128) => "i128",
					Primitive::UInt(Width::W8) => "u8",
					Primitive::UInt(Width::W16) => "u16",
					Primitive::UInt(Width::Arch) => "usize",
					Primitive::UInt(Width::W64) => "u64",
					Primitive::UInt(Width::W128) => "u128",
					Primitive::Float(Width::W16) => "f16",
					Primitive::Float(Width::W32) => "f32",
					Primitive::Float(Width::W64) => "f64",
					Primitive::Bool => "bool",
					Primitive::String => "str",
					Primitive::Char => "char",
					_ => return None,
				};
				self.primitive_map.get(name).map(|id| self.id_to_number(id))
			}
			_ => None,
		}
	}

	pub(super) fn id_to_number(&self, id: &Id) -> i64 {
		use std::{collections::hash_map::DefaultHasher, hash::{Hash, Hasher}};

		let mut hasher = DefaultHasher::new();
		id.0.hash(&mut hasher);
		hasher.finish() as i64
	}

	pub(super) fn type_(&self, ty: &rustdoc_types::Type) -> Result<Type> {
		match ty {
			rustdoc_types::Type::ResolvedPath(path) => {
				if path.path == "Self" {
					return Ok(Type::SelfType);
				}
				let ir_path = self.resolved_path(path)?;
				Ok(Type::TypeReference(ir_path))
			}

			rustdoc_types::Type::DynTrait(dyn_trait) => {
				let traits =
					dyn_trait.traits.iter().map(|pt| self.poly_trait(pt)).collect::<Result<Vec<_>>>()?;

				Ok(Type::DynTrait(DynTrait { traits, lifetime: dyn_trait.lifetime.clone() }))
			}

			rustdoc_types::Type::Generic(name) => {
				if name == "Self" {
					Ok(Type::SelfType)
				} else {
					Ok(Type::GenericParam(ir::ty::GenericParam { name: name.clone(), kind: None }))
				}
			}

			rustdoc_types::Type::Primitive(prim) => {
				if prim == "never" || prim == "!" {
					Ok(Type::Never)
				} else {
					Ok(Type::Primitive(self.primitive(prim)?))
				}
			}

			rustdoc_types::Type::FunctionPointer(fp) => {
				let function_pointer = self.function_pointer(fp)?;
				Ok(Type::FunctionPointer(function_pointer))
			}

			rustdoc_types::Type::Tuple(types) => {
				let parsed_types = types.iter().map(|t| self.type_(t)).collect::<Result<Vec<_>>>()?;
				Ok(Type::Tuple(parsed_types))
			}

			rustdoc_types::Type::Slice(inner) => {
				let parsed_inner = Box::new(self.type_(inner)?);
				Ok(Type::Slice(parsed_inner))
			}

			rustdoc_types::Type::Array { type_, len } => {
				let parsed_ty = Box::new(self.type_(type_)?);
				let length = len.parse::<usize>().map_err(|_| Parse::TypeResolution {
					type_name: "array".to_string(),
					reason:    format!("Invalid array length: {}", len),
				})?;
				Ok(Type::Array { r#type: parsed_ty, length })
			}

			rustdoc_types::Type::Pat { .. } => Ok(Type::Infer),

			rustdoc_types::Type::ImplTrait(bounds) => {
				let generic_bounds = self.generic_bounds(bounds)?;
				Ok(Type::ImplTrait(generic_bounds))
			}

			rustdoc_types::Type::Infer => Ok(Type::Infer),

			rustdoc_types::Type::RawPointer { is_mutable, type_ } => {
				let parsed_ty = Box::new(self.type_(type_)?);
				Ok(Type::RawPointer { is_mutable: *is_mutable, r#type: parsed_ty })
			}

			rustdoc_types::Type::BorrowedRef { lifetime, is_mutable, type_ } => {
				let parsed_ty = Box::new(self.type_(type_)?);
				Ok(Type::BorrowedRef {
					lifetime:   lifetime.clone(),
					is_mutable: *is_mutable,
					r#type:     parsed_ty,
				})
			}

			rustdoc_types::Type::QualifiedPath { name, args, self_type, trait_ } => {
				let parsed_self_type = Box::new(self.type_(self_type)?);
				let parsed_trait = trait_.as_ref().map(|path| self.resolved_path(path)).transpose()?;
				let generic_args =
					args.as_ref().map(|ga| self.generic_args(ga)).transpose()?.filter(|v| !v.is_empty());

				Ok(Type::QualifiedPath(QualifiedPath {
					name:              name.clone(),
					generic_arguments: generic_args,
					self_type:         parsed_self_type,
					tr:                parsed_trait,
				}))
			}
		}
	}

	pub(super) fn resolved_path(&self, path: &rustdoc_types::Path) -> Result<TypeReference> {
		let path_str = &path.path;
		let generic_args =
			path.args.as_ref().map(|ga| self.generic_args(ga)).transpose()?.filter(|v| !v.is_empty());

		Ok(TypeReference { identifier: path_str.clone(), generic_args })
	}

	pub(super) fn poly_trait(&self, pt: &rustdoc_types::PolyTrait) -> Result<PolyTrait> {
		let trait_ref = self.path_to_trait_ref(&pt.trait_)?;
		Ok(PolyTrait {
			trait_ref,
			lifetimes: pt.generic_params.iter().map(|gp| gp.name.clone()).collect(),
		})
	}

	pub(super) fn path_to_trait_ref(&self, path: &rustdoc_types::Path) -> Result<TraitRef> {
		let args = path
			.args
			.as_ref()
			.map(|ga| {
				self.generic_args(ga).and_then(|args| {
					args
						.into_iter()
						.map(|arg| match arg {
							GenericArg::Type(ty) => Ok(TypeExpr { name: format!("{:?}", ty), args: vec![] }),
							GenericArg::ConstExpr(ce) => Ok(TypeExpr { name: format!("{:?}", ce), args: vec![] }),
							GenericArg::Lifetime(lt) => Ok(TypeExpr { name: lt, args: vec![] }),
							GenericArg::Constraint(_) => Ok(TypeExpr { name: String::new(), args: vec![] }),
							GenericArg::Module(_) => Ok(TypeExpr { name: String::new(), args: vec![] }),
						})
						.collect()
				})
			})
			.transpose()?
			.unwrap_or_default();

		Ok(TraitRef { name: path.path.clone(), args })
	}

	pub(super) fn primitive(&self, prim: &str) -> Result<Primitive> {
		match prim {
			"i8" => Ok(Primitive::Int(Width::W8)),
			"i16" => Ok(Primitive::Int(Width::W16)),
			"i32" | "isize" => Ok(Primitive::Int(Width::Arch)),
			"i64" => Ok(Primitive::Int(Width::W64)),
			"i128" => Ok(Primitive::Int(Width::W128)),
			"u8" => Ok(Primitive::UInt(Width::W8)),
			"u16" => Ok(Primitive::UInt(Width::W16)),
			"u32" | "usize" => Ok(Primitive::UInt(Width::Arch)),
			"u64" => Ok(Primitive::UInt(Width::W64)),
			"u128" => Ok(Primitive::UInt(Width::W128)),
			"f16" => Ok(Primitive::Float(Width::W16)),
			"f32" => Ok(Primitive::Float(Width::W32)),
			"f64" | "f128" => Ok(Primitive::Float(Width::W64)),
			"bool" => Ok(Primitive::Bool),
			"str" => Ok(Primitive::String),
			"char" => Ok(Primitive::Char),
			_ => Err(Parse::InvalidPrimitive(prim.to_string())),
		}
	}

	pub(super) fn map_rustdoc_term(&self, term: rustdoc_types::Term) -> Term {
		match term {
			rustdoc_types::Term::Type(typer) => Term::Equality(Box::new(
				self.type_(&typer).unwrap_or(ir::ty::Type::Infer),
			)),
			rustdoc_types::Term::Constant(_constant) => Term::Bound(vec![]),
		}
	}
}
