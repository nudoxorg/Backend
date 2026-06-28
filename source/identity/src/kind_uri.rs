use std::fmt;

use crate::entry_uri::canonical_rust_package;

/// The schema class that prefixes a [`KindUri`].
///
/// Each variant corresponds to one TerminusDB schema class.
/// [`KindPrefix::Other`] captures classes added after this enum so call sites
/// degrade gracefully instead of panicking on unknown input.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum KindPrefix {
	Constant,
	Event,
	Field,
	Function,
	Info,
	Macro,
	Module,
	PrimitiveType,
	RecordType,
	SumType,
	TraitDef,
	TraitImpl,
	TypeAlias,
	UnionType,
	Variable,
	/// A schema class not yet represented as a variant.
	Other(String),
}

impl KindPrefix {
	/// The wire string for this prefix (used in URI serialization).
	pub fn as_str(&self) -> &str {
		match self {
			KindPrefix::Constant => "Constant",
			KindPrefix::Event => "Event",
			KindPrefix::Field => "Field",
			KindPrefix::Function => "Function",
			KindPrefix::Info => "Info",
			KindPrefix::Macro => "Macro",
			KindPrefix::Module => "Module",
			KindPrefix::PrimitiveType => "PrimitiveType",
			KindPrefix::RecordType => "RecordType",
			KindPrefix::SumType => "SumType",
			KindPrefix::TraitDef => "TraitDef",
			KindPrefix::TraitImpl => "TraitImpl",
			KindPrefix::TypeAlias => "TypeAlias",
			KindPrefix::UnionType => "UnionType",
			KindPrefix::Variable => "Variable",
			KindPrefix::Other(s) => s.as_str(),
		}
	}
}

impl From<&str> for KindPrefix {
	fn from(s: &str) -> Self {
		match s {
			"Constant" => KindPrefix::Constant,
			"Event" => KindPrefix::Event,
			"Field" => KindPrefix::Field,
			"Function" => KindPrefix::Function,
			"Info" => KindPrefix::Info,
			"Macro" => KindPrefix::Macro,
			"Module" => KindPrefix::Module,
			"PrimitiveType" => KindPrefix::PrimitiveType,
			"RecordType" => KindPrefix::RecordType,
			"SumType" => KindPrefix::SumType,
			"TraitDef" => KindPrefix::TraitDef,
			"TraitImpl" => KindPrefix::TraitImpl,
			"TypeAlias" => KindPrefix::TypeAlias,
			"UnionType" => KindPrefix::UnionType,
			"Variable" => KindPrefix::Variable,
			other => KindPrefix::Other(other.to_owned()),
		}
	}
}

impl fmt::Display for KindPrefix {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
}

/// Canonical `{prefix}/{lang}/{package}/{path}` URI for Kind documents stored
/// in TerminusDB. The prefix is the schema class name (e.g. `Function`,
/// `RecordType`), mirroring the `Entry/…` scheme used by [`EntryUri`].
///
/// [`EntryUri`]: crate::EntryUri
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct KindUri {
	prefix:  KindPrefix,
	lang:    String,
	package: String,
	path:    String,
}

impl KindUri {
	/// Construct a canonical kind URI. `lang` is trimmed and lowercased; for
	/// Rust the `package` segment is normalized (same rule as [`EntryUri::new`]).
	pub fn new(prefix: KindPrefix, lang: &str, package: &str, path: &str) -> Self {
		let lang = lang.trim().to_ascii_lowercase();
		let package =
			if lang == "rust" { canonical_rust_package(package, path) } else { package.to_owned() };
		Self { prefix, lang, package, path: path.to_owned() }
	}

	/// Parse `"{prefix}/{lang}/{package}/{path}"`. Returns `None` if the string
	/// contains fewer than four slash-separated segments.
	///
	/// Invariant: `KindUri::parse(uri.to_string()) == Some(uri)` for any
	/// `uri` produced by `KindUri::new`.
	pub fn parse(s: &str) -> Option<Self> {
		let (prefix_str, rest) = s.split_once('/')?;
		let (lang, rest) = rest.split_once('/')?;
		let (package, path) = rest.split_once('/')?;
		Some(Self::new(KindPrefix::from(prefix_str), lang, package, path))
	}

	pub fn prefix(&self) -> &KindPrefix { &self.prefix }

	pub fn lang(&self) -> &str { &self.lang }

	pub fn package(&self) -> &str { &self.package }

	pub fn path(&self) -> &str { &self.path }
}

impl fmt::Display for KindUri {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		write!(f, "{}/{}/{}/{}", self.prefix, self.lang, self.package, self.path)
	}
}

impl From<KindUri> for String {
	fn from(u: KindUri) -> String { u.to_string() }
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trip() {
		let cases = [
			(KindPrefix::Function, "rust", "serde", "serde::Serialize"),
			(KindPrefix::RecordType, "typescript", "zod", "ZodString"),
			(KindPrefix::Module, "rust", "cranelift-object", "cranelift_object::ObjectModule"),
		];
		for (prefix, lang, pkg, path) in cases {
			let uri = KindUri::new(prefix, lang, pkg, path);
			let parsed = KindUri::parse(&uri.to_string())
				.unwrap_or_else(|| panic!("parse failed for {lang}/{pkg}/{path}"));
			assert_eq!(uri, parsed);
		}
	}

	#[test]
	fn rust_package_is_normalized() {
		let uri = KindUri::new(
			KindPrefix::Function,
			"rust",
			"cranelift_object",
			"cranelift_object::ObjectModule",
		);
		assert_eq!(uri.package(), "cranelift-object");
	}

	#[test]
	fn other_prefix_round_trips() {
		let uri = KindUri::new(KindPrefix::Other("FutureThing".into()), "rust", "serde", "Serialize");
		let parsed = KindUri::parse(&uri.to_string()).unwrap();
		assert_eq!(uri, parsed);
	}

	#[test]
	fn parse_rejects_too_few_segments() {
		assert!(KindUri::parse("Function/rust/serde").is_none());
	}
}
