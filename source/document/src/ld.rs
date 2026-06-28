use ir::{entry::Entry, kind::Visibility};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::schema::{DocCtx, EmitError, URI, UriOps};

/// Represents the abstract class Kind in JsonLD format
/// this is what will be serialized with serde
#[derive(Serialize, Deserialize, Debug)]
pub struct LDKind {
	#[serde(rename = "@id")]
	pub uri:       URI,
	#[serde(flatten)]
	pub inheritor: LDInheritor,
}

impl LDKind {
	pub fn try_new(
		path: &ir::entry::NudoxPath,
		kind: &Entry,
		ctx: &DocCtx,
	) -> Result<Self, EmitError> {
		let uri = ctx.kind_uri(kind, path);
		let inheritor = LDInheritor::try_from(kind)
			.map_err(|e| EmitError::KindConversionFailed { uri: uri.clone(), error: e.to_string() })?;
		Ok(LDKind { uri, inheritor })
	}
}

/// These are concerte implemetnation in schema of what a kind can be
/// think RecordType, Function, Module, etc
// TODO -> implement the LDKindVariant and then add them here.
//         they will need serialization info for field specific information
//         but not things like type, etc. that will live on the LDKind
//         ALSO apply FROM trait (ir::type) into respective LDKind
//         QUESTION - is From Kind to LDInheritor
#[derive(Serialize, Deserialize, Debug)]
#[serde(tag = "@type")] // this is the type variant with internal tagging
// HINT add new type here
pub enum LDInheritor {
	RecordType(LDRecord),
	UnionType(LDUnion),
	TraitDef(LDTraitDef),
	TraitImpl(LDTraitImpl),
	SumType(LDSum),
	Function(LDFunction),
	TypeAlias(LDTypeAlias),
	Module, // dont forget these, else resolves to None type...
	Info,
	InterfaceType,
	Constant,
	Variable,
	Macro,
	PrimitiveType,
	Field,
	Event,
	None,
}

#[derive(Debug, thiserror::Error)]
pub enum LDConversionError {
	#[error("required `name` field is missing")]
	NameMissing,
	#[error("required `visibility` field is missing")]
	VisibilityMissing,
}

/// Handles the conversion from inner Kind type to an LDInheritor type,
/// which represents the concrete implementation of that class
impl TryFrom<&Entry> for LDInheritor {
	type Error = LDConversionError;

	fn try_from(item: &Entry) -> Result<Self, Self::Error> {
		match item {
			// HINT add match case here for next supported item
			Entry::RecordType(s) => {
				// try to get LDRecord, either returns error OR ldrecord wrapped into
				// LDInheritor Enum
				LDRecord::try_from(s).map(LDInheritor::RecordType)
			}
			Entry::UnionType(s) => LDUnion::try_from(s).map(LDInheritor::UnionType),
			Entry::TraitDef(s) => LDTraitDef::try_from(s).map(LDInheritor::TraitDef),
			Entry::TraitImpl(s) => LDTraitImpl::try_from(s).map(LDInheritor::TraitImpl),
			Entry::SumType(s) => LDSum::try_from(s).map(LDInheritor::SumType),
			Entry::Function(s) => LDFunction::try_from(s).map(LDInheritor::Function),
			Entry::TypeAlias(s) => LDTypeAlias::try_from(s).map(LDInheritor::TypeAlias),
			Entry::Module(_) => Ok(LDInheritor::Module),
			Entry::Info(_) => Ok(LDInheritor::Info),
			Entry::Constant(_) => Ok(LDInheritor::Constant),
			Entry::Variable(_) => Ok(LDInheritor::Variable),
			Entry::Macro(_) => Ok(LDInheritor::Macro),
			Entry::PrimitiveType(_) => Ok(LDInheritor::PrimitiveType),
			Entry::Field(_) => Ok(LDInheritor::Field),
			Entry::Event(_) => Ok(LDInheritor::Event),
		}
	}
}

// MARK: - LDRecord

#[derive(Serialize, Deserialize, Debug)]
// HINT prefer to exclude option.. use concrete values and write logic for stuff
// that is missing in the ir types, name is optional, but it is mostly needed
// here, so we require it
pub struct LDRecord {
	pub name:       String,
	pub visibility: Visibility,
	#[serde(skip_serializing_if = "Option::is_none")]
	// skip serialization for non-existent generics
	pub generics: Option<Value>,
	// TODO improve -> Create type Field in the TerminusDB schema and have this that
	// this should be fixed on the LDField kind when implemented, or create better rules for
	// flattening overall do this after main Kind variants are supported
	pub fields:     Vec<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub methods:    Option<Value>,
}

impl TryFrom<&ir::kind::Symbol<ir::record::Record>> for LDRecord {
	// for fields that can fail
	type Error = LDConversionError;

	fn try_from(s: &ir::kind::Symbol<ir::record::Record>) -> Result<Self, Self::Error> {
		let res = LDRecord {
			name:       s.name.clone(),
			visibility: s.visibility.clone(),
			generics:   s.inner.generics.as_ref().map(|g| json!(g)),
			fields:     s.inner.fields.iter().map(|f| json!(f)).collect(),
			methods:    s.inner.methods.as_ref().map(|methods| json!(methods)),
		};
		Ok(res)
	}
}

// MARK: - LDUnion

#[derive(Serialize, Deserialize, Debug)]
pub struct LDUnion {
	pub types: Vec<Value>,
}

