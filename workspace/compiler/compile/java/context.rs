//! The lowering context: name resolution, path minting, and the
//! extraction → [`Index`] driver.
//!
//! ## Path scheme
//! Mirrors the Go producer's `import/path::Name` scheme, dotted for Java:
//!
//! * a package's `Entry::Module` is keyed `Local("<package>")`
//!   (`Local("(default)")` for the unnamed package);
//! * a JPMS module's `Entry::Module` is keyed `Local("module:<name>")`;
//! * a type is keyed `Local("<package>::<Relative.Name>")`, where the
//!   relative name is the qualified name minus the package prefix — nested
//!   types keep their dotted spine (`com.example::Outer.Inner`), which *is*
//!   the enclosing provenance;
//! * a type's method group is keyed `Local("<package>::<Relative>.<method>")`.
//!
//! ## Name resolution
//! [`Lowering`] indexes every extracted [`schema::TypeDecl`] by qualified
//! name (for permits/interface wiring and nested-type lookups) and builds
//! the simple-name → fully-qualified-name resolver `javadoc` uses to resolve
//! `{@link}` targets. The resolver is seeded with the ubiquitous `java.lang`
//! names so bare references like `{@link String}` resolve even though the
//! JDK itself is never part of an extraction; extracted types override the
//! seed, and collisions between extracted simple names keep the first
//! qualified name in sorted order (deterministic; the oracle sorts types).
//!
//! ## Structure wiring
//! Nesting is expressed via `TypeDecl::enclosing` and is a tree by
//! construction; a visited guard in [`lower_extraction`] keeps a malformed
//! (cyclic) extraction from looping. Top-level types become their package
//! module's `members`; nested types are wired as members of their enclosing
//! record/trait by `item`.

use std::collections::HashMap;
use std::path::PathBuf;

use ir::entry::{Index, NudoxPath};
use ir::kind::{Entry, Symbol, Visibility};
use ir::module::Module;

use super::item;
use super::javadoc::{self, ParsedJavadoc};
use super::schema;

/// The name of the synthetic package used for types in the unnamed package.
pub const DEFAULT_PACKAGE: &str = "(default)";

/// Ubiquitous `java.lang` simple names, seeded into the link resolver.
const JAVA_LANG: &[&str] = &[
	"AutoCloseable", "Boolean", "Byte", "CharSequence", "Character", "Class",
	"ClassCastException", "Cloneable", "Comparable", "Deprecated", "Double", "Enum",
	"Error", "Exception", "Float", "FunctionalInterface", "IllegalArgumentException",
	"IllegalStateException", "IndexOutOfBoundsException", "Integer", "Iterable", "Long",
	"Math", "NullPointerException", "Number", "Object", "Override", "Record", "Runnable",
	"RuntimeException", "SafeVarargs", "Short", "String", "StringBuilder",
	"SuppressWarnings", "System", "Thread", "Throwable", "UnsupportedOperationException",
	"Void",
];

/// The lowering context for one extraction.
pub struct Lowering<'a> {
	/// Every type declaration, keyed by qualified name.
	types_by_name: HashMap<&'a str, &'a schema::TypeDecl>,
	/// Simple name → fully qualified name, for `{@link}` resolution.
	resolver: HashMap<String, String>,
}

impl<'a> Lowering<'a> {
	pub fn new(extraction: &'a schema::Extraction) -> Self {
		let mut types_by_name = HashMap::new();
		let mut resolver: HashMap<String, String> = HashMap::new();

		for name in JAVA_LANG {
			resolver.insert((*name).to_string(), format!("java.lang.{name}"));
		}
		for decl in &extraction.types {
			types_by_name.insert(decl.qualified_name.as_str(), decl);
			// Extracted types override the java.lang seed; the first
			// extracted claimant (types arrive name-sorted) wins ties.
			match resolver.get(&decl.simple_name) {
				Some(existing) if !existing.starts_with("java.lang.") => {}
				_ => {
					resolver
						.insert(decl.simple_name.clone(), decl.qualified_name.clone());
				}
			}
		}

		Lowering { types_by_name, resolver }
	}

