use deno_doc::{params::{ParamDef, ParamPatternDef}, ts_type::{IndexSignatureDef, LiteralDef, LiteralDefKind, ThisOrIdent, TsTypeDef, TsTypeDefKind}, ts_type_param::TsTypeParamDef};
use ir::{generics::{GenericArg, *}, parameter::{Parameter, ParameterAttribute, TypeParam, TypeParamOrigin}, pipeline::output_parameters_from_type, primitives::{Primitive, Width}, record::*, ty::{ConditionalType, FunctionPointer, MappedType, PredicateSubject, QualifiedPath, Type, TypeOperator, TypePredicate, TypeReference}};

use super::{PropertyFieldMetadata, Result, TsDocParser, error::Parse, extract_doc, modifier_prefix};

impl TsDocParser {
	pub(super) fn property_field(
		&mut self,
		name: &str,
		ts_type: Option<&TsTypeDef>,
		metadata: PropertyFieldMetadata<'_>,
	) -> Result<Field> {
		let ty = ts_type.map(|t| self.ts_type(t).map(Box::new)).transpose()?;
		Ok(Field::Known(ir::record::KnownField {
			key:           ir::record::FieldKey::Ident(name.to_string()),
			r#type:        ty,
			default_value: None,
			attributes:    ir::record::FieldAttributes {
				is_mutable:  !metadata.readonly,
				is_optional: metadata.optional,
				decorators:  metadata.decorators.to_vec(),
				is_static:   metadata.is_static,
			},
			visibility:    metadata.visibility,
			documentation: metadata.documentation,
		}))
	}

	pub(super) fn index_signature(&mut self, sig: &IndexSignatureDef) -> Result<IndexSignature> {
		let key_param = sig.params.first().ok_or_else(|| Parse::TypeResolution {
			type_name: "index_signature".to_string(),
			reason:    "missing key parameter".to_string(),
		})?;
		let key_type = key_param.ts_type.as_ref().ok_or_else(|| Parse::TypeResolution {
			type_name: "index_signature".to_string(),
			reason:    "missing key type".to_string(),
		})?;
		let value_type = sig.ts_type.as_ref().ok_or_else(|| Parse::TypeResolution {
			type_name: "index_signature".to_string(),
			reason:    "missing value type".to_string(),
		})?;
		Ok(IndexSignature {
			key_type:   Box::new(self.ts_type(key_type)?),
			value_type: Box::new(self.ts_type(value_type)?),
		})
	}

	pub(super) fn type_literal_record(
		&mut self,
		name: Option<String>,
		literal: &deno_doc::ts_type::TsTypeLiteralDef,
	) -> Result<Record> {
		let fields = literal
			.properties
			.iter()
			.map(|prop| {
				self.property_field(&prop.name, prop.ts_type.as_ref(), PropertyFieldMetadata {
					optional:      prop.optional,
					readonly:      prop.readonly,
					is_static:     false,
					visibility:    None,
					documentation: extract_doc(&prop.js_doc),
					decorators:    &[],
				})
			})
			.collect::<Result<Vec<_>>>()?;
		let index_signatures = literal
			.index_signatures
			.iter()
			.map(|sig| self.index_signature(sig))
			.collect::<Result<Vec<_>>>()?;
		let methods = literal
			.methods
			.iter()
			.map(|method| self.method_signature_function(method))
			.collect::<Result<Vec<_>>>()?;
		let constructors = literal
			.constructors
			.iter()
			.map(|ctor| self.constructor_signature_from_type_literal(ctor))
			.collect::<Result<Vec<_>>>()?;
		let call_signatures = literal
			.call_signatures
			.iter()
			.map(|sig| self.call_signature_function(sig))
			.collect::<Result<Vec<_>>>()?;

		Ok(Record {
			name,
			generics: None,
			fields,
			call_signatures: if call_signatures.is_empty() { None } else { Some(call_signatures) },
			constructors: if constructors.is_empty() { None } else { Some(constructors) },
			methods: if methods.is_empty() { None } else { Some(methods) },
			index_signatures: if index_signatures.is_empty() { None } else { Some(index_signatures) },
			super_types: None,
			members: None,
			implemented_protocols: None,
		})
	}

	pub(super) fn type_params(&mut self, params: &[TsTypeParamDef]) -> Result<Option<Generics>> {
		if params.is_empty() {
			return Ok(None);
		}

		let mut type_params = Vec::new();
		let mut constraints = Vec::new();

		for p in params {
			let default_type =
				p.default.as_ref().map(|t| self.ts_type(t)).transpose()?.map(|ty| self.type_to_expr(&ty));

			type_params.push(Parameter::Type(TypeParam {
				name: Some(p.name.clone()),
				kind: ir::generics::Kind::Type,
				variance: Variance::Invariant,
				default_type,
				params: None,
				origin: TypeParamOrigin::Free,
			}));

			if let Some(constraint) = &p.constraint {
				let trait_ref = self.ts_type_to_trait_ref(constraint)?;
				constraints.push(Constraint::TraitBound { param: p.name.clone(), trait_ref });
			}
		}

		Ok(Some(Generics { params: type_params, constraints }))
	}

