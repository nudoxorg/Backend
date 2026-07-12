//! Lowering TypeScript functions/methods: overload grouping, `this`-receiver
//! detection, parameters (rest/optional/default/destructured), and attributes.

use deno_doc::{Declaration, DeclarationDef, class::ClassConstructorDef, function::FunctionDef, params::{ParamDef, ParamPatternDef}, ts_type::{CallSignatureDef, MethodDef}};
use ir::{function::{Attribute, Function}, generics::ConstExpr, parameter::{LiteralParameter, Parameter, ParameterAttribute}, pipeline::{output_parameters_from_type, parameter_link_key}, protocols::ReceiverKind};
use rustc_hash::FxHashMap as HashMap;

use deno_doc::class::ClassConstructorDef;
use deno_doc::ts_type::{CallSignatureDef, IndexSignatureDef, MethodDef};
use deno_doc::{Declaration, DeclarationDef, params::{ParamDef, ParamPatternDef}};
use deno_doc::function::FunctionDef;
use ir::entry::NudoxPath;
use ir::function::{Attribute, Function};
use ir::kind::Entry;
use ir::parameter::{Parameter, ParameterAttribute};
use ir::generics::ConstExpr;
use ir::protocols::ReceiverKind;
use ir::pipeline::{output_parameters_from_type, parameter_link_key};

use super::{error::Parse, Result, TsDocParser, TsParseContext, TsParseState};
use super::{is_function_declaration, pick_primary_declaration};
use crate::empty_to_none;

impl TsDocParser {
	pub(super) fn function_overloads(
		&mut self,
		declarations: &[Declaration],
		primary_decl: &Declaration,
	) -> Result<Option<Vec<Function>>> {
		if !is_function_declaration(primary_decl) {
			return Ok(None);
		}
		let overloads = declarations
			.iter()
			.filter(|decl| is_function_declaration(decl) && !std::ptr::eq(*decl, primary_decl))
			.map(|decl| match &decl.def {
				DeclarationDef::Function(function_def) => self.function_def("", function_def, &[]),
				_ => unreachable!(),
			})
			.collect::<Result<Vec<_>>>()?;
		Ok(empty_to_none(overloads))
	}

	pub(super) fn params_with_receiver(
		&mut self,
		params: &[&ParamDef],
		default_receiver: Option<ReceiverKind>,
	) -> Result<(Option<ReceiverKind>, Option<Vec<Parameter>>)> {
		let mut receiver = default_receiver;
		let mut parsed = Vec::new();

		for (idx, param) in params.iter().enumerate() {
			if idx == 0
				&& matches!(&param.pattern, ParamPatternDef::Identifier { name, .. } if name == "this")
			{
				receiver = Some(ReceiverKind::SharedRef);
				continue;
			}
			parsed.push(self.param(param)?);
		}

		let parsed = if parsed.is_empty() { None } else { Some(parsed) };
		Ok((receiver, parsed))
	}

	pub(super) fn constructor_signature(&mut self, ctor: &ClassConstructorDef) -> Result<Function> {
		let (_, input_parameters) = self.params_with_receiver(
			&ctor.params.iter().map(|param| &param.param).collect::<Vec<_>>(),
			Some(ReceiverKind::Static),
		)?;
		let type_links = self.build_type_links_for_params(input_parameters.as_deref(), None);
		Ok(Function {
			input_parameters,
			output_parameters: None,
			type_links,
			attributes: None,
			generics: None,
			receiver: Some(ReceiverKind::Static),
			overloads: None,
			implemented: ctor.has_body,
			members: None,
			implemented_protocols: None,
		})
	}

