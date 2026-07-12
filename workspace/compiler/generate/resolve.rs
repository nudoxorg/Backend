//! Resolution + attribution: the language-agnostic engine that turns a
//! package's per-file [`Extraction`]s into an [`OccurrenceSet`]
//! (REFERENCES-PLAN §4.2).
//!
//! For each use-site it runs the resolution ladder (shadow → rooted → import →
//! lexical scope → package-wide → suffix → unresolved), attaching a
//! [`Confidence`] tier. For each span it finds the innermost enclosing
//! definition (containment), yielding the syntactic FQN even when that
//! definition is not in the surface index (principle 4: attribution never
//! fails). Every definition also emits a `Definition` occurrence, giving IR
//! items declaration coordinates for free.
//!
//! Language specifics end at the `Extraction` boundary; the only per-language
//! knowledge here is folding the absolute markers (`crate`/`self`/`super`).

use std::{ops::Range, path::PathBuf};

use heart::Language;
use ir::{
	entry::{Index, NudoxPath},
	syntax::{Confidence, FileOccurrences, Occurrence, OccurrenceSet, ReferenceKind, ResolutionStats, Role},
};

use crate::{
	graph::symtab::SymbolTable,
	treesitter::spec::{
		DefKind, Extraction, ImportBinding, ImportSource, RawDefinition, RawReference, ReceiverShape,
	},
};

/// Bump when the ladder, marker folding, or attribution logic changes: it keys
/// the occurrences pipeline stage's CAS entry (REFERENCES-PLAN §4.3).
pub const RESOLVER_VERSION: &str = "occ-v1";

/// Resolve and attribute every file's extraction into the package occurrence
/// corpus.
pub fn resolve(lang: Language, files: &[(PathBuf, Extraction)], index: &Index) -> OccurrenceSet {
	let symtab = SymbolTable::build(index);
	let mut stats = ResolutionStats::default();
	let mut out = Vec::with_capacity(files.len());

	for (path, extraction) in files {
		let file = FileResolver { lang, symtab: &symtab, extraction };
		let occurrences = file.resolve_file(&mut stats);
		out.push(FileOccurrences { path: path.clone(), occurrences });
	}

	out.sort_by(|a, b| a.path.cmp(&b.path));
	OccurrenceSet { files: out, stats }
}

/// Per-file resolution state.
struct FileResolver<'a> {
	lang: Language,
	symtab: &'a SymbolTable,
	extraction: &'a Extraction,
}

