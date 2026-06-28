use super::parse::{ParseContext, ParseState};
use super::{ParseError, Result};
use ir::entry::NudoxPath;
use ir::function::Function;
use ir::kind::Entry;
use ir::protocols::{AssociatedType, AssociatedTypeImpl, TraitAttribute, TraitConstant, TraitDef, TraitImpl};
use ir::record::{Field, FieldAttributes, FieldKey, KnownField, Record, SumVariant};
use ir::ty::Type;
use rustdoc_types::{Id, ItemEnum};

impl ParseContext {
	pub(super) fn collect_implemented_protocols(&self, impl_ids: &[Id]) -> Vec<NudoxPath> {
		let mut implemented_protocols = Vec::new();
		let mut seen = HashSet::new();

		for impl_id in impl_ids {
			let Some(impl_item) = self.krate.index.get(impl_id) else {
				continue;
			};

			let ItemEnum::Impl(imp) = &impl_item.inner else {
				continue;
			};

			if imp.is_negative {
				continue;
			}

			let Some(trait_path) = &imp.trait_ else {
				continue;
			};

			let nudox_path = self.nudox_path_for_rustdoc_path(trait_path);
			if seen.insert(nudox_path.clone()) {
				implemented_protocols.push(nudox_path);
			}
		}

		implemented_protocols
	}

	pub(super) fn collect_record_methods(&self, impl_ids: &[Id]) -> Result<Vec<Function>> {
		let mut methods = Vec::new();

		for impl_id in impl_ids {
			let Some(impl_item) = self.krate.index.get(impl_id) else {
				continue;
			};

			let ItemEnum::Impl(imp) = &impl_item.inner else {
				continue;
			};

			if imp.is_negative {
				continue;
			}

			let implemented_protocol =
				imp.trait_.as_ref().map(|path| self.nudox_path_for_rustdoc_path(path));

			for assoc_item_id in &imp.items {
				let Some(assoc_item) = self.krate.index.get(assoc_item_id) else {
					continue;
				};

				let ItemEnum::Function(function_item) = &assoc_item.inner else {
					continue;
				};

				let mut function = self.parse_function(assoc_item_id, function_item)?;
				function.implemented_protocols = implemented_protocol.clone().map(|path| vec![path]);
				methods.push(function);
			}
		}

		Ok(methods)
	}

	pub(super) fn parse_item_kind(&self, state: &mut ParseState, id: &Id, inner: &ItemEnum) -> Result<Entry> {
		match inner {
			ItemEnum::Module(_) => {
				Ok(Entry::Module(ir::kind::Symbol::placeholder(ir::module::Module { members: None })))
			}

			ItemEnum::Struct(s) => {
				let record = self.parse_struct(id, s)?;
				Ok(Entry::RecordType(ir::kind::Symbol::placeholder(record)))
			}

			ItemEnum::Enum(e) => {
				let variants = self.parse_enum_variants(e)?;
				Ok(Entry::SumType(ir::kind::Symbol::placeholder(variants)))
			}

			ItemEnum::Function(f) => {
				let function = self.parse_function(id, f)?;
				Ok(Entry::Function(ir::kind::Symbol::placeholder(function)))
			}

			ItemEnum::Trait(t) => {
				let trait_def = self.parse_trait(state, id, t)?;
				Ok(Entry::TraitDef(ir::kind::Symbol::placeholder(trait_def)))
			}

			ItemEnum::Impl(i) => {
				let trait_impl = self.parse_impl(state, id, i)?;
				Ok(Entry::TraitImpl(ir::kind::Symbol::placeholder(trait_impl)))
			}

			ItemEnum::TypeAlias(alias) => {
				Ok(Entry::TypeAlias(ir::kind::Symbol::placeholder(self.parse_type(&alias.type_)?)))
			}

			ItemEnum::Constant { .. } => Ok(Entry::Constant(ir::kind::Symbol::placeholder(()))),

			ItemEnum::Static(_) => Ok(Entry::Variable(ir::kind::Symbol::placeholder(()))),

			ItemEnum::Macro(_) => Ok(Entry::Macro(ir::kind::Symbol::placeholder(()))),

			ItemEnum::ProcMacro(_) => Ok(Entry::Macro(ir::kind::Symbol::placeholder(()))),

			ItemEnum::Union(u) => {
				let types = self.parse_union_fields(u)?;
				Ok(Entry::UnionType(ir::kind::Symbol::placeholder(types)))
			}

			ItemEnum::StructField(_)
			| ItemEnum::AssocType { .. }
			| ItemEnum::AssocConst { .. }
			| ItemEnum::Variant(_)
			| ItemEnum::ExternCrate { .. } => Err(ParseError::UnsupportedItemType(format!("{inner:?}"))),

			_ => Err(ParseError::UnsupportedItemType(format!("{inner:?}"))),
		}
	}

