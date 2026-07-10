//! Lowering context: path caches, impl index, stable IDs, cycle guards.

use std::{hash::Hasher, path::PathBuf, sync::Arc};

use ir::{
	entry::NudoxPath,
	kind::{Entry, Symbol, Visibility as IrVisibility},
};
use ra_ap_hir::{
	Adt, Crate, DisplayTarget, HasVisibility, Impl, Module, ModuleDef, ScopeDef, Semantics, Trait,
	Visibility,
};
use ra_ap_base_db::SourceDatabase;
use ra_ap_ide_db::{FileId, RootDatabase, line_index::LineIndex};
use ra_ap_syntax::Edition;
use rustc_hash::{FxHashMap, FxHashSet, FxHasher};
use smol_str::SmolStr;

use super::docs;

/// Version-stable identity for defs — salsa ids are *not* stable across runs.
/// Canonical path string, e.g. `"serde::de::Deserialize"`.
pub(crate) type PathKey = SmolStr;

/// Per-crate lowering state shared across packages in one load.
pub(crate) struct LowerCtx<'db> {
	pub(crate) db: &'db RootDatabase,
	pub(crate) sema: Semantics<'db, RootDatabase>,
	pub(crate) krate: Crate,
	pub(crate) display: DisplayTarget,
	pub(crate) edition: Edition,
	pub(crate) document_private: bool,

	/// Canonical path string → classified [`NudoxPath`].
	pub(crate) path_cache: FxHashMap<PathKey, NudoxPath>,
	/// All public paths per def (re-export aliases, incl. globs).
	pub(crate) alias_cache: FxHashMap<PathKey, FxHashSet<Vec<String>>>,
	/// Trait- and self-ty-bucketed impls for the probe tier.
	pub(crate) impls: ImplIndex,
	/// FileId → LineIndex, lazily built for span/source-map extraction.
	pub(crate) lines: FxHashMap<FileId, Arc<LineIndex>>,

	pub(crate) visiting: FxHashSet<PathKey>,
	pub(crate) cache: FxHashMap<PathKey, Entry>,
}

/// Bucketed impls for inherent methods + trait protocol membership.
#[derive(Default)]
pub(crate) struct ImplIndex {
	/// ADT-keyed local inherent/trait impls (`Impl::all_in_crate`).
	pub(crate) by_self_ty: FxHashMap<PathKey, Vec<Impl>>,
	/// Impls whose self_ty is a bare generic param.
	pub(crate) blanket: Vec<(Trait, Impl)>,
	/// Send, Sync, Unpin, … resolved once per load.
	pub(crate) auto_traits: Vec<Trait>,
}

impl ImplIndex {
	/// Partition `Impl::all_in_crate` into ADT buckets + blanket list.
	///
	/// `adt_key` resolves an ADT to its canonical [`PathKey`] (caller supplies
	/// path identity so this stays free of `LowerCtx` borrow issues).
	pub(crate) fn from_crate(
		db: &RootDatabase,
		krate: Crate,
		mut adt_key: impl FnMut(Adt) -> Option<PathKey>,
	) -> Self {
		let mut index = Self::default();
		for imp in Impl::all_in_crate(db, krate) {
			let self_ty = imp.self_ty(db);

			if let Some(adt) = self_ty.as_adt() {
				if let Some(key) = adt_key(adt) {
					index.by_self_ty.entry(key).or_default().push(imp);
				}
				continue;
			}

			// Bare type-param self_ty → blanket (e.g. `impl<T> Trait for T`).
			if self_ty.as_type_param(db).is_some()
				&& let Some(tr) = imp.trait_(db)
			{
				index.blanket.push((tr, imp));
			}
		}
		index
	}
}