impl FileResolver<'_> {
	fn resolve_file(&self, stats: &mut ResolutionStats) -> Vec<Occurrence> {
		let defs = &self.extraction.definitions;
		let module = &self.extraction.module_path;

		// Precompute each definition's fq segments + anchored path once.
		let anchored: Vec<(Vec<String>, NudoxPath, bool)> = (0..defs.len())
			.map(|i| {
				let fq = def_fq_segments(defs, module, i);
				let (path, is_anchored) = self.anchor(&fq);
				(fq, path, is_anchored)
			})
			.collect();

		let mut occurrences = Vec::new();

		// Definition occurrences: one per definition, at its name span.
		for (i, def) in defs.iter().enumerate() {
			let (_, target, is_anchored) = &anchored[i];
			let (enclosing, encl_anchored) = match def.parent {
				Some(p) => (Some(anchored[p].1.clone()), anchored[p].2),
				None => (None, false),
			};
			let confidence = if *is_anchored { Confidence::Index } else { Confidence::Syntactic };
			stats.record(def_ref_kind(&def.kind), confidence);
			occurrences.push(Occurrence {
				span: def.name_span.clone(),
				target: target.clone(),
				kind: def_ref_kind(&def.kind),
				role: Role::Definition,
				enclosing,
				anchored: encl_anchored,
				confidence,
			});
		}

		// Reference occurrences.
		for r in &self.extraction.references {
			let encl = enclosing_def(defs, &r.span);
			let (enclosing, encl_anchored) = match encl {
				Some(i) => (Some(anchored[i].1.clone()), anchored[i].2),
				None => (None, false),
			};
			// Enclosing names (past the module prefix) for the lexical walk.
			let encl_names: &[String] = match encl {
				Some(i) => &anchored[i].0[module.len().min(anchored[i].0.len())..],
				None => &[],
			};

			match self.resolve_reference(r, module, encl_names) {
				Some((target, confidence)) => {
					stats.record(r.kind, confidence);
					occurrences.push(Occurrence {
						span: r.span.clone(),
						target,
						kind: r.kind,
						role: Role::Reference,
						enclosing,
						anchored: encl_anchored,
						confidence,
					});
				}
				None => stats.record_unresolved(),
			}
		}

		occurrences.sort_by(|a, b| a.span.start.cmp(&b.span.start).then(a.span.end.cmp(&b.span.end)));
		occurrences
	}

	/// The resolution ladder for one reference. Returns the resolved target and
	/// the confidence tier, or `None` (tallied unresolved, not emitted).
	fn resolve_reference(
		&self,
		r: &RawReference,
		module: &[String],
		encl_names: &[String],
	) -> Option<(NudoxPath, Confidence)> {
		let chain = &r.segments;
		if chain.is_empty() {
			return None;
		}

		// 1. Local shadow: a `self`/`cls` receiver's method attributes to the
		//    enclosing type, not a global; drop it to the syntactic tier. (Full
		//    let-binding shadowing needs the oracle tier.)
		if matches!(r.receiver, Some(ReceiverShape::SelfRef | ReceiverShape::ClassRef)) {
			// Try the enclosing type: module + all-but-last enclosing names + leaf.
			if let Some(leaf) = chain.last() {
				if !encl_names.is_empty() {
					let mut cand: Vec<String> =
						module.iter().chain(&encl_names[..encl_names.len() - 1]).cloned().collect();
					cand.push(leaf.clone());
					if let Some(p) = self.symtab.resolve_exact(&cand.join("::")) {
						return Some((p.clone(), Confidence::Index));
					}
				}
			}
			// Fall through: still try the general ladder below.
		}

		// 2. Rooted absolute marker (crate/self/super), folded against M.
		if let Some(abs) = fold_root_marker(self.lang, chain, module) {
			if let Some(p) = self.symtab.resolve_exact(&abs.join("::")) {
				return Some((p.clone(), Confidence::Index));
			}
		}

		// 3. Import binding on the head segment.
		if let Some(binding) = self.import_for(&chain[0]) {
			if let Some(hit) = self.resolve_via_import(binding, chain, module) {
				return Some(hit);
			}
		}

		// 4. Lexical scope walk: innermost enclosing prefix → file root.
		for k in (0..=encl_names.len()).rev() {
			let cand: Vec<String> =
				module.iter().chain(&encl_names[..k]).chain(chain).cloned().collect();
			if let Some(p) = self.symtab.resolve_exact(&cand.join("::")) {
				return Some((p.clone(), Confidence::Index));
			}
		}

		// 5. Package-wide exact / alias.
		if let Some(p) = self.symtab.resolve_exact(&chain.join("::")) {
			return Some((p.clone(), Confidence::Index));
		}

		// 6. Unique suffix (leaf segment).
		if let Some(leaf) = chain.last() {
			if let Some(p) = self.symtab.resolve_suffix(leaf) {
				return Some((p.clone(), Confidence::Suffix));
			}
		}

		// 7. Unresolved.
		None
	}

	/// Resolve `chain` given that its head is bound by `binding`.
	fn resolve_via_import(
		&self,
		binding: &ImportBinding,
		chain: &[String],
		module: &[String],
	) -> Option<(NudoxPath, Confidence)> {
		let tail = &chain[1..];
		match &binding.source {
			ImportSource::Internal(base) => {
				let base = fold_internal_base(self.lang, base, module);
				let full: Vec<String> = base.iter().chain(tail).cloned().collect();
				self.symtab
					.resolve_exact(&full.join("::"))
					.map(|p| (p.clone(), Confidence::Import))
			}
			ImportSource::External { dependency, path } => {
				let full: Vec<String> = path.iter().chain(tail).cloned().collect();
				Some((
					NudoxPath::External {
						dependency: dependency.clone(),
						path: full.iter().collect(),
					},
					Confidence::Import,
				))
			}
			ImportSource::Glob(prefix) => {
				// Try the (relative-folded) glob prefix joined with the whole chain.
				let base = fold_internal_base(self.lang, &prefix.path, module);
				let full: Vec<String> = base.iter().chain(chain).cloned().collect();
				self.symtab
					.resolve_exact(&full.join("::"))
					.map(|p| (p.clone(), Confidence::Import))
			}
		}
	}

	/// The (first) import binding whose local name is `head`.
	fn import_for(&self, head: &str) -> Option<&ImportBinding> {
		self.extraction.imports.iter().find(|b| b.local == head)
	}

	/// Anchor an fq: the index's canonical path if present (anchored), else a
	/// syntactic `Local` path (unanchored).
	fn anchor(&self, fq_segments: &[String]) -> (NudoxPath, bool) {
		let fq = fq_segments.join("::");
		match self.symtab.resolve_exact(&fq) {
			Some(p) => (p.clone(), true),
			None => (NudoxPath::Local(fq_segments.iter().collect()), false),
		}
	}
}

// ─── Attribution helpers ─────────────────────────────────────────────────────