	pub(super) fn type_to_expr(&self, ty: &Type) -> TypeExpr {
		match ty {
			Type::TypeReference(tr) => {
				let args = tr
					.generic_args
					.as_ref()
					.map(|args| {
						args
							.iter()
							.filter_map(|arg| {
								if let GenericArg::Type(t) = arg { Some(self.type_to_expr(t)) } else { None }
							})
							.collect()
					})
					.unwrap_or_default();
				TypeExpr { name: tr.identifier.clone(), args }
			}
			Type::SelfType => TypeExpr { name: "Self".to_string(), args: vec![] },
			_ => TypeExpr { name: format!("{:?}", ty), args: vec![] },
		}
	}

	pub(super) fn ts_type_to_trait_ref(&mut self, ty: &TsTypeDef) -> Result<TraitRef> {
		match &ty.kind {
			TsTypeDefKind::TypeRef(type_ref) => {
				let args = type_ref
					.type_params
					.as_ref()
					.map(|tp| {
						tp.iter()
							.map(|t| self.ts_type(t).map(|t| self.type_to_expr(&t)))
							.collect::<Result<Vec<_>>>()
					})
					.transpose()?
					.unwrap_or_default();
				Ok(TraitRef { name: type_ref.type_name.clone(), args })
			}
			_ => {
				let parsed = self.ts_type(ty)?;
				let expr = self.type_to_expr(&parsed);
				Ok(TraitRef { name: expr.name, args: expr.args })
			}
		}
	}

	pub(super) fn ts_type(&mut self, ts_type: &TsTypeDef) -> Result<Type> {
		match &ts_type.kind {
			TsTypeDefKind::Keyword(value) => Ok(self.keyword_type(value)),

			TsTypeDefKind::Literal(value) => Ok(self.literal_type(value)),

			TsTypeDefKind::TypeRef(value) => {
				let generic_args = value
					.type_params
					.as_ref()
					.map(|tp| {
						tp.iter().map(|t| self.ts_type(t).map(GenericArg::Type)).collect::<Result<Vec<_>>>()
					})
					.transpose()?
					.filter(|v| !v.is_empty());

				Ok(Type::TypeReference(TypeReference { identifier: value.type_name.clone(), generic_args }))
			}

			TsTypeDefKind::Union(value) => {
				let types: Result<Vec<Type>> = value.iter().map(|t| self.ts_type(t)).collect();
				Ok(Type::Union(types?))
			}

			TsTypeDefKind::Intersection(value) => {
				let types: Result<Vec<Type>> = value.iter().map(|t| self.ts_type(t)).collect();
				Ok(Type::Intersection(types?))
			}

			TsTypeDefKind::Array(value) => Ok(Type::Slice(Box::new(self.ts_type(value)?))),

			TsTypeDefKind::Tuple(value) => {
				let types: Result<Vec<Type>> = value.iter().map(|t| self.ts_type(t)).collect();
				Ok(Type::Tuple(types?))
			}

			TsTypeDefKind::FnOrConstructor(value) => {
				let inputs: Result<Vec<Parameter>> =
					value.params.iter().map(|p| self.param_type_only(p)).collect();

				let outputs = output_parameters_from_type(self.ts_type(&value.ts_type)?);

				Ok(Type::FunctionPointer(FunctionPointer {
					inputs: Some(inputs?),
					outputs,
					attributes: None,
				}))
			}

			TsTypeDefKind::Parenthesized(value) => self.ts_type(value),

			TsTypeDefKind::Rest(value) => Ok(Type::Variadic(Box::new(self.ts_type(value)?))),

			TsTypeDefKind::Optional(value) => self.ts_type(value),

			TsTypeDefKind::TypeQuery(value) => {
				Ok(Type::TypeReference(TypeReference { identifier: value.clone(), generic_args: None }))
			}

			TsTypeDefKind::This => Ok(Type::SelfType),

			TsTypeDefKind::Conditional(value) => Ok(Type::Conditional(ConditionalType {
				check_type:   Box::new(self.ts_type(&value.check_type)?),
				extends_type: Box::new(self.ts_type(&value.extends_type)?),
				true_type:    Box::new(self.ts_type(&value.true_type)?),
				false_type:   Box::new(self.ts_type(&value.false_type)?),
			})),

			TsTypeDefKind::Infer(_) => Ok(Type::Infer),

			TsTypeDefKind::IndexedAccess(value) => {
				let self_type = Box::new(self.ts_type(&value.obj_type)?);
				let index_repr = format!("{:?}", self.ts_type(&value.index_type)?);
				Ok(Type::QualifiedPath(QualifiedPath {
					name: index_repr,
					generic_arguments: None,
					self_type,
					tr: None,
				}))
			}

			TsTypeDefKind::TypeOperator(value) => Ok(Type::TypeOperator(TypeOperator {
				operator: value.operator.clone(),
				r#type:   Box::new(self.ts_type(&value.ts_type)?),
			})),

			TsTypeDefKind::TypeLiteral(value) => {
				Ok(Type::RecordLiteral(Box::new(self.type_literal_record(None, value)?)))
			}

			TsTypeDefKind::Mapped(value) => Ok(Type::Mapped(MappedType {
				readonly:    modifier_prefix(value.readonly),
				optional:    modifier_prefix(value.optional),
				parameter:   value.type_param.name.clone(),
				source_type: Box::new(self.ts_type(value.type_param.constraint.as_ref().ok_or_else(
					|| Parse::TypeResolution {
						type_name: "mapped_type".to_string(),
						reason:    "missing mapped type source constraint".to_string(),
					},
				)?)?),
				name_type:   value
					.name_type
					.as_ref()
					.map(|ty| self.ts_type(ty).map(Box::new))
					.transpose()?,
				value_type:  value.ts_type.as_ref().map(|ty| self.ts_type(ty).map(Box::new)).transpose()?,
			})),

			TsTypeDefKind::ImportType(value) => {
				let name = value.qualifier.clone().unwrap_or_else(|| value.specifier.clone());
				let generic_args = value
					.type_params
					.as_ref()
					.map(|tp| {
						tp.iter().map(|t| self.ts_type(t).map(GenericArg::Type)).collect::<Result<Vec<_>>>()
					})
					.transpose()?
					.filter(|v| !v.is_empty());

				Ok(Type::TypeReference(TypeReference { identifier: name, generic_args }))
			}

			TsTypeDefKind::TypePredicate(value) => Ok(Type::Predicate(TypePredicate {
				asserts: value.asserts,
				subject: match &value.param {
					ThisOrIdent::This => PredicateSubject::This,
					ThisOrIdent::Identifier { name } => PredicateSubject::Identifier(name.clone()),
				},
				r#type:  value.r#type.as_ref().map(|ty| self.ts_type(ty).map(Box::new)).transpose()?,
			})),

			TsTypeDefKind::Unsupported => Ok(Type::Infer),
		}
	}