	/// Look a type declaration up by qualified name.
	pub fn decl(&self, qualified: &str) -> Option<&'a schema::TypeDecl> {
		self.types_by_name.get(qualified).copied()
	}

	/// The simple-name resolver handed to the javadoc parser.
	pub fn resolver(&self) -> &HashMap<String, String> {
		&self.resolver
	}

	/// Parse a raw doc comment against this extraction's resolver.
	/// `self_type` is the qualified name of the documented (or enclosing)
	/// type, used for bare `#member` references.
	pub fn parse_doc(
		&self,
		doc: Option<&str>,
		doc_kind: Option<&str>,
		self_type: Option<&str>,
	) -> Option<ParsedJavadoc> {
		javadoc::parse_opt(doc, doc_kind, self_type, &self.resolver)
	}

	/// The `Index` key of a type declaration.
	pub fn type_key(&self, decl: &schema::TypeDecl) -> NudoxPath {
		item_key(&decl.package, relative_name(decl))
	}

	/// The `Index` key of a member (method group) of a type declaration.
	pub fn member_key(&self, decl: &schema::TypeDecl, member: &str) -> NudoxPath {
		item_key(&decl.package, &format!("{}.{}", relative_name(decl), member))
	}
}

/// A package's `Entry::Module` key.
pub fn package_key(package: &str) -> NudoxPath {
	let name = if package.is_empty() { DEFAULT_PACKAGE } else { package };
	NudoxPath::Local(PathBuf::from(name))
}

/// A package-scoped item key: `<package>::<relative>`.
pub fn item_key(package: &str, relative: &str) -> NudoxPath {
	let pkg = if package.is_empty() { DEFAULT_PACKAGE } else { package };
	NudoxPath::Local(PathBuf::from(format!("{pkg}::{relative}")))
}

/// The type's name relative to its package: the qualified name with the
/// package prefix stripped (`com.example.Outer.Inner` → `Outer.Inner`).
pub fn relative_name(decl: &schema::TypeDecl) -> &str {
	if decl.package.is_empty() {
		return &decl.qualified_name;
	}
	decl.qualified_name
		.strip_prefix(decl.package.as_str())
		.and_then(|rest| rest.strip_prefix('.'))
		.unwrap_or(&decl.qualified_name)
}

/// Whether a declaration is part of the API surface we lower. Local and
/// anonymous classes are implementation detail (javadoc rarely includes
/// them, but the guard costs nothing).
fn is_api_type(decl: &schema::TypeDecl) -> bool {
	matches!(decl.nesting.as_str(), "TOP_LEVEL" | "MEMBER")
}

/// Lower a whole extraction into an [`Index`].
///
/// Packages become `Entry::Module`s (their `members` = top-level types),
/// JPMS modules become doc-only `Entry::Module`s, and every included type
/// lowers via [`item::lower_type_decl`]. `root_ids` are the package modules
/// (plus JPMS modules when present).
pub fn lower_extraction(extraction: &schema::Extraction) -> Index {
	let ctx = Lowering::new(extraction);

	let mut index = Index { root_ids: Vec::new(), entries_by_path: Default::default() };

	// --- 1. JPMS modules (doc-only entries). ------------------------------
	for module in &extraction.modules {
		let key = NudoxPath::Local(PathBuf::from(format!("module:{}", module.name)));
		let documentation = module_documentation(&ctx, module);
		index.root_ids.push(key.clone());
		index.entries_by_path.insert(
			key.clone(),
			Entry::Module(Symbol {
				name: module.name.clone(),
				path: key,
				aliases: None,
				visibility: Visibility::Public,
				documentation,
				deprecation: None,
				doc_links: None,
				inner: Module { members: None },
			}),
		);
	}

	// --- 2. Packages. ------------------------------------------------------
	// Types can live in packages that carry no package-info.java (and hence
	// no `packages` entry); collect the union of declared + inhabited.
	let mut package_names: Vec<&str> =
		extraction.packages.iter().map(|p| p.name.as_str()).collect();
	for decl in &extraction.types {
		if !package_names.contains(&decl.package.as_str()) {
			package_names.push(decl.package.as_str());
		}
	}
	package_names.sort_unstable();
	package_names.dedup();

	let declared: HashMap<&str, &schema::Package> =
		extraction.packages.iter().map(|p| (p.name.as_str(), p)).collect();

	for name in &package_names {
		let key = package_key(name);
		let documentation = declared.get(name).and_then(|p| {
			let parsed =
				ctx.parse_doc(p.doc.as_deref(), p.doc_kind.as_deref(), None)?;
			let mut sections: Vec<String> = Vec::new();
			if let Some(text) = parsed.documentation() {
				sections.push(text);
			}
			sections.extend(item::tag_sections(&parsed));
			if sections.is_empty() { None } else { Some(sections.join("\n\n")) }
		});
		let display =
			if name.is_empty() { DEFAULT_PACKAGE.to_string() } else { (*name).to_string() };
		index.root_ids.push(key.clone());
		index.entries_by_path.insert(
			key.clone(),
			Entry::Module(Symbol {
				name: display,
				path: key,
				aliases: None,
				visibility: Visibility::Public,
				documentation,
				deprecation: None,
				doc_links: None,
				inner: Module { members: None },
			}),
		);
	}

	// --- 3. Types (flat; nesting is wired via keys + members). -------------
	// The `enclosing` chain is a tree by construction; the visited guard
	// only protects against a malformed extraction.
	let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
	let mut members_by_package: HashMap<String, Vec<NudoxPath>> = HashMap::new();

	for decl in &extraction.types {
		if !is_api_type(decl) || !visited.insert(decl.qualified_name.as_str()) {
			continue;
		}
		if decl.enclosing.is_none() {
			members_by_package
				.entry(decl.package.clone())
				.or_default()
				.push(ctx.type_key(decl));
		}
		for (path, entry) in item::lower_type_decl(&ctx, decl) {
			index.entries_by_path.insert(path, entry);
		}
	}

	// --- 4. Wire package members. -------------------------------------------
	for (package, mut members) in members_by_package {
		members.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
		let key = package_key(&package);
		if let Some(Entry::Module(symbol)) = index.entries_by_path.get_mut(&key) {
			symbol.inner.members = Some(members);
		}
	}

	index.root_ids.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
	index
}