/// The fq segments of definition `i`: module path ⧺ names along its parent
/// chain (outermost → the definition itself).
fn def_fq_segments(defs: &[RawDefinition], module: &[String], i: usize) -> Vec<String> {
	let mut names = Vec::new();
	let mut cur = Some(i);
	while let Some(idx) = cur {
		names.push(defs[idx].name.clone());
		cur = defs[idx].parent;
	}
	names.reverse();
	let mut fq = module.to_vec();
	fq.extend(names);
	fq
}

/// The innermost definition whose body span contains `span`, if any.
fn enclosing_def(defs: &[RawDefinition], span: &Range<usize>) -> Option<usize> {
	let mut best: Option<usize> = None;
	for (i, d) in defs.iter().enumerate() {
		if d.body_span.start <= span.start && span.end <= d.body_span.end {
			let width = d.body_span.end - d.body_span.start;
			let take = match best {
				Some(b) => width < defs[b].body_span.end - defs[b].body_span.start,
				None => true,
			};
			if take {
				best = Some(i);
			}
		}
	}
	best
}

/// Normalize an internal import base into concrete module segments, folding
/// relative markers against the importing file's module path:
///
/// - Python relative dots (encoded by the extractor as leading empty segments):
///   `n` leading `""` pops `n` segments off `module`, then appends the rest.
/// - Rust `crate`/`self`/`super` roots, via [`fold_root_marker`].
/// - Otherwise the base is already absolute-in-package and used as-is.
fn fold_internal_base(lang: Language, base: &[String], module: &[String]) -> Vec<String> {
	let empties = base.iter().take_while(|s| s.is_empty()).count();
	if empties > 0 {
		let kept = &module[..module.len().saturating_sub(empties)];
		return kept.iter().chain(&base[empties..]).cloned().collect();
	}
	if let Some(folded) = fold_root_marker(lang, base, module) {
		return folded;
	}
	base.to_vec()
}

// ─── Language marker folding ─────────────────────────────────────────────────

/// Fold an absolute path marker into concrete module segments, or `None` when
/// the chain is not rooted at a marker this language recognizes.
fn fold_root_marker(lang: Language, chain: &[String], module: &[String]) -> Option<Vec<String>> {
	match lang {
		Language::Rust => match chain.first().map(String::as_str) {
			// `crate::a::b` → the crate root (the first module segment, i.e. the
			// crate name, since module paths are crate-rooted) ⧺ the tail.
			Some("crate") => {
				let root = &module[..module.len().min(1)];
				Some(root.iter().chain(&chain[1..]).cloned().collect())
			}
			// `self::a` → relative to the current module.
			Some("self") => Some(module.iter().chain(&chain[1..]).cloned().collect()),
			// `super::a` → relative to the parent module.
			Some("super") => {
				let parent = &module[..module.len().saturating_sub(1)];
				Some(parent.iter().chain(&chain[1..]).cloned().collect())
			}
			_ => None,
		},
		// Other languages route absolute qualifiers through the import table.
		_ => None,
	}
}

// ─── Kind mapping for definitions ────────────────────────────────────────────

/// The `ReferenceKind` a `Definition` occurrence carries — it reflects the
/// defined entity's category (consumers discriminate on `role`, not `kind`).
fn def_ref_kind(kind: &DefKind) -> ReferenceKind {
	match kind {
		DefKind::Function => ReferenceKind::FunctionCall,
		DefKind::Method => ReferenceKind::MethodCall,
		DefKind::Type | DefKind::Trait | DefKind::Impl { .. } | DefKind::Class => {
			ReferenceKind::TypeReference
		}
		DefKind::Module => ReferenceKind::Import,
	}
}

#[cfg(test)]
mod tests {
	use std::path::PathBuf;

	use ir::kind::{Entry, Symbol};

	use super::*;
	use crate::treesitter::spec::{RawDefinition, RawReference};

	fn local(seg: &str) -> NudoxPath {
		NudoxPath::Local(PathBuf::from(seg))
	}

	fn func(name: &str, path: &str) -> (NudoxPath, Entry) {
		let mut s = Symbol::placeholder(ir::function::Function {
			input_parameters: None,
			output_parameters: None,
			type_links: None,
			attributes: None,
			generics: None,
			receiver: None,
			overloads: None,
			implemented: true,
			members: None,
			implemented_protocols: None,
		});
		s.name = name.into();
		s.path = local(path);
		(local(path), Entry::Function(s))
	}

	fn index(entries: Vec<(NudoxPath, Entry)>) -> Index {
		Index { root_ids: vec![], entries_by_path: entries.into_iter().collect() }
	}