	pub(super) fn keyword_type(&self, keyword: &str) -> Type {
		match keyword {
			"string" => Type::Primitive(Primitive::String),
			"number" => Type::Primitive(Primitive::Float(Width::W64)),
			"boolean" => Type::Primitive(Primitive::Bool),
			"bigint" => Type::Primitive(Primitive::Int(Width::W128)),
			"null" | "undefined" | "void" => Type::Tuple(vec![]),
			"never" => Type::Never,
			"any" | "unknown" => Type::Any,
			"this" => Type::SelfType,
			"object" => Type::TypeReference(TypeReference {
				identifier:   "object".to_string(),
				generic_args: None,
			}),
			"symbol" | "unique symbol" => Type::TypeReference(TypeReference {
				identifier:   "Symbol".to_string(),
				generic_args: None,
			}),
			other => {
				Type::TypeReference(TypeReference { identifier: other.to_string(), generic_args: None })
			}
		}
	}

	pub(super) fn literal_type(&self, lit: &LiteralDef) -> Type {
		match lit.kind {
			LiteralDefKind::String => Type::Primitive(Primitive::String),
			LiteralDefKind::Number => Type::Primitive(Primitive::Float(Width::W64)),
			LiteralDefKind::Boolean => Type::Primitive(Primitive::Bool),
			LiteralDefKind::BigInt => Type::Primitive(Primitive::Int(Width::W128)),
			LiteralDefKind::Template => Type::Primitive(Primitive::String),
		}
	}

	pub(super) fn param_type_only(&mut self, param: &ParamDef) -> Result<Parameter> {
		let (name, attrs) = match &param.pattern {
			ParamPatternDef::Identifier { name, optional } => {
				let attrs = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				(name.clone(), attrs)
			}
			ParamPatternDef::Rest { .. } => (param.to_string(), Some(vec![ParameterAttribute::Variadic])),
			ParamPatternDef::Assign { left, .. } => {
				let inner_name = match &left.pattern {
					ParamPatternDef::Identifier { name, .. } => name.clone(),
					_ => left.to_string(),
				};
				(inner_name, None)
			}
			ParamPatternDef::Array { optional, .. } => {
				let attrs = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				(param.to_string(), attrs)
			}
			ParamPatternDef::Object { optional, .. } => {
				let attrs = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				(param.to_string(), attrs)
			}
		};

		let ty = param.ts_type.as_ref().map(|t| self.ts_type(t)).transpose()?;
		Ok(Parameter::Literal(ir::parameter::LiteralParameter {
			name,
			r#type: ty,
			attributes: attrs,
			default_value: None,
			description: None,
		}))
	}

	pub(super) fn resolve_ir_type_to_entry_id(&self, ty: &Type) -> Option<i64> {
		match ty {
			Type::TypeReference(tr) => {
				if let Some(&id) = self.ctx.type_name_to_id.get(tr.identifier.as_str()) {
					return Some(id);
				}
				for (k, &v) in &self.ctx.type_name_to_id {
					if k.ends_with(tr.identifier.as_str()) {
						return Some(v);
					}
				}
				None
			}
			_ => None,
		}
	}
}
