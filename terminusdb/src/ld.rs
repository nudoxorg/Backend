use ir::{kind::{Kind, Visibility}, record::RecordKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::termdb::{DocCtx, URI, UriOps};

/// Represents the abstract class Kind in JsonLD format
/// this is what will be serialized with serde
#[derive(Serialize, Deserialize, Debug)]
pub struct LDKind {
	#[serde(rename = "@id")]
	pub uri:       URI,
	#[serde(rename = "kind_tag")]
	pub kind_tag:  &'static str,
	// add serde_flatten
	#[serde(flatten)]
	pub inheritor: LDInheritor,
}

/// Generates Tags for different fields
pub trait TagGen {
	fn tag(&self) -> &'static str;
}

/// snake_case tags for kind
impl TagGen for Kind {
	fn tag(&self) -> &'static str {
		let tag = match self {
			Kind::Module => "module",
			Kind::Info => "info",
			Kind::InterfaceType => "interface_type",
			Kind::Constant => "constant",
			Kind::Variable => "variable",
			Kind::Macro => "macro",
			Kind::PrimitiveType => "primitive_type",
			Kind::Event => "event",
			Kind::Field => "field",
			Kind::RecordType(_) => "record",
			Kind::UnionType(_) => "union",
			Kind::TraitDef(_) => "trait_def",
			Kind::TraitImpl(_) => "trait_impl",
			Kind::SumType(_) => "sum_type",
			Kind::TypeAlias(_) => "type_alias",
			Kind::Function(_) => "function",
		};
		tag
	}
}