	pub(super) fn constructor_signature_from_type_literal(
		&mut self,
		ctor: &deno_doc::ts_type::ConstructorDef,
	) -> Result<Function> {
		let (_, input_parameters) = self
			.params_with_receiver(&ctor.params.iter().collect::<Vec<_>>(), Some(ReceiverKind::Static))?;
		let output_parameters = ctor
			.return_type
			.as_ref()
			.map(|return_type| self.ts_type(return_type))
			.transpose()?
			.and_then(output_parameters_from_type);
		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());
		let generics =
			if ctor.type_params.is_empty() { None } else { self.type_params(&ctor.type_params)? };
		Ok(Function {
			input_parameters,
			output_parameters,
			type_links,
			attributes: None,
			generics,
			receiver: Some(ReceiverKind::Static),
			overloads: None,
			implemented: false,
			members: None,
			implemented_protocols: None,
		})
	}

	pub(super) fn method_signature_function(&mut self, method: &MethodDef) -> Result<Function> {
		let params = method.params.iter().collect::<Vec<_>>();
		let (receiver, input_parameters) = self.params_with_receiver(&params, None)?;
		let output_parameters = method
			.return_type
			.as_ref()
			.map(|return_type| self.ts_type(return_type))
			.transpose()?
			.and_then(output_parameters_from_type);
		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());
		let generics =
			if method.type_params.is_empty() { None } else { self.type_params(&method.type_params)? };
		Ok(Function {
			input_parameters,
			output_parameters,
			type_links,
			attributes: None,
			generics,
			receiver,
			overloads: None,
			implemented: false,
			members: None,
			implemented_protocols: None,
		})
	}

	pub(super) fn call_signature_function(&mut self, sig: &CallSignatureDef) -> Result<Function> {
		let params = sig.params.iter().collect::<Vec<_>>();
		let (receiver, input_parameters) = self.params_with_receiver(&params, None)?;
		let output_parameters = sig
			.ts_type
			.as_ref()
			.map(|return_type| self.ts_type(return_type))
			.transpose()?
			.and_then(output_parameters_from_type);
		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());
		let generics =
			if sig.type_params.is_empty() { None } else { self.type_params(&sig.type_params)? };
		Ok(Function {
			input_parameters,
			output_parameters,
			type_links,
			attributes: None,
			generics,
			receiver,
			overloads: None,
			implemented: false,
			members: None,
			implemented_protocols: None,
		})
	}

	pub(super) fn function_def(
		&mut self,
		_name: &str,
		func: &FunctionDef,
		_path: &[String],
	) -> Result<Function> {
		let params = func.params.iter().collect::<Vec<_>>();
		let (receiver, input_parameters) = self.params_with_receiver(&params, None)?;
		let output_parameters = func
			.return_type
			.as_ref()
			.map(|rt| self.ts_type(rt))
			.transpose()?
			.and_then(output_parameters_from_type);

		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());

		let attributes = self.function_attributes(func);

		let generics =
			if func.type_params.is_empty() { None } else { self.type_params(&func.type_params)? };

		Ok(Function {
			input_parameters,
			output_parameters,
			type_links,
			attributes,
			generics,
			receiver,
			overloads: None,
			implemented: func.has_body,
			members: None,
			implemented_protocols: None,
		})
	}

	pub(super) fn function_attributes(&self, func: &FunctionDef) -> Option<Vec<Attribute>> {
		let mut attrs = Vec::new();
		if func.is_async {
			attrs.push(Attribute::Async);
		}
		if func.is_generator {
			attrs.push(Attribute::Generator);
		}
		if attrs.is_empty() { None } else { Some(attrs) }
	}

	pub(super) fn build_type_links_for_params(
		&self,
		inputs: Option<&[Parameter]>,
		outputs: Option<&[Parameter]>,
	) -> Option<HashMap<String, i64>> {
		let mut links = HashMap::default();

		if let Some(params) = inputs {
			for (idx, param) in params.iter().enumerate() {
				if let Parameter::Literal(l) = param
					&& let Some(ref ty) = l.r#type
					&& let Some(entry_id) = self.resolve_ir_type_to_entry_id(ty)
				{
					links.insert(parameter_link_key("in", idx, params.len(), &l.name), entry_id);
				}
			}
		}

		if let Some(params) = outputs {
			for (idx, param) in params.iter().enumerate() {
				if let Parameter::Literal(l) = param
					&& let Some(ref ty) = l.r#type
					&& let Some(entry_id) = self.resolve_ir_type_to_entry_id(ty)
				{
					links.insert(parameter_link_key("out", idx, params.len(), &l.name), entry_id);
				}
			}
		}

		if links.is_empty() { None } else { Some(links) }
	}

	pub(super) fn param(&mut self, param: &ParamDef) -> Result<Parameter> {
		match &param.pattern {
			ParamPatternDef::Identifier { name, optional } => {
				let ty = param.ts_type.as_ref().map(|t| self.ts_type(t)).transpose()?;
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter::Literal(LiteralParameter {
					name: name.clone(),
					r#type: ty,
					attributes,
					default_value: None,
					description: None,
				}))
			}

			ParamPatternDef::Rest { arg } => {
				let ty = param
					.ts_type
					.as_ref()
					.map(|t| self.ts_type(t))
					.transpose()?
					.or(arg.ts_type.as_ref().map(|t| self.ts_type(t)).transpose()?);
				Ok(Parameter::Literal(LiteralParameter {
					name:          param.to_string(),
					r#type:        ty,
					attributes:    Some(vec![ParameterAttribute::Variadic]),
					default_value: None,
					description:   None,
				}))
			}

			ParamPatternDef::Assign { left, right } => {
				let mut inner = self.param(left)?;
				if let Parameter::Literal(ref mut l) = inner {
					if l.r#type.is_none() {
						l.r#type = param.ts_type.as_ref().map(|t| self.ts_type(t)).transpose()?;
					}
					l.default_value = Some(ConstExpr::Var(right.clone()));
				}
				Ok(inner)
			}

			ParamPatternDef::Array { optional, .. } => {
				let ty = param.ts_type.as_ref().map(|t| self.ts_type(t)).transpose()?;
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter::Literal(LiteralParameter {
					name: param.to_string(),
					r#type: ty,
					attributes,
					default_value: None,
					description: None,
				}))
			}

			ParamPatternDef::Object { optional, .. } => {
				let ty = param.ts_type.as_ref().map(|t| self.ts_type(t)).transpose()?;
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter::Literal(LiteralParameter {
					name: param.to_string(),
					r#type: ty,
					attributes,
					default_value: None,
					description: None,
				}))
			}
		}
	}
}
