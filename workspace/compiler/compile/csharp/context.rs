//! The lowering context: path minting, declaration lookup, and the
//! extraction → [`Index`] driver (CSHARP-PLAN §3.4).
//!
//! ## Path scheme
//! Mirrors the Java producer's dotted `package::Name` scheme, with C#
//! namespaces as the container:
//!
//! * a namespace's `Entry::Module` is keyed `Local("<namespace>")`
//!   (`Local("(global)")` for the global namespace);
//! * a type is keyed `Local("<namespace>::<Relative.Name>")`, where the
//!   relative name is the qualified metadata name minus the namespace prefix
//!   — nested types keep their dotted spine (`N::Outer.Inner`) and their
//!   **arity backticks** (`N::List\`1`), since C# types overload by arity;
//! * a type's member group is keyed `Local("<namespace>::<Relative>.<member>")`.

use std::collections::HashMap;
use std::path::PathBuf;

use ir::entry::{Index, NudoxPath};
use ir::kind::{Entry, Symbol, Visibility};
use ir::module::Module;

use super::item;
use super::schema;
use super::xmldoc;

/// The name of the synthetic namespace used for types in the global namespace.
pub const GLOBAL_NAMESPACE: &str = "(global)";

/// The lowering context for one extraction.
pub struct Lowering<'a> {
	/// Every type declaration, keyed by qualified metadata name.
	types_by_name: HashMap<&'a str, &'a schema::TypeDecl>,
	/// Every type declaration, keyed by its documentation-comment id.
	types_by_doc_id: HashMap<&'a str, &'a schema::TypeDecl>,
}

impl<'a> Lowering<'a> {
	pub fn new(extraction: &'a schema::Extraction) -> Self {
		let mut types_by_name = HashMap::new();
		let mut types_by_doc_id = HashMap::new();
		for decl in &extraction.types {
			types_by_name.insert(decl.qualified_name.as_str(), decl);
			if !decl.doc_id.is_empty() {
				types_by_doc_id.insert(decl.doc_id.as_str(), decl);
			}
		}
		Lowering { types_by_name, types_by_doc_id }
	}

	/// Look a type declaration up by qualified metadata name.
	pub fn decl(&self, qualified: &str) -> Option<&'a schema::TypeDecl> {
		self.types_by_name.get(qualified).copied()
	}

	/// The `Index` key of a type declaration.
	pub fn type_key(&self, decl: &schema::TypeDecl) -> NudoxPath {
		item_key(&decl.namespace, &relative_name(decl))
	}

	/// The `Index` key of a member (method group / field) of a type.
	pub fn member_key(&self, decl: &schema::TypeDecl, member: &str) -> NudoxPath {
		item_key(&decl.namespace, &format!("{}.{}", relative_name(decl), member))
	}

	/// Best-effort resolution of a documentation-comment id to a path: type
	/// doc-ids resolve to the type's key; anything else stays a literal path.
	pub fn doc_id_to_path(&self, doc_id: &str) -> NudoxPath {
		if let Some(decl) = self.types_by_doc_id.get(doc_id) {
			return self.type_key(decl);
		}
		NudoxPath::Local(PathBuf::from(doc_id))
	}
}

/// A namespace's `Entry::Module` key.
pub fn namespace_key(namespace: &str) -> NudoxPath {
	let name = if namespace.is_empty() { GLOBAL_NAMESPACE } else { namespace };
	NudoxPath::Local(PathBuf::from(name))
}

/// A namespace-scoped item key: `<namespace>::<relative>`.
pub fn item_key(namespace: &str, relative: &str) -> NudoxPath {
	let ns = if namespace.is_empty() { GLOBAL_NAMESPACE } else { namespace };
	NudoxPath::Local(PathBuf::from(format!("{ns}::{relative}")))
}

/// The type's name relative to its namespace: the qualified metadata name with
/// the namespace prefix stripped and metadata nesting (`+`) normalized to `.`
/// (`N.Outer+Inner` → `Outer.Inner`). Arity backticks are kept.
pub fn relative_name(decl: &schema::TypeDecl) -> String {
	let rest = if decl.namespace.is_empty() {
		decl.qualified_name.as_str()
	} else {
		decl.qualified_name
			.strip_prefix(decl.namespace.as_str())
			.and_then(|r| r.strip_prefix('.'))
			.unwrap_or(&decl.qualified_name)
	};
	rest.replace('+', ".")
}