impl<'db> LowerCtx<'db> {
	pub(crate) fn new(
		db: &'db RootDatabase,
		krate: Crate,
		document_private: bool,
	) -> Self {
		let display = krate.to_display_target(db);
		let edition = krate.edition(db);
		Self {
			sema: Semantics::new(db),
			db,
			krate,
			display,
			edition,
			document_private,
			path_cache: FxHashMap::default(),
			alias_cache: FxHashMap::default(),
			impls: ImplIndex::default(),
			lines: FxHashMap::default(),
			visiting: FxHashSet::default(),
			cache: FxHashMap::default(),
		}
	}

	/// Visibility gate: public-only unless `document_private`.
	///
	/// `BuiltinType` is always `Public` in HIR; every other `ModuleDef`
	/// implements `HasVisibility`.
	pub(crate) fn include(&self, def: ModuleDef) -> bool {
		if self.document_private {
			return true;
		}
		matches!(def.visibility(self.db), Visibility::Public)
	}

	/// Map HIR visibility onto IR, matching the rustdoc producer.
	///
	/// | RA | IR |
	/// |---|---|
	/// | `Public` | `Public` |
	/// | `PubCrate` / `Module(crate_root, _)` | `Internal` (`pub(crate)`) |
	/// | other `Module(..)` | `Package` (`pub(super)` / `pub(in …)`) |
	///
	/// Default (private) items are `Module(parent, Implicit)`. At crate root
	/// that collapses to `Internal`; nested private modules become `Package`.
	/// Tests accept `Internal | Private` for private/`pub(crate)` items.
	pub(crate) fn visibility(&self, def: impl HasVisibility) -> IrVisibility {
		match def.visibility(self.db) {
			Visibility::Public => IrVisibility::Public,
			Visibility::PubCrate(_) => IrVisibility::Internal,
			Visibility::Module(m, _) => {
				let module = Module::from(m);
				if module.is_crate_root(self.db) {
					IrVisibility::Internal
				} else {
					IrVisibility::Package
				}
			}
		}
	}

	// ── path helpers ─────────────────────────────────────────────────────────

	/// Canonical path of `def`: defining-module chain + name, `::`-joined.
	///
	/// Crate segment is the rustc name (`odd-duck` → `odd_duck`). Classifies
	/// as [`NudoxPath::Local`] only when the def lives in the currently
	/// lowering crate (`self.krate`); every other crate — including local path
	/// deps — becomes [`NudoxPath::External`] (rustdoc parity when documenting
	/// a single package).
	pub(crate) fn canonical(&mut self, def: ModuleDef) -> Option<PathKey> {
		let segments = self.path_segments(def)?;
		let key = PathKey::from(segments.join("::"));
		if !self.path_cache.contains_key(&key) {
			let nudox = classify_nudox(self.db, self.krate, def, &segments);
			self.path_cache.insert(key.clone(), nudox);
		}
		Some(key)
	}

	/// Cached [`NudoxPath`] for `def` (computes + stores via [`Self::canonical`]).
	pub(crate) fn nudox_path(&mut self, def: ModuleDef) -> Option<NudoxPath> {
		let key = self.canonical(def)?;
		self.path_cache.get(&key).cloned()
	}

	/// Look up a previously-cached NudoxPath by its path key.
	pub(crate) fn path_of(&self, key: &PathKey) -> Option<&NudoxPath> {
		self.path_cache.get(key)
	}

	/// Alias segment vectors for a canonical key (excluding the primary path).
	pub(crate) fn aliases_of(&self, key: &PathKey) -> Option<FxHashSet<Vec<String>>> {
		self.alias_cache.get(key).filter(|s| !s.is_empty()).cloned()
	}