impl TryFrom<&ir::kind::Symbol<Vec<ir::ty::Type>>> for LDUnion {
	type Error = LDConversionError;

	fn try_from(s: &ir::kind::Symbol<Vec<ir::ty::Type>>) -> Result<Self, Self::Error> {
		Ok(LDUnion { types: s.inner.iter().map(|t| json!(t)).collect() })
	}
}

// MARK: - LDTraitDef

#[derive(Serialize, Deserialize, Debug)]
pub struct LDTraitDef {
	pub name:               String,
	pub visibility:         Visibility,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub generics:           Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub super_traits:       Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub associated_types:   Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub required_methods:   Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub provided_methods:   Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub required_constants: Option<Value>,
}

impl TryFrom<&ir::kind::Symbol<ir::protocols::TraitDef>> for LDTraitDef {
	type Error = LDConversionError;

	fn try_from(s: &ir::kind::Symbol<ir::protocols::TraitDef>) -> Result<Self, Self::Error> {
		Ok(LDTraitDef {
			name:               s.name.clone(),
			visibility:         s.visibility.clone(),
			generics:           s.inner.generics.as_ref().map(|g| json!(g)),
			super_traits:       s.inner.super_traits.as_ref().map(|s| json!(s)),
			associated_types:   s.inner.associated_types.as_ref().map(|a| json!(a)),
			required_methods:   s.inner.required_methods.as_ref().map(|r| json!(r)),
			provided_methods:   s.inner.provided_methods.as_ref().map(|p| json!(p)),
			required_constants: s.inner.required_constants.as_ref().map(|c| json!(c)),
		})
	}
}

// MARK: - LDTraitImpl

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Polarity { Positive, Negative }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImplScope { Specific, Blanket }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Safety { Safe, Unsafe }

#[derive(Serialize, Deserialize, Debug)]
pub struct LDTraitImpl {
	pub trait_ref:            Value,
	pub for_type:             Value,
	pub visibility:           Visibility,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub generics:             Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub methods:              Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub associated_types:     Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub associated_constants: Option<Value>,
	pub polarity:             Polarity,
	pub scope:                ImplScope,
	pub safety:               Safety,
}

impl TryFrom<&ir::kind::Symbol<ir::protocols::TraitImpl>> for LDTraitImpl {
	type Error = LDConversionError;

	fn try_from(s: &ir::kind::Symbol<ir::protocols::TraitImpl>) -> Result<Self, Self::Error> {
		Ok(LDTraitImpl {
			trait_ref:            json!(s.inner.tr),
			for_type:             json!(s.inner.for_type),
			visibility:           s.visibility.clone(),
			generics:             s.inner.generics.as_ref().map(|g| json!(g)),
			methods:              s.inner.methods.as_ref().map(|m| json!(m)),
			associated_types:     s.inner.associated_types.as_ref().map(|a| json!(a)),
			associated_constants: s.inner.associated_constants.as_ref().map(|c| json!(c)),
			polarity:             if s.inner.is_negative { Polarity::Negative } else { Polarity::Positive },
			scope:                if s.inner.is_blanket  { ImplScope::Blanket  } else { ImplScope::Specific },
			safety:               if s.inner.is_unsafe   { Safety::Unsafe      } else { Safety::Safe },
		})
	}
}

// MARK: - LDSum

#[derive(Serialize, Deserialize, Debug)]
pub struct LDSum {
	pub variants: Vec<Value>,
}

impl TryFrom<&ir::kind::Symbol<Vec<ir::record::SumVariant>>> for LDSum {
	type Error = LDConversionError;

	fn try_from(s: &ir::kind::Symbol<Vec<ir::record::SumVariant>>) -> Result<Self, Self::Error> {
		Ok(LDSum { variants: s.inner.iter().map(|v| json!(v)).collect() })
	}
}

// MARK: - LDFunction

#[derive(Serialize, Deserialize, Debug)]
pub struct LDFunction {
	pub name:              String,
	pub visibility:        Visibility,
	pub implemented:       bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub input_parameters:  Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub output_parameters: Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub attributes:        Option<Value>,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub generics:          Option<Value>,
}

impl TryFrom<&ir::kind::Symbol<ir::function::Function>> for LDFunction {
	type Error = LDConversionError;

	fn try_from(s: &ir::kind::Symbol<ir::function::Function>) -> Result<Self, Self::Error> {
		Ok(LDFunction {
			name:              s.name.clone(),
			visibility:        s.visibility.clone(),
			implemented:       s.inner.implemented,
			input_parameters:  s.inner.input_parameters.as_ref().map(|i| json!(i)),
			output_parameters: s.inner.output_parameters.as_ref().map(|o| json!(o)),
			attributes:        s.inner.attributes.as_ref().map(|a| json!(a)),
			generics:          s.inner.generics.as_ref().map(|g| json!(g)),
		})
	}
}

// MARK: - LDTypeAlias

#[derive(Serialize, Deserialize, Debug)]
pub struct LDTypeAlias {
	pub aliased_type: Value,
}

impl TryFrom<&ir::kind::Symbol<ir::ty::Type>> for LDTypeAlias {
	type Error = LDConversionError;

	fn try_from(s: &ir::kind::Symbol<ir::ty::Type>) -> Result<Self, Self::Error> {
		Ok(LDTypeAlias { aliased_type: json!(s.inner) })
	}
}