/// A JPMS module's documentation: its doc comment plus a readable summary
/// of its directives (the IR module vocabulary has no directive slots).
fn module_documentation(ctx: &Lowering<'_>, module: &schema::Module) -> Option<String> {
	let mut sections: Vec<String> = Vec::new();

	if let Some(parsed) =
		ctx.parse_doc(module.doc.as_deref(), module.doc_kind.as_deref(), None)
	{
		if let Some(doc) = parsed.documentation() {
			sections.push(doc);
		}
	}

	let mut lines: Vec<String> = Vec::new();
	for directive in &module.directives {
		lines.push(match directive {
			schema::Directive::Requires { module, transitive, is_static } => {
				let mut flags = String::new();
				if *transitive {
					flags.push_str(" transitive");
				}
				if *is_static {
					flags.push_str(" static");
				}
				format!("- requires{flags} `{module}`")
			}
			schema::Directive::Exports { package, to } => match to {
				Some(targets) => {
					format!("- exports `{package}` to `{}`", targets.join("`, `"))
				}
				None => format!("- exports `{package}`"),
			},
			schema::Directive::Opens { package, to } => match to {
				Some(targets) => {
					format!("- opens `{package}` to `{}`", targets.join("`, `"))
				}
				None => format!("- opens `{package}`"),
			},
			schema::Directive::Uses { service } => format!("- uses `{service}`"),
			schema::Directive::Provides { service, implementations } => format!(
				"- provides `{service}` with `{}`",
				implementations.join("`, `")
			),
		});
	}
	if !lines.is_empty() {
		sections.push(format!("Module directives:\n{}", lines.join("\n")));
	}

	if sections.is_empty() { None } else { Some(sections.join("\n\n")) }
}

#[cfg(test)]
mod tests {
	use super::*;

	fn decl(qualified: &str, package: &str, simple: &str) -> schema::TypeDecl {
		// A minimal TypeDecl via serde keeps this test honest about
		// `#[serde(default)]` coverage on optional fields.
		let json = serde_json::json!({
			"qualifiedName": qualified,
			"simpleName": simple,
			"kind": "CLASS",
			"package": package,
			"module": null,
			"enclosing": null,
			"nesting": "TOP_LEVEL",
			"superclass": null,
			"doc": null,
			"docKind": null,
			"position": null,
		});
		serde_json::from_value(json).expect("minimal TypeDecl deserializes")
	}

	#[test]
	fn relative_names_keep_the_nesting_spine() {
		let mut d = decl("com.example.Outer.Inner", "com.example", "Inner");
		assert_eq!(relative_name(&d), "Outer.Inner");
		d.package = String::new();
		d.qualified_name = "Loner".to_string();
		assert_eq!(relative_name(&d), "Loner");
	}

	#[test]
	fn resolver_prefers_extracted_types() {
		let extraction: schema::Extraction = serde_json::from_value(serde_json::json!({
			"format": 1,
			"javaVersion": "25",
			"types": [
				serde_json::json!({
					"qualifiedName": "com.example.String",
					"simpleName": "String",
					"kind": "CLASS",
					"package": "com.example",
					"module": null, "enclosing": null, "nesting": "TOP_LEVEL",
					"superclass": null, "doc": null, "docKind": null, "position": null,
				}),
			],
		}))
		.expect("extraction deserializes");
		let ctx = Lowering::new(&extraction);
		assert_eq!(
			ctx.resolver().get("String").map(String::as_str),
			Some("com.example.String")
		);
		assert_eq!(
			ctx.resolver().get("Object").map(String::as_str),
			Some("java.lang.Object")
		);
	}
}