	/// Build the symbol shell used by every entry kind.
	///
	/// Path / aliases / visibility / docs come from the def's defining site;
	/// `inner` is the kind-specific payload.
	pub(crate) fn symbol_shell<T>(
		&mut self,
		def: ModuleDef,
		inner: T,
	) -> Option<Symbol<T>> {
		let name = match def {
			ModuleDef::Module(m) if m.is_crate_root(self.db) => crate_name(self.db, m.krate(self.db)),
			_ => def.name(self.db)?.as_str().to_owned(),
		};
		let key = self.canonical(def)?;
		let path = self.path_cache.get(&key).cloned()?;
		let aliases = self.aliases_of(&key);
		let visibility = self.visibility(def);
		let documentation = docs::documentation(self, def);
		let deprecation = docs::deprecation(self, def);
		let doc_links = docs::doc_links(self, def, documentation.as_deref());
		Some(Symbol {
			name,
			path,
			aliases,
			visibility,
			documentation,
			deprecation,
			doc_links,
			inner,
		})
	}

	/// Convenience when name/path are already known (e.g. impl entries).
	pub(crate) fn symbol_shell_parts<T>(
		name: String,
		path: NudoxPath,
		visibility: IrVisibility,
		documentation: Option<String>,
		aliases: Option<FxHashSet<Vec<String>>>,
		inner: T,
	) -> Symbol<T> {
		Symbol {
			name,
			path,
			aliases,
			visibility,
			documentation,
			deprecation: None,
			doc_links: None,
			inner,
		}
	}

	/// One pass over every local module's `Module::scope` (glob-aware).
	///
	/// Each `(Name, ScopeDef)` whose def's defining path differs from
	/// `module_path::name` is recorded as an alias on that def's canonical key.
	/// `Module::scope` already surfaces glob re-exports (`use foo::*`), so those
	/// alias paths are captured here — strictly better than rustdoc, which skips
	/// globs.
	///
	/// TODO(P4): tag *which* aliases came from a glob (vs a named/renamed
	/// re-export). Not possible on ra_ap 0.0.341: `Module::scope` returns
	/// `(Name, ScopeDef)` and `ScopeDef` carries no import/glob provenance; the
	/// glob-vs-import distinction (`ImportOrGlob`) lives in `hir_def::item_scope`
	/// and is not reachable through the public `ra_ap_hir` surface. Would need a
	/// `hir_def` `DefMap`/`ItemScope` accessor upstream.
	pub(crate) fn collect_aliases(&mut self) {
		let mut stack = vec![self.krate.root_module(self.db)];
		while let Some(module) = stack.pop() {
			stack.extend(module.children(self.db));

			let Some(module_segs) = self.path_segments(ModuleDef::Module(module)) else {
				continue;
			};

			for (name, scope_def) in module.scope(self.db, None) {
				let ScopeDef::ModuleDef(def) = scope_def else {
					continue;
				};
				let Some(canon_key) = self.canonical(def) else {
					continue;
				};

				let mut alias_segs = module_segs.clone();
				alias_segs.push(name.as_str().to_owned());

				// Primary path as segments — skip if this scope entry *is* the def.
				let primary: Vec<String> =
					canon_key.split("::").map(str::to_owned).collect();
				if alias_segs == primary {
					continue;
				}
				self.alias_cache
					.entry(canon_key)
					.or_default()
					.insert(alias_segs);
			}
		}
	}

	/// Bucket local crate impls into `self.impls` (inherent + trait + blanket).
	///
	/// Prefer this over ad-hoc `Impl::all_for_type` — that API excludes blankets.
	pub(crate) fn build_impl_index(&mut self) {
		// Collect first so we can mutably resolve ADT paths without fighting
		// the borrow on `self.impls`.
		let impls: Vec<Impl> = Impl::all_in_crate(self.db, self.krate);
		let mut by_self_ty: FxHashMap<PathKey, Vec<Impl>> = FxHashMap::default();
		let mut blanket: Vec<(Trait, Impl)> = Vec::new();

		for imp in impls {
			let self_ty = imp.self_ty(self.db);

			if let Some(adt) = self_ty.as_adt() {
				if let Some(key) = self.canonical(ModuleDef::Adt(adt)) {
					by_self_ty.entry(key).or_default().push(imp);
				}
				continue;
			}

			if self_ty.as_type_param(self.db).is_some()
				&& let Some(tr) = imp.trait_(self.db)
			{
				blanket.push((tr, imp));
			}
		}

		self.impls.by_self_ty = by_self_ty;
		self.impls.blanket = blanket;
	}