	pub(super) fn parse_struct(&self, id: &Id, s: &rustdoc_types::Struct) -> Result<Record> {
		let item = self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;
		let generics =
			s.generics.params.is_empty().then(|| self.parse_generic_params(&s.generics)).flatten();

		let fields = match &s.kind {
			rustdoc_types::StructKind::Unit => vec![],
			rustdoc_types::StructKind::Tuple(field_ids) => self.parse_tuple_fields(field_ids)?,
			rustdoc_types::StructKind::Plain { fields: field_ids, .. } => {
				self.parse_named_fields(field_ids)?
			}
		};

		let _visibility = self.parse_visibility(&item.visibility);

		Ok(Record {
			name: item.name.clone(),
			generics,
			fields,
			call_signatures: None,
			constructors: None,
			methods: None,
			index_signatures: None,
			super_types: None,
			implemented_protocols: None,
			members: None,
		})
	}

	pub(super) fn parse_tuple_fields(&self, field_ids: &[Option<Id>]) -> Result<Vec<Field>> {
		field_ids
			.iter()
			.enumerate()
			.filter_map(|(idx, opt_id)| {
				opt_id.as_ref().map(|id| {
					let item = self.krate.index.get(id)?;
					if let ItemEnum::StructField(ty) = &item.inner {
						let parsed_ty = self.parse_type(&ty).ok()?;

						Some(Field::Known(KnownField {
							key:           FieldKey::Index(idx),
							r#type:        Some(Box::new(parsed_ty)),
							default_value: None,
							attributes:    FieldAttributes {
								decorators:  vec![],
								is_mutable:  false,
								is_optional: false,
								is_static:   false,
							},
							visibility:    Some(self.parse_visibility(&item.visibility)),
							documentation: item.docs.clone(),
						}))
					} else {
						None
					}
				})
			})
			.collect::<Option<Vec<_>>>()
			.ok_or_else(|| ParseError::MissingField {
				field:   "tuple fields".to_string(),
				context: "struct".to_string(),
			})
	}

	pub(super) fn parse_named_fields(&self, field_ids: &[Id]) -> Result<Vec<Field>> {
		field_ids
			.iter()
			.map(|id| {
				let item = self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;

				if let ItemEnum::StructField(ty) = &item.inner {
					let parsed_ty = self.parse_type(&ty)?;

					Ok(Field::Known(KnownField {
						key:           FieldKey::Ident(item.name.clone().unwrap_or_default()),
						r#type:        Some(Box::new(parsed_ty)),
						default_value: None,
						attributes:    FieldAttributes {
							decorators:  vec![],
							is_mutable:  false,
							is_optional: false,
							is_static:   false,
						},
						visibility:    Some(self.parse_visibility(&item.visibility)),
						documentation: item.docs.clone(),
					}))
				} else {
					Err(ParseError::InvalidItemKind {
						id:       id.0.to_string(),
						expected: "StructField".to_string(),
						actual:   format!("{:?}", item.inner),
					})
				}
			})
			.collect()
	}