impl LDKind {
	/// Should never fail, but using try_from, so handle errors here
	pub fn new(path: &[String], kind: Kind, ctx: &DocCtx) -> Self {
		LDKind {
			uri:       ctx.kind_uri(&kind, path),
			kind_tag:  kind.tag(),
			inheritor: match LDInheritor::try_from(kind) {
				Ok(i) => i,
				Err(e) => {
					// TODO log error -> this shouldnt happen
					dbg!(e);
					// and just set to empty
					LDInheritor::None
				}
			},
		}
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

#[derive(Debug)]
// HINT add mor eerrors here for specific kinds
pub enum LDConversionError {
	NameMissing,
	VisibilityMissing,
}

/// Handles the conversion from inner Kind type to an LDInheritor type,
/// which represents the concrete implementation of that class
impl TryFrom<Kind> for LDInheritor {
	type Error = LDConversionError;

	fn try_from(item: Kind) -> Result<Self, Self::Error> {
		match item {
			// HINT add match case here for next supported item
			Kind::RecordType(rk) => {
				// try to get LDRecord, either returns error OR ldrecord wrapped into
				// LDInheritor Enum
				LDRecord::try_from(rk).map(|ldrecord| LDInheritor::RecordType(ldrecord))
			}
			Kind::UnionType(ut) => LDUnion::try_from(ut).map(|ldunion| LDInheritor::UnionType(ldunion)),
			Kind::TraitDef(td) => LDTraitDef::try_from(td).map(|ldtrait| LDInheritor::TraitDef(ldtrait)),
			Kind::TraitImpl(ti) => LDTraitImpl::try_from(ti).map(|ldimpl| LDInheritor::TraitImpl(ldimpl)),
			Kind::SumType(st) => LDSum::try_from(st).map(|ldsum| LDInheritor::SumType(ldsum)),
			Kind::Function(func) => {
				LDFunction::try_from(func).map(|ldfunc| LDInheritor::Function(ldfunc))
			}
			Kind::TypeAlias(ta) => {
				LDTypeAlias::try_from(ta).map(|ldalias| LDInheritor::TypeAlias(ldalias))
			}
			Kind::Module => Ok(LDInheritor::Module),
			Kind::Info => Ok(LDInheritor::Info),
			Kind::InterfaceType => Ok(LDInheritor::InterfaceType),
			Kind::Constant => Ok(LDInheritor::Constant),
			Kind::Variable => Ok(LDInheritor::Variable),
			Kind::Macro => Ok(LDInheritor::Macro),
			Kind::PrimitiveType => Ok(LDInheritor::PrimitiveType),
			Kind::Field => Ok(LDInheritor::Field),
			Kind::Event => Ok(LDInheritor::Event),
			_ => Ok(LDInheritor::None),
		}
	}
}

// MARK: - LDRecord

#[derive(Serialize, Deserialize, Debug)]
// HINT prefer to exclude option.. use concrete values and write logic for stuff
// that is missing in the ir types, name is optional, but it is mostly needed
// here, so we require it
pub struct LDRecord {
	pub name:        String,
	pub record_kind: RecordKind,
	pub visibility:  Visibility,
	#[serde(skip_serializing_if = "Option::is_none")]
	// skip serialization for non-existent generics
	pub generics: Option<Value>,
	// TODO improve -> Create type Field in the TerminusDB schema and have this that
	// this should be fixed on the LDField kind when implemented, or create better rules for
	// flattening overall do this after main Kind variants are supported
	pub fields:      Vec<Value>,
}

impl TryFrom<ir::record::Record> for LDRecord {
	// for fields that can fail
	type Error = LDConversionError;

	fn try_from(item: ir::record::Record) -> Result<Self, Self::Error> {
		let res = LDRecord {
			name:        {
				match item.name {
					Some(s) => s,
					None => return Err(LDConversionError::NameMissing),
				}
			},
			record_kind: item.kind,
			visibility:  match item.visibility {
				Some(v) => v,
				None => return Err(LDConversionError::VisibilityMissing),
			},
			generics:    item.generics.map(|g| json!(g)),
			fields:      item.fields.iter().flatten().map(|f| json!(f)).collect(),
		};
		Ok(res)
	}
}

// MARK: - LDUnion

#[derive(Serialize, Deserialize, Debug)]
pub struct LDUnion {
	pub types: Vec<Value>,
}

impl TryFrom<Vec<ir::ty::Type>> for LDUnion {
	type Error = LDConversionError;

	fn try_from(item: Vec<ir::ty::Type>) -> Result<Self, Self::Error> {
		Ok(LDUnion { types: item.iter().map(|t| json!(t)).collect() })
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
	#[serde(skip_serializing_if = "Option::is_none")]
	pub docs:               Option<String>,
}

impl TryFrom<ir::protocols::TraitDef> for LDTraitDef {
	type Error = LDConversionError;

	fn try_from(item: ir::protocols::TraitDef) -> Result<Self, Self::Error> {
		Ok(LDTraitDef {
			name:               item.name,
			visibility:         item.visibility.ok_or(LDConversionError::VisibilityMissing)?,
			generics:           item.generics.map(|g| json!(g)),
			super_traits:       item.super_traits.map(|s| json!(s)),
			associated_types:   item.associated_types.map(|a| json!(a)),
			required_methods:   item.required_methods.map(|r| json!(r)),
			provided_methods:   item.provided_methods.map(|p| json!(p)),
			required_constants: item.required_constants.map(|c| json!(c)),
			docs:               item.docs,
		})
	}
}

// MARK: - LDTraitImpl

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
	pub is_negative:          bool,
	pub is_blanket:           bool,
	pub is_unsafe:            bool,
	#[serde(skip_serializing_if = "Option::is_none")]
	pub docs:                 Option<String>,
}

impl TryFrom<ir::protocols::TraitImpl> for LDTraitImpl {
	type Error = LDConversionError;

	fn try_from(item: ir::protocols::TraitImpl) -> Result<Self, Self::Error> {
		Ok(LDTraitImpl {
			trait_ref:            json!(item.tr),
			for_type:             json!(item.for_type),
			visibility:           item.visibility.ok_or(LDConversionError::VisibilityMissing)?,
			generics:             item.generics.map(|g| json!(g)),
			methods:              item.methods.map(|m| json!(m)),
			associated_types:     item.associated_types.map(|a| json!(a)),
			associated_constants: item.associated_constants.map(|c| json!(c)),
			is_negative:          item.is_negative,
			is_blanket:           item.is_blanket,
			is_unsafe:            item.is_unsafe,
			docs:                 item.docs,
		})
	}
}

// MARK: - LDSum

#[derive(Serialize, Deserialize, Debug)]
pub struct LDSum {
	pub variants: Vec<Value>,
}

impl TryFrom<Vec<ir::record::SumVariant>> for LDSum {
	type Error = LDConversionError;

	fn try_from(item: Vec<ir::record::SumVariant>) -> Result<Self, Self::Error> {
		Ok(LDSum { variants: item.iter().map(|v| json!(v)).collect() })
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

impl TryFrom<ir::function::Function> for LDFunction {
	type Error = LDConversionError;

	fn try_from(item: ir::function::Function) -> Result<Self, Self::Error> {
		Ok(LDFunction {
			name:              item.name,
			visibility:        item.visibility.ok_or(LDConversionError::VisibilityMissing)?,
			implemented:       item.implemented,
			input_parameters:  item.input_parameters.map(|i| json!(i)),
			output_parameters: item.output_parameters.map(|o| json!(o)),
			attributes:        item.attributes.map(|a| json!(a)),
			generics:          item.generics.map(|g| json!(g)),
		})
	}
}

// MARK: - LDTypeAlias

#[derive(Serialize, Deserialize, Debug)]
pub struct LDTypeAlias {
	pub aliased_type: Value,
}

impl TryFrom<ir::ty::Type> for LDTypeAlias {
	type Error = LDConversionError;

	fn try_from(item: ir::ty::Type) -> Result<Self, Self::Error> {
		Ok(LDTypeAlias { aliased_type: json!(item) })
	}
}