	/// Lazy `LineIndex` for `file_id` (via analysis db file text).
	///
	/// Builds with `std::sync::Arc` (not triomphe's Arc from `ide_db::line_index`).
	pub(crate) fn line_index(&mut self, file_id: FileId) -> Option<Arc<LineIndex>> {
		if let Some(li) = self.lines.get(&file_id) {
			return Some(li.clone());
		}
		let text = self.db.file_text(file_id).text(self.db);
		let li = Arc::new(LineIndex::new(text));
		self.lines.insert(file_id, li.clone());
		Some(li)
	}

	/// Defining-module path segments for `def`, crate name first.
	fn path_segments(&self, def: ModuleDef) -> Option<Vec<String>> {
		// Crate root: single segment = rustc crate name.
		if let ModuleDef::Module(m) = def
			&& m.is_crate_root(self.db)
		{
			return Some(vec![crate_name(self.db, m.krate(self.db))]);
		}

		// Builtins have no module; path is just the type name (`i32`, …).
		if let ModuleDef::BuiltinType(b) = def {
			return Some(vec![b.name().as_str().to_owned()]);
		}

		let mut segs = Vec::new();
		segs.push(def.name(self.db)?.as_str().to_owned());

		// Containing module chain (root last). Root has no `Name` — inject crate.
		let containing = def.module(self.db)?;
		for m in containing.path_to_root(self.db) {
			if let Some(n) = m.name(self.db) {
				segs.push(n.as_str().to_owned());
			} else if m.is_crate_root(self.db) {
				segs.push(crate_name(self.db, m.krate(self.db)));
			}
		}
		segs.reverse();
		Some(segs)
	}
}

/// Deterministic `Function.type_links` id: Fx hash of the canonical path.
pub(crate) fn stable_id(path: &PathKey) -> i64 {
	let mut hasher = FxHasher::default();
	hasher.write(path.as_str().as_bytes());
	hasher.finish() as i64
}

/// Rustc / display crate name (`odd-duck` package → `odd_duck`).
pub(crate) fn crate_name(db: &RootDatabase, krate: Crate) -> String {
	krate
		.display_name(db)
		.map(|n| n.to_string())
		.unwrap_or_else(|| "_".into())
}

/// `Local` only for defs in `current` (the crate being lowered); everything
/// else is `External` — including `CrateOrigin::Local` path dependencies.
///
/// External paths drop the leading crate segment so rustdoc's shape is matched
/// (`helper::Marker` → `External { dependency: "helper", path: "Marker" }`).
fn classify_nudox(
	db: &RootDatabase,
	current: Crate,
	def: ModuleDef,
	segments: &[String],
) -> NudoxPath {
	let joined = segments.join("::");
	let Some(krate) = def_krate(db, def) else {
		// Builtins / no crate: treat as Local (e.g. `i32`).
		return NudoxPath::Local(PathBuf::from(joined));
	};

	if krate == current {
		return NudoxPath::Local(PathBuf::from(joined));
	}

	let dependency = crate_name(db, krate);
	let relative = match segments {
		[head, rest @ ..] if *head == dependency => rest.join("::"),
		other => other.join("::"),
	};
	NudoxPath::External {
		dependency,
		path: PathBuf::from(relative),
	}
}

fn def_krate(db: &RootDatabase, def: ModuleDef) -> Option<Crate> {
	match def {
		ModuleDef::Module(m) => Some(m.krate(db)),
		ModuleDef::BuiltinType(_) => None,
		other => other.module(db).map(|m| m.krate(db)),
	}
}