	pub(super) fn parse_enum_variants(&self, e: &rustdoc_types::Enum) -> Result<Vec<SumVariant>> {
		e.variants
			.iter()
			.map(|variant_id| {
				let item =
					self.krate.index.get(variant_id).ok_or(ParseError::ItemNotFound(variant_id.0))?;

				let name = item.name.clone().unwrap_or_default();

				if let ItemEnum::Variant(v) = &item.inner {
					let data = match &v.kind {
						rustdoc_types::VariantKind::Plain => None,
						rustdoc_types::VariantKind::Tuple(fields) => {
							let types: Result<Vec<Type>> = fields
								.iter()
								.filter_map(|opt_id| opt_id.as_ref())
								.map(|id| {
									let field_item =
										self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;
									if let ItemEnum::StructField(ty) = &field_item.inner {
										self.parse_type(&ty)
									} else {
										Err(ParseError::InvalidItemKind {
											id:       id.0.to_string(),
											expected: "StructField".to_string(),
											actual:   format!("{:?}", field_item.inner),
										})
									}
								})
								.collect();
							Some(ir::record::SumField::Tuple(types?))
						}
						rustdoc_types::VariantKind::Struct { fields, .. } => {
							let types: Result<Vec<Field>> = fields
								.iter()
								.map(|id| {
									let field_item =
										self.krate.index.get(id).ok_or(ParseError::ItemNotFound(id.0))?;
									if let ItemEnum::StructField(ty) = &field_item.inner {
										Ok(Field::Known(ir::record::KnownField {
											key:           ir::record::FieldKey::Ident(
												field_item.name.clone().unwrap_or_default(),
											),
											r#type:        Some(Box::new(self.parse_type(&ty)?)),
											default_value: None,
											attributes:    ir::record::FieldAttributes {
												is_mutable:  false,
												is_optional: false,
												decorators:  vec![],
												is_static:   false,
											},
											visibility:    Some(self.parse_visibility(&field_item.visibility)),
											documentation: field_item.docs.clone(),
										}))
									} else {
										Err(ParseError::InvalidItemKind {
											id:       id.0.to_string(),
											expected: "StructField".to_string(),
											actual:   format!("{:?}", field_item.inner),
										})
									}
								})
								.collect();
							Some(ir::record::SumField::StructLike(types?))
						}
					};

					Ok(SumVariant { name, data, documentation: item.docs.clone() })
				} else {
					Err(ParseError::InvalidItemKind {
						id:       variant_id.0.to_string(),
						expected: "Variant".to_string(),
						actual:   format!("{:?}", item.inner),
					})
				}
			})
			.collect()
	}

