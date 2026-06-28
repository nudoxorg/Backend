use std::collections::HashMap;

use deno_doc::class::ClassConstructorDef;
use deno_doc::ts_type::{CallSignatureDef, IndexSignatureDef, MethodDef};
use deno_doc::{Declaration, DeclarationDef, params::{ParamDef, ParamPatternDef}};
use deno_doc::function::FunctionDef;
use ir::entry::NudoxPath;
use ir::function::{Attribute, Function};
use ir::kind::Entry;
use ir::parameter::{Parameter, ParameterAttribute, ConstExpr};
use ir::protocols::ReceiverKind;
use ir::pipeline::{output_parameters_from_type, parameter_link_key};

use super::{ParseError, Result, TsDocParser, TsParseContext, TsParseState};

impl TsDocParser {
	pub(super) fn parse_function_overloads(
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
				DeclarationDef::Function(function_def) => self.parse_function_def("", function_def, &[]),
				_ => unreachable!(),
			})
			.collect::<Result<Vec<_>>>()?;
		Ok(if overloads.is_empty() { None } else { Some(overloads) })
	}

	pub(super) fn parse_params_with_receiver(
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
			parsed.push(self.parse_param(param)?);
		}

		let parsed = if parsed.is_empty() { None } else { Some(parsed) };
		Ok((receiver, parsed))
	}

	pub(super) fn parse_constructor_signature(&mut self, ctor: &ClassConstructorDef) -> Result<Function> {
		let (_, input_parameters) = self.parse_params_with_receiver(
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
			body: None,
		})
	}

	pub(super) fn parse_constructor_signature_from_type_literal(
		&mut self,
		ctor: &deno_doc::ts_type::ConstructorDef,
	) -> Result<Function> {
		let (_, input_parameters) = self.parse_params_with_receiver(
			&ctor.params.iter().collect::<Vec<_>>(),
			Some(ReceiverKind::Static),
		)?;
		let output_parameters = ctor
			.return_type
			.as_ref()
			.map(|return_type| self.parse_ts_type(return_type))
			.transpose()?
			.and_then(output_parameters_from_type);
		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());
		let generics =
			if ctor.type_params.is_empty() { None } else { self.parse_type_params(&ctor.type_params)? };
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
			body: None,
		})
	}

	pub(super) fn parse_method_signature_function(&mut self, method: &MethodDef) -> Result<Function> {
		let params = method.params.iter().collect::<Vec<_>>();
		let (receiver, input_parameters) = self.parse_params_with_receiver(&params, None)?;
		let output_parameters = method
			.return_type
			.as_ref()
			.map(|return_type| self.parse_ts_type(return_type))
			.transpose()?
			.and_then(output_parameters_from_type);
		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());
		let generics = if method.type_params.is_empty() {
			None
		} else {
			self.parse_type_params(&method.type_params)?
		};
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
			body: None,
		})
	}

	pub(super) fn parse_call_signature_function(&mut self, sig: &CallSignatureDef) -> Result<Function> {
		let params = sig.params.iter().collect::<Vec<_>>();
		let (receiver, input_parameters) = self.parse_params_with_receiver(&params, None)?;
		let output_parameters = sig
			.ts_type
			.as_ref()
			.map(|return_type| self.parse_ts_type(return_type))
			.transpose()?
			.and_then(output_parameters_from_type);
		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());
		let generics =
			if sig.type_params.is_empty() { None } else { self.parse_type_params(&sig.type_params)? };
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
			body: None,
		})
	}
	pub(super) fn parse_function_def(
		&mut self,
		_name: &str,
		func: &FunctionDef,
		_path: &[String],
	) -> Result<Function> {
		let params = func.params.iter().collect::<Vec<_>>();
		let (receiver, input_parameters) = self.parse_params_with_receiver(&params, None)?;
		let output_parameters = func
			.return_type
			.as_ref()
			.map(|rt| self.parse_ts_type(rt))
			.transpose()?
			.and_then(output_parameters_from_type);

		let type_links =
			self.build_type_links_for_params(input_parameters.as_deref(), output_parameters.as_deref());

		let attributes = self.parse_function_attributes(func);

		let generics =
			if func.type_params.is_empty() { None } else { self.parse_type_params(&func.type_params)? };

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
			body: None,
		})
	}

	pub(super) fn parse_function_attributes(&self, func: &FunctionDef) -> Option<Vec<FnAttribute>> {
		let mut attrs = Vec::new();
		if func.is_async {
			attrs.push(FnAttribute::Async);
		}
		if func.is_generator {
			attrs.push(FnAttribute::Generator);
		}
		if attrs.is_empty() { None } else { Some(attrs) }
	}

	pub(super) fn build_type_links_for_params(
		&self,
		inputs: Option<&[Parameter]>,
		outputs: Option<&[Parameter]>,
	) -> Option<HashMap<String, i64>> {
		let mut links = HashMap::new();

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
	pub(super) fn parse_param(&mut self, param: &ParamDef) -> Result<Parameter> {
		match &param.pattern {
			ParamPatternDef::Identifier { name, optional } => {
				let ty = param.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?;
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter::Literal(ir::parameter::LiteralParameter {
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
					.map(|t| self.parse_ts_type(t))
					.transpose()?
					.or(arg.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?);
				Ok(Parameter::Literal(ir::parameter::LiteralParameter {
					name:          param.to_string(),
					r#type:        ty,
					attributes:    Some(vec![ParameterAttribute::Variadic]),
					default_value: None,
					description:   None,
				}))
			}

			ParamPatternDef::Assign { left, right } => {
				let mut inner = self.parse_param(left)?;
				if let Parameter::Literal(ref mut l) = inner {
					if l.r#type.is_none() {
						l.r#type = param.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?;
					}
					l.default_value = Some(ConstExpr::Var(right.clone()));
				}
				Ok(inner)
			}

			ParamPatternDef::Array { optional, .. } => {
				let ty = param.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?;
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter::Literal(ir::parameter::LiteralParameter {
					name: param.to_string(),
					r#type: ty,
					attributes,
					default_value: None,
					description: None,
				}))
			}

			ParamPatternDef::Object { optional, .. } => {
				let ty = param.ts_type.as_ref().map(|t| self.parse_ts_type(t)).transpose()?;
				let attributes = if *optional { Some(vec![ParameterAttribute::Optional]) } else { None };
				Ok(Parameter::Literal(ir::parameter::LiteralParameter {
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
