use ir::{function::{Attribute as FnAttribute, Function}, parameter::{LiteralParameter, Parameter}, pipeline::{output_parameters_from_type, parameter_link_key}, protocols::{ReceiverKind, TraitMethod}, ty::FunctionPointer};
use rustc_hash::FxHashMap as HashMap;
use rustdoc_types::Id;

use super::{Result, context::ParseContext, error::Parse};
use crate::parse::{ReceiverExtract, VisibilityMap, empty_to_none};

impl ReceiverExtract for ParseContext {
	type Param = (String, rustdoc_types::Type);

	fn receiver(&self, first_param: Option<&(String, rustdoc_types::Type)>) -> Option<ReceiverKind> {
		let Some((name, ty)) = first_param else {
			return Some(ReceiverKind::Static);
		};
		if name != "self" {
			return Some(ReceiverKind::Static);
		}
		match ty {
			rustdoc_types::Type::BorrowedRef { is_mutable, .. } => {
				if *is_mutable {
					Some(ReceiverKind::MutRef)
				} else {
					Some(ReceiverKind::SharedRef)
				}
			}
			rustdoc_types::Type::Generic(n) if n == "Self" => Some(ReceiverKind::Owned),
			rustdoc_types::Type::ResolvedPath(path) if path.path == "Self" => Some(ReceiverKind::Owned),
			_ => Some(ReceiverKind::Arbitrary),
		}
	}
}

impl ParseContext {
	pub(super) fn function(&self, id: &Id, f: &rustdoc_types::Function) -> Result<Function> {
		let item = self.krate.index.get(id).ok_or(Parse::ItemNotFound(id.0))?;
		let vis = item.visibility.clone();
		let (receiver, input_parameters) = self.function_inputs(&f.sig.inputs)?;

		let output_parameters = f
			.sig
			.output
			.as_ref()
			.map(|output_ty| self.type_(output_ty))
			.transpose()?
			.and_then(output_parameters_from_type);

		let attributes = self.function_attributes(f);

		let generics =
			if f.generics.params.is_empty() { None } else { self.generic_params(&f.generics) };

		let _visibility = Some(self.visibility(&vis));

		let type_links = {
			let mut links = HashMap::default();

			if let Some(ref params) = input_parameters {
				for (idx, param) in params.iter().enumerate() {
					if let Parameter::Literal(l) = param
						&& let Some(ref ty) = l.r#type
						&& let Some(entry_id) = self.resolve_type_to_entry_id(ty)
					{
						links.insert(parameter_link_key("in", idx, params.len(), &l.name), entry_id);
					}
				}
			}

			if let Some(ref params) = output_parameters {
				for (idx, param) in params.iter().enumerate() {
					if let Parameter::Literal(l) = param
						&& let Some(ref ty) = l.r#type
						&& let Some(entry_id) = self.resolve_type_to_entry_id(ty)
					{
						links.insert(parameter_link_key("out", idx, params.len(), &l.name), entry_id);
					}
				}
			}

			if !links.is_empty() { Some(links) } else { None }
		};

		Ok(Function {
			input_parameters,
			output_parameters,
			attributes,
			generics,
			receiver: match receiver {
				Some(ReceiverKind::Static) | None => None,
				other => other,
			},
			overloads: None,
			implemented: true,
			type_links,
			implemented_protocols: None,
			members: None,
			body: None,
		})
	}

	pub(super) fn function_inputs(
		&self,
		inputs: &[(String, rustdoc_types::Type)],
	) -> Result<(Option<ReceiverKind>, Option<Vec<Parameter>>)> {
		let receiver = self.receiver(inputs.first());
		let params = inputs
			.iter()
			.enumerate()
			.filter(|(idx, (name, _))| !(*idx == 0 && name == "self"))
			.map(|(_, (name, ty))| {
				let parsed_ty = self.type_(&ty)?;
				Ok(Parameter::Literal(LiteralParameter {
					name:          name.clone(),
					r#type:        Some(parsed_ty),
					attributes:    None,
					default_value: None,
					description:   None,
				}))
			})
			.collect::<Result<Vec<_>>>()?;
		Ok((receiver, empty_to_none(params)))
	}

	pub(super) fn function_attributes(
		&self,
		f: &rustdoc_types::Function,
	) -> Option<Vec<FnAttribute>> {
		let mut attrs = Vec::new();

		let header = &f.header;
		if header.is_const {
			attrs.push(FnAttribute::Const);
		}
		if header.is_unsafe {
			attrs.push(FnAttribute::Unsafe);
		}
		if header.is_async {
			attrs.push(FnAttribute::Async);
		}

		empty_to_none(attrs)
	}

	pub(super) fn trait_method(&self, id: &Id, f: &rustdoc_types::Function) -> Result<TraitMethod> {
		let item = self.krate.index.get(id).ok_or(Parse::ItemNotFound(id.0))?;

		let (receiver, parameters) = self.function_inputs(&f.sig.inputs)?;

		let return_type = f.sig.output.as_ref().map(|ty| self.type_(&ty).map(Box::new)).transpose()?;

		let generics =
			if f.generics.params.is_empty() { None } else { self.generic_params(&f.generics) };

		let attributes = self.function_attributes(f);

		Ok(TraitMethod {
			name: item.name.clone().unwrap_or_default(),
			parameters,
			return_type,
			generics,
			attributes,
			documentation: item.docs.clone(),
			receiver,
			has_default_implementation: f.has_body,
		})
	}

	pub(super) fn function_pointer(
		&self,
		fp: &rustdoc_types::FunctionPointer,
	) -> Result<FunctionPointer> {
		let (_, inputs) = self.function_inputs(&fp.sig.inputs)?;

		let outputs = fp.sig.output.as_ref().map(|ty| self.type_(&ty)).transpose()?;

		let mut attributes = Vec::new();
		if fp.header.is_const {
			attributes.push(FnAttribute::Const);
		}
		if fp.header.is_unsafe {
			attributes.push(FnAttribute::Unsafe);
		}
		if fp.header.is_async {
			attributes.push(FnAttribute::Async);
		}

		Ok(FunctionPointer {
			inputs,
			outputs: outputs.and_then(output_parameters_from_type),
			attributes: if attributes.is_empty() { None } else { Some(attributes) },
		})
	}
}