	fn def(name: &str, kind: DefKind, body: Range<usize>, name_span: Range<usize>, parent: Option<usize>) -> RawDefinition {
		RawDefinition { name: name.into(), kind, name_span, body_span: body, parent }
	}

	fn call(seg: &str, span: Range<usize>) -> RawReference {
		RawReference { segments: vec![seg.into()], span, kind: ReferenceKind::FunctionCall, receiver: None }
	}

	/// `fn hello() { yo() }` — the call resolves to `yo` and attributes to
	/// `hello` (the dream, in miniature).
	#[test]
	fn resolves_call_and_attributes_to_enclosing() {
		let extraction = Extraction {
			definitions: vec![
				def("hello", DefKind::Function, 0..20, 3..8, None),
				def("yo", DefKind::Function, 30..40, 33..35, None),
			],
			imports: vec![],
			references: vec![call("yo", 12..14)],
			module_path: vec![],
		};
		let files = vec![(PathBuf::from("lib.rs"), extraction)];
		let set = resolve(Language::Rust, &files, &index(vec![func("yo", "yo"), func("hello", "hello")]));

		let refs: Vec<_> = set.files[0].occurrences.iter().filter(|o| o.role == Role::Reference).collect();
		assert_eq!(refs.len(), 1);
		assert_eq!(refs[0].target, local("yo"));
		assert_eq!(refs[0].enclosing, Some(local("hello")));
		assert!(refs[0].anchored);
		assert_eq!(refs[0].confidence, Confidence::Index);

		let defs = set.files[0].occurrences.iter().filter(|o| o.role == Role::Definition).count();
		assert_eq!(defs, 2, "one Definition occurrence per definition");
	}

	/// An unknown callee is tallied unresolved, never emitted.
	#[test]
	fn unknown_callee_is_tallied_not_emitted() {
		let extraction = Extraction {
			definitions: vec![def("hello", DefKind::Function, 0..20, 3..8, None)],
			imports: vec![],
			references: vec![call("nope", 12..16)],
			module_path: vec![],
		};
		let files = vec![(PathBuf::from("lib.rs"), extraction)];
		let set = resolve(Language::Rust, &files, &index(vec![func("hello", "hello")]));

		assert_eq!(set.stats.unresolved, 1);
		assert!(set.files[0].occurrences.iter().all(|o| o.role != Role::Reference));
	}

	/// An import binding routes an external reference to `NudoxPath::External`.
	#[test]
	fn external_import_resolves_to_external_path() {
		let extraction = Extraction {
			definitions: vec![def("hello", DefKind::Function, 0..40, 3..8, None)],
			imports: vec![ImportBinding {
				local: "serde".into(),
				source: ImportSource::External { dependency: "serde".into(), path: vec![] },
				span: 0..0,
			}],
			references: vec![RawReference {
				segments: vec!["serde".into(), "Serialize".into()],
				span: 12..30,
				kind: ReferenceKind::TypeReference,
				receiver: None,
			}],
			module_path: vec![],
		};
		let files = vec![(PathBuf::from("lib.rs"), extraction)];
		let set = resolve(Language::Rust, &files, &index(vec![func("hello", "hello")]));

		let r = set.files[0]
			.occurrences
			.iter()
			.find(|o| o.role == Role::Reference)
			.expect("reference resolved");
		assert_eq!(
			r.target,
			NudoxPath::External { dependency: "serde".into(), path: PathBuf::from("Serialize") }
		);
		assert_eq!(r.confidence, Confidence::Import);
	}

	/// A `crate::`-rooted chain folds against the crate root (the first, crate-
	/// name segment of the module path) — cross-module resolution.
	#[test]
	fn crate_rooted_chain_folds_to_crate_root() {
		// A file in module `mycrate::app` referencing `crate::math::add`.
		let extraction = Extraction {
			definitions: vec![def("run", DefKind::Function, 0..40, 3..6, None)],
			imports: vec![],
			references: vec![RawReference {
				segments: vec!["crate".into(), "math".into(), "add".into()],
				span: 12..30,
				kind: ReferenceKind::FunctionCall,
				receiver: None,
			}],
			module_path: vec!["mycrate".into(), "app".into()],
		};
		let files = vec![(PathBuf::from("app.rs"), extraction)];
		let set = resolve(
			Language::Rust,
			&files,
			&index(vec![func("add", "mycrate::math::add"), func("run", "mycrate::app::run")]),
		);

		let r = set.files[0]
			.occurrences
			.iter()
			.find(|o| o.role == Role::Reference)
			.expect("reference resolved");
		assert_eq!(r.target, local("mycrate::math::add"));
		assert_eq!(r.enclosing, Some(local("mycrate::app::run")));
		assert_eq!(r.confidence, Confidence::Index);
	}
}