/// Lower a whole extraction into an [`Index`].
///
/// Namespaces become `Entry::Module`s (their `members` = top-level types), and
/// every included type lowers via [`item::lower_type_decl`]. Nested types are
/// wired as members of their enclosing record/trait by `item`.
pub fn lower_extraction(extraction: &schema::Extraction) -> Index {
	let ctx = Lowering::new(extraction);

	let mut index = Index { root_ids: Vec::new(), entries_by_path: Default::default() };

	// --- 1. Namespaces (declared + inhabited). -----------------------------
	let mut namespace_names: Vec<&str> =
		extraction.namespaces.iter().map(|n| n.name.as_str()).collect();
	for decl in &extraction.types {
		if decl.enclosing.is_none() && !namespace_names.contains(&decl.namespace.as_str()) {
			namespace_names.push(decl.namespace.as_str());
		}
	}
	namespace_names.sort_unstable();
	namespace_names.dedup();

	let declared: HashMap<&str, &schema::Namespace> =
		extraction.namespaces.iter().map(|n| (n.name.as_str(), n)).collect();

	for name in &namespace_names {
		let key = namespace_key(name);
		let documentation = declared
			.get(name)
			.and_then(|n| xmldoc::parse_opt(n.doc.as_deref()))
			.and_then(|p| p.documentation());
		let display = if name.is_empty() { GLOBAL_NAMESPACE.to_string() } else { (*name).to_string() };
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

	// --- 2. Types (flat; nesting is wired via keys + members). -------------
	let mut visited: std::collections::HashSet<&str> = std::collections::HashSet::new();
	let mut members_by_namespace: HashMap<String, Vec<NudoxPath>> = HashMap::new();

	for decl in &extraction.types {
		if !visited.insert(decl.qualified_name.as_str()) {
			continue;
		}
		if decl.enclosing.is_none() {
			members_by_namespace
				.entry(decl.namespace.clone())
				.or_default()
				.push(ctx.type_key(decl));
		}
		for (path, entry) in item::lower_type_decl(&ctx, decl) {
			index.entries_by_path.insert(path, entry);
		}
	}

	// --- 3. Wire namespace members. ----------------------------------------
	for (namespace, mut members) in members_by_namespace {
		members.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
		let key = namespace_key(&namespace);
		if let Some(Entry::Module(symbol)) = index.entries_by_path.get_mut(&key) {
			symbol.inner.members = Some(members);
		}
	}

	index.root_ids.sort_by(|a, b| format!("{a:?}").cmp(&format!("{b:?}")));
	index
}

#[cfg(test)]
mod tests {
	use super::*;

	fn decl(qualified: &str, namespace: &str, simple: &str, enclosing: Option<&str>) -> schema::TypeDecl {
		let json = serde_json::json!({
			"docId": format!("T:{qualified}"),
			"qualifiedName": qualified,
			"simpleName": simple,
			"kind": "CLASS",
			"namespace": namespace,
			"enclosing": enclosing,
		});
		serde_json::from_value(json).expect("minimal TypeDecl deserializes")
	}

	#[test]
	fn relative_name_keeps_nesting_and_arity() {
		let d = decl("N.Outer`1+Inner", "N", "Inner", Some("N.Outer`1"));
		assert_eq!(relative_name(&d), "Outer`1.Inner");
		let g = decl("Loner", "", "Loner", None);
		assert_eq!(relative_name(&g), "Loner");
	}

	#[test]
	fn keys_use_namespace_scope() {
		let d = decl("N.C", "N", "C", None);
		assert_eq!(item_key("N", "C"), NudoxPath::Local(PathBuf::from("N::C")));
		let g = decl("C", "", "C", None);
		assert_eq!(
			namespace_key(&g.namespace),
			NudoxPath::Local(PathBuf::from(GLOBAL_NAMESPACE))
		);
		let _ = d;
	}
}