	pub(super) fn parse_trait(
		&self,
		_state: &mut ParseState,
		_id: &Id,
		t: &rustdoc_types::Trait,
	) -> Result<TraitDef> {
		let generics =
			if t.generics.params.is_empty() { None } else { self.parse_generic_params(&t.generics) };

		let super_traits =
			if t.bounds.is_empty() { None } else { Some(self.parse_trait_bounds(&t.bounds)?) };

		let mut required_methods = Vec::new();
		let mut provided_methods = Vec::new();
		let mut associated_types = Vec::new();
		let mut required_constants = Vec::new();

		for item_id in &t.items {
			let trait_item = self.krate.index.get(item_id).ok_or(ParseError::ItemNotFound(item_id.0))?;

			match &trait_item.inner {
				ItemEnum::Function(f) => {
					let method = self.parse_trait_method(item_id, &f)?;
					if f.has_body {
						provided_methods.push(method);
					} else {
						required_methods.push(method);
					}
				}
				ItemEnum::AssocType { generics: _, bounds, type_ } => {
					let assoc_type = AssociatedType {
						name:         trait_item.name.clone().unwrap_or_default(),
						bounds:       if bounds.is_empty() {
							None
						} else {
							Some(self.parse_generic_bounds(&bounds)?)
						},
						default_type: if let Some(ty) = type_ { Some(self.parse_type(&ty)?) } else { None },
					};
					associated_types.push(assoc_type);
				}
				ItemEnum::AssocConst { type_, value } => {
					let constant = TraitConstant {
						name:          trait_item.name.clone().unwrap_or_default(),
						r#type:        Box::new(self.parse_type(&type_)?),
						default_value: value.as_ref().map(|d| ConstExpr::Var(d.clone())),
					};
					required_constants.push(constant);
				}
				_ => {}
			}
		}

		let attributes = if t.is_auto {
			Some(vec![TraitAttribute::Auto])
		} else if t.is_unsafe {
			Some(vec![TraitAttribute::Unsafe])
		} else {
			None
		};

		Ok(TraitDef {
			generics,
			super_traits,
			associated_types: if associated_types.is_empty() { None } else { Some(associated_types) },
			properties: None,
			required_methods: if required_methods.is_empty() { None } else { Some(required_methods) },
			provided_methods: if provided_methods.is_empty() { None } else { Some(provided_methods) },
			required_constants: if required_constants.is_empty() {
				None
			} else {
				Some(required_constants)
			},
			attributes,
			members: None,
		})
	}

	pub(super) fn parse_impl(
		&self,
		_state: &mut ParseState,
		_id: &Id,
		i: &rustdoc_types::Impl,
	) -> Result<TraitImpl> {
		let tr =
			i.trait_.as_ref().map(|path| self.parse_path_to_trait_ref(path)).transpose()?.ok_or_else(
				|| ParseError::ImplBlockParsing {
					reason: "Inherent impl blocks not supported as TraitImpl".to_string(),
				},
			)?;

		let for_type = Box::new(self.parse_type(&i.for_)?);

		let generics =
			if i.generics.params.is_empty() { None } else { self.parse_generic_params(&i.generics) };

		let where_constraints = if i.generics.where_predicates.is_empty() {
			None
		} else {
			Some(self.parse_where_predicates(&i.generics.where_predicates)?)
		};

		let mut methods = Vec::new();
		let mut associated_types = Vec::new();
		let mut associated_constants = Vec::new();

		for item_id in &i.items {
			let impl_item = self.krate.index.get(item_id).ok_or(ParseError::ItemNotFound(item_id.0))?;

			match &impl_item.inner {
				ItemEnum::Function(f) => {
					let function = self.parse_function(item_id, &f)?;
					methods.push(function);
				}
				ItemEnum::AssocType { type_: Some(ty), .. } => {
					let assoc_type_impl = AssociatedTypeImpl {
						name:   impl_item.name.clone().unwrap_or_default(),
						r#type: Box::new(self.parse_type(&ty)?),
					};
					associated_types.push(assoc_type_impl);
				}
				ItemEnum::AssocType { type_: None, .. } => {}
				ItemEnum::AssocConst { type_, value } => {
					let constant = TraitConstant {
						name:          impl_item.name.clone().unwrap_or_default(),
						r#type:        Box::new(self.parse_type(&type_)?),
						default_value: value.as_ref().map(|d| ConstExpr::Var(d.clone())),
					};
					associated_constants.push(constant);
				}
				_ => {}
			}
		}

		Ok(TraitImpl {
			tr,
			for_type,
			generics,
			where_constraints,
			methods: if methods.is_empty() { None } else { Some(methods) },
			associated_types: if associated_types.is_empty() { None } else { Some(associated_types) },
			associated_constants: if associated_constants.is_empty() {
				None
			} else {
				Some(associated_constants)
			},
			is_negative: i.is_negative,
			is_blanket: i.blanket_impl.is_some(),
			is_unsafe: i.is_unsafe,
			members: None,
		})
	}
}
