# Source: user prompt — target IR rewrite (authoritative for VCS design)

> **Status (Rev 3.2):** Authoritative for the Entry/Node/Kind/Registry/EntryBuilder model, but a **POC snapshot**. `design/IR-NATIVE-VCS-DESIGN.md` specifies the production deltas: fallible resolver methods (`Result<_, ResolveError>` instead of panics), `PackageLineageId` in place of the path-based `PackageId`, explicit append-only `u16` values on `KindDiscriminant`, the full-parity `Symbol` (aliases/deprecation/doc_links), and `StableRef` for cross-package wire references. Where this dump and the design disagree, the design wins.

# nudox IR rewrite
	
	## Core Improvements
	
	### Entry Rework
	
	`Entry` goes from being a simple enum owning `Symbol<T>` to a proper structure storing three things:
	
	- `Symbol`: always-present IR metadata (name, visibility, docs, source location + span)
	- `Node`: IR tree information (parent & child pointers). these are automatically tracked.
	- `EntryInner`: either a direct `Kind` (used for most entries) or a `RawEntryIdx` reference to
	  another entry. This enables a representation for re-exports both within and between packages. Since
	  `RawEntryIdx` points to an entire `Entry`, this means that re-exports also track their own metadata
	  (parent location, `Symbol` name, visbility, etc).
	
	### Nested Types -> Arenas w/ Indicies
	
	Migrates from nested types to arenas with indicies to reference other entries.
	
	For example:
	
	#### OLD
	
	```rust
	pub struct Record {
		pub methods: Vec<Function>,
		// ...
	}
	```
	
	#### NEW
	
	```rust
	pub struct Record {
		pub methods: Vec<EntryIdx<Function>>,
		// ...
	}
	```
	
	This enables a few key things:
	
	1. we don't have to build the entire function multiple times.
	2. we get a full `Symbol` and an entire `Entry` instead of just what exists on `Function`
	3. we can deduplicate data within the IR (e.g., `Function` and `Record` do not need to own identifying
	   symbol information, like a name).
	
	This change drives the majority of the IR improvements.
	
	### Deduplicating IR
	
	As mentioned above, we no longer need to track identifying information for each entry `Kind`.
	
	### Refer to an IR Entry
	
	Previously, there existed the `NudoxPath` type to attempt to identify the location of an IR entry.
	This is now superseded by a `RawEntryIdx`, which is semantically an untyped `EntryIdx`, and can refer
	to any `Entry` within an entire `Registry`'s IR.
	
	## New Features
	
	### Registry-aware IR
	
	The IR is now aware of the concept of a `Registry` that holds multiple packages.
	
	A `Registry` allows the IR builder to build an identifier to create a reference to a externally
	defined `Entry` (with `Registry::EntryId`), which the registry can resolve to an `EntryIdx` internally.
	
	> [!NOTE]
	> The currently implemented `Registry` trait abstraction is primarily a POC, has not been tested
	> with any compiler frontends, and will probably need to be redesigned to some extent. It exists
	> primarily as a way to create an initial outline of some of the expected design.
	
	My concept for the `Registry` essentially is an `EntryArena`-per-package model. Possibly with the
	`Registry` implementor asynchronously or lazily resolving (possibly pre-existing) IR for packages
	as it's needed. This could also possibly mean re-designing such that we have a `RegistryResolver`
	and a single owned generic `Registry<T: RegistryResolver>` type to unify the operations that would
	be shared between all regisitry implementors in the current model, delegating to the resolver where
	needed.
	
	> [!NOTE]
	> we can consider the pros and cons of making `Registry`/`RegistryResolver` `async`.
	> If it *is* async, that will likely be propogated to operations interacting with the
	> IR (currently `TypedEntry::kind` and `TypedEntry::get` accessors).
	
	### IR builder
	
	The IR can now be constructed in a tree-like fashion, using `EntryArena::create_top_level`. This serves
	as the entrypoint for creating an IR. It exposes a `build: impl FnOnce(&mut EntryBuilder) -> T` to
	actually build the IR entry. `EntryBuilder` handles recursively building the IR entries, and
	automatically handles the parent-child relationship graph internally.
	
	> [!NOTE]
	> The entrypoint for building an IR probably needs to be re-evaluated. This should be done together
	> with starting the porting of the compiler frontends to design an API that's easy and convenient.
	
	#### EntryBuilder Public API
	
	```rust
	impl EntryBuilder {
		/// Create a new child entry and recieve an EntryIdx for it
		///
		/// This is the public API for building entries.
		pub fn create<T>(&mut self, sym: Symbol, build: impl FnOnce(&mut Self) -> T) -> EntryIdx<T>
		where
			T: EntryKind;
	
		/// Emits a link between the currently-being-built entry and another entry.
		pub fn link<T>(&mut self, idx: EntryIdx<T>)
		where
			T: EntryKind;
	
		/// Emits a link between two entry
		pub fn link_between<T, U>(&mut self, a: EntryIdx<T>, b: EntryIdx<U>)
		where
			T: EntryKind,
			U: EntryKind;
	}
	```
	
	`EntryBuilder` also allows emitting links between entries, turing the IR into a proper graph.
	
	### IR is a proper graph
	
	`EntryBuilder` also tracks links between types.
	
	Links between IR entries are tracked storing both the raw index and the kind of the entry. This
	means that we have the capability to query relationships between IR entries.
	
	Currently these must be emitted through `EntryBuilder` with `link` and `link_between`, but if IR
	builder APIs are introduced, these could be handled automatically under the hood.
	
	### Kind Enum Implementation
	
	The `Kind` enum is now generated using the `register_kinds` macro. It accepts optional doc comments,
	and generates an enum entry matching the name of the registered kind (e.g., `Record` generates `Record(Record)`).
	
	See [`kind.rs`](crates/nudox-ir/src/kind.rs)
	
	The `register_kinds` macro also generates a `KindDescriminant` enum which can represent the type of
	a `Kind` without the associated data.
	
	Finally, `register_kinds` also handles implementing the new `EntryKind` trait, which powers most of
	the generic type parameters.
	
	```rust
	pub(crate) trait EntryKind: Any {
		fn into_kind(self) -> Kind;
		fn discriminant() -> KindDiscriminant;
	}
	```
	
	This ensures that any `Kind` variant that exists implements this trait, and allows us to get a
	`Kind` enum from said variant, as well as get the discriminant of any kind variant `T`.
	
	## Not Yet Implemented
	
	- Most of the actual IR `Kind`'s have not yet been ported.
	- IR kind builder APIs have not been designed at all.
	- Emitting re-export references
20
crates/nudox-ir/Cargo.toml
Normal file
	@ -0,0 +1,20 @@
	[package]
	name = "nudox-ir"
	version = "0.1.0"
	edition.workspace = true
	license.workspace = true
	
	[dependencies]
	bimap = { workspace = true }
	bon = { workspace = true }
	elsa = { workspace = true }
	erased-serde = { workspace = true }
	parking_lot = { workspace = true }
	rustc-hash = { workspace = true }
	serde = { workspace = true }
	serde_context = { workspace = true }
	triomphe = { workspace = true }
	
	[dev-dependencies]
	itertools = { workspace = true }
	serde_json = { workspace = true }
28
crates/nudox-ir/src/entry.rs
Normal file
	@ -0,0 +1,28 @@
	mod node;
	mod typed;
	
	pub use self::{node::Node, typed::TypedEntry};
	use crate::{kind::Kind, registry::RawEntryIdx, symbol::Symbol};
	
	#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub struct Entry {
		pub sym:  Symbol,
		pub node: Node,
		pub kind: EntryInner,
	}
	impl Entry {
		pub(crate) const fn new(sym: Symbol, node: Node, kind: Kind) -> Self {
			Self { sym, node, kind: EntryInner::Owned(kind) }
		}
	
		#[expect(unused)] // TODO: test and actually use
		pub(crate) const fn reference(sym: Symbol, node: Node, idx: RawEntryIdx) -> Self {
			Self { sym, node, kind: EntryInner::Reference(idx) }
		}
	}
	
	#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub enum EntryInner {
		Owned(Kind),
		Reference(RawEntryIdx),
	}
26
crates/nudox-ir/src/entry/node.rs
Normal file
	@ -0,0 +1,26 @@
	use crate::{List, registry::RawEntryIdx};
	
	#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub struct Node {
		pub parent:   Option<RawEntryIdx>,
		pub children: List<RawEntryIdx>,
	}
	
	impl Node {
		pub fn new(parent: RawEntryIdx, children: impl IntoIterator<Item = RawEntryIdx>) -> Self {
			Self::build(Some(parent), children)
		}
	
		pub fn root(children: impl IntoIterator<Item = RawEntryIdx>) -> Self {
			Self::build(None, children)
		}
	
		pub fn leaf(parent: RawEntryIdx) -> Self { Self::build(Some(parent), []) }
	
		pub fn build(
			parent: Option<RawEntryIdx>,
			children: impl IntoIterator<Item = RawEntryIdx>,
		) -> Self {
			Node { parent, children: children.into_iter().collect() }
		}
	}
57
crates/nudox-ir/src/entry/typed.rs
Normal file
	@ -0,0 +1,57 @@
	use std::marker::PhantomData;
	
	use super::{Entry, EntryInner};
	use crate::{kind::{EntryKind, Kind}, registry::{Registry, RegistryResolver}, symbol::Symbol};
	
	#[repr(transparent)]
	pub struct TypedEntry<T> {
		inner: Entry,
		_p:    PhantomData<T>,
	}
	
	impl<T> TypedEntry<T> {
		/// the raw (untyped) inner entry
		pub fn entry(&self) -> &Entry { &self.inner }
	
		/// the entry's symbol
		pub fn sym(&self) -> &Symbol { &self.inner.sym }
	}
	
	impl<T: EntryKind> TypedEntry<T> {
		pub(crate) fn new(entry: &Entry) -> &Self {
			// Safety: `TypedEntry` is `repr(transparent)` and there are no possibilities
			// for UB as all operations are checked before accessing the inner Kind
			// variant regardless
			unsafe { &*std::ptr::from_ref(entry).cast() }
		}
	
		/// the entry's raw kind enum
		pub fn kind<'a>(&'a self, r: &'a Registry<impl RegistryResolver>) -> &'a Kind {
			match &self.inner.kind {
				EntryInner::Owned(kind) => kind,
				EntryInner::Reference(idx) => r.resolve_typed(idx.typed::<T>()).kind(r),
			}
		}
	
		// TODO: do we want to impl Deref and make this more like a smart pointer?
		pub fn get<'a>(&'a self, r: &'a Registry<impl RegistryResolver>) -> &'a T {
			self.kind(r).variant_as_dyn().downcast_ref().expect("using TypedEntry with incorrect type")
		}
	}
	
	#[cfg(test)]
	mod tests {
		use super::*;
		use crate::test_helpers::*;
	
		#[test]
		fn get_allows_typed_access() {
			let registry = dummy_registry();
	
			let entry = entry("test_sym", n::root(vec![]), Module);
	
			let entry = TypedEntry::new(&entry);
	
			let _module: &Module = entry.get(&registry);
		}
	}
11
crates/nudox-ir/src/function.rs
Normal file
	@ -0,0 +1,11 @@
	use crate::List;
	
	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub struct Function {
		pub input_params:  List<Param>,
		pub output_params: List<Param>,
		// TODO: rest
	}
	
	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub enum Param {}
73
crates/nudox-ir/src/kind.rs
Normal file
	@ -0,0 +1,73 @@
	use std::any::Any;
	
	use crate::{function::Function, module::Module, record::{Field, Record}, ty::Type};
	
	register_kinds! {
		/// A namespace, package, or module — a container for other entries.
		Module,
	
		/// A product type: struct, class, record, or data class.
		Record,
	
		Field,
	
		Function,
	
		Type,
	}
	
	pub trait EntryKind: Any + private::Sealed {
		fn into_kind(self) -> Kind
		where
			Self: Sized;
	
		fn discriminant() -> KindDiscriminant
		where
			Self: Sized;
	}
	
	mod private {
		pub trait Sealed {}
	}
	
	macro_rules! register_kinds {
		($($(#[$meta:meta])* $kind:ident,)*) => {
			#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
			pub enum Kind {
				$(
				$(#[$meta])*
				$kind($kind),
				)*
			}
	
			impl Kind {
				pub(crate) fn variant_as_dyn(&self) -> &dyn Any {
					match self {
						$(
						Kind::$kind(it) => it,
						)*
					}
				}
			}
	
			#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
			pub enum KindDiscriminant {
				$(
				$(#[$meta])*
				$kind,
				)*
			}
	
			$(
			impl private::Sealed for $kind {}
	
			impl EntryKind for $kind {
				fn into_kind(self) -> Kind { Kind::$kind(self) }
	
				fn discriminant() -> KindDiscriminant { KindDiscriminant::$kind }
			}
			)*
		};
	}
	
	use register_kinds;
25
crates/nudox-ir/src/lib.rs
Normal file
	@ -0,0 +1,25 @@
	#![feature(trait_alias)]
	
	pub mod entry;
	pub mod function;
	pub mod kind;
	pub mod package;
	pub mod primitive;
	pub mod record;
	pub mod registry;
	pub mod symbol;
	pub mod ty;
	
	pub mod module {
		#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
		pub struct Module;
	}
	
	pub type List<T> = Box<[T]>;
	
	#[cfg(test)]
	mod test_helpers;
	// pub mod generics;
	// pub mod parameter;
	// pub mod protocols;
	// pub mod syntax;
43
crates/nudox-ir/src/package.rs
Normal file
	@ -0,0 +1,43 @@
	use std::{fmt, path::{Path, PathBuf}};
	
	use triomphe::Arc;
	
	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
	pub enum PackageIdView<'a> {
		Path(&'a Path),
	}
	
	#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
	pub struct PackageId {
		repr: Repr,
	}
	
	impl PackageId {
		pub fn path(path: impl AsRef<Path>) -> Self { PackageId { repr: Repr::path(path.as_ref()) } }
	
		pub fn view(&self) -> PackageIdView<'_> { self.repr.view() }
	}
	
	impl fmt::Debug for PackageId {
		fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { fmt::Debug::fmt(&self.view(), f) }
	}
	
	#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
	struct Repr {
		inner: Arc<ReprInner>,
	}
	
	#[derive(Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
	enum ReprInner {
		Path(PathBuf),
	}
	
	impl Repr {
		fn path(path: &Path) -> Self { Repr { inner: Arc::new(ReprInner::Path(path.to_path_buf())) } }
	
		fn view(&self) -> PackageIdView<'_> {
			match self.inner.as_ref() {
				ReprInner::Path(path) => PackageIdView::Path(path),
			}
		}
	}
74
crates/nudox-ir/src/primitive.rs
Normal file
	@ -0,0 +1,74 @@
	use crate::{registry::EntryIdx, ty::Type};
	
	/// A language-level primitive type, independent of any target architecture.
	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub enum Primitive {
		Integer {
			signed: bool,
			width:  Width,
		},
	
		Float(Width),
	
		Bool,
	
		Char,
	
		/// Type of a string literal (if it exists)
		///
		/// Note that this is specifically for primitive types, so this should be
		/// equivalent to `str` in Rust or `string` in C#, not Rust's `String` or
		/// C++'s `std::string`.
		// TODO: should rust string literals resolve to BorrowedRef then?
		// TODO: should C/C++ string literals resolve to `char*` or `char[]` instead?
		Str,
	
		/// A raw, mutable, unmanaged pointer.
		/// Ex: `*mut T`, `int*`.
		MutPointer(EntryIdx<Type>),
	
		/// A raw, const, unmanaged pointer.
		/// Ex: `*const T`, `int *const`.
		ConstPointer(EntryIdx<Type>),
	
		/// A managed reference with optional lifetime/mutability tracking.
		/// Ex: `&'a mut T`.
		Reference {
			lifetime: Option<String>,
			mutable:  bool,
			ty:       EntryIdx<Type>,
		},
	
		/// An arbitrary primtive type, e.g. Date in JavaScript/TypeScript
		Builtin(String),
	}
	
	/// A language-level primitive type, independent of any target architecture.
	#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub enum Width {
		Fixed(usize),
	
		/// Machine-dependent / pointer-sized (e.g., `usize`, `isize`).
		/// Generally not applicable to Floats
		Arch,
	}
	
	impl Width {
		/// 8-bit width
		pub const W8: Self = Width::Fixed(8);
	
		/// 16-bit width
		pub const W16: Self = Width::Fixed(16);
	
		/// 32-bit width
		pub const W32: Self = Width::Fixed(32);
	
		/// 64-bit width
		pub const W64: Self = Width::Fixed(64);
	
		/// 80-bit width
		pub const W80: Self = Width::Fixed(80);
	
		/// 128-bit width
		pub const W128: Self = Width::Fixed(128);
	}
50
crates/nudox-ir/src/record.rs
Normal file
	@ -0,0 +1,50 @@
	use crate::{List, registry::{EntryBuilder, EntryIdx}};
	
	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub struct Record {
		pub fields: List<EntryIdx<Field>>,
	}
	
	#[bon::bon]
	impl Record {
		#[builder(finish_fn(name = finish, vis = ""))]
		pub fn new(#[builder(with = FromIterator::from_iter)] fields: List<EntryIdx<Field>>) -> Self {
			Record { fields }
		}
	}
	
	impl<S: record_builder::IsComplete> RecordBuilder<S> {
		pub fn build(self, b: &mut EntryBuilder) -> Record {
			let record = self.finish();
	
			b.link_many(record.fields.iter().copied());
	
			record
		}
	}
	
	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub struct Field {
		// TODO
	}
	
	// this serves as an example of the intended usage of the builder APIs.
	// note a few key points from the snippets above:
	//
	// 1. `#[builder(finish_fn(name = finish, vis = ""))]`:
	//
	// this makes the `finish` fn private, meaning that anyone using the builder
	// API can _only_ finish building via a method that _we_ explcitily control
	//
	// 2. `RecordBuilder::<S>::build`
	//
	// we expose a public `build` method on the RecordBuilder's completed state,
	// allowing the user to instantiate the `Record` by calling our provided method.
	//
	// this allows us to run the custom code after we call `finish` ourselves, which
	// is what enables us to guarentee that we emit IR links if the builder API is
	// used.
	
	fn _example_builder_api_usage(b: &mut EntryBuilder) -> Record {
		Record::builder().fields([]).build(b)
	}
77
crates/nudox-ir/src/registry.rs
Normal file
	@ -0,0 +1,77 @@
	mod arena;
	mod builder;
	mod idx;
	mod link;
	mod resolver;
	mod state;
	
	#[cfg(test)]
	mod tests;
	
	use crate::{entry::{Entry, TypedEntry}, kind::EntryKind, module::Module, package::PackageId, symbol::Symbol};
	
	use self::{arena::EntryArena, resolver::DynRegistryResolver};
	
	// reexport at pub(crate) level to allow test_helpers to use
	#[cfg(test)]
	pub(crate) use self::idx::{ArenaIdx, PackageIdx};
	
	#[cfg(not(test))]
	use self::idx::{ArenaIdx, PackageIdx};
	
	pub use self::{builder::EntryBuilder, idx::{EntryIdx, RawEntryIdx}, link::EntryLink, resolver::{EntryId, RegistryResolver}, state::RegistryState};
	
	pub struct Registry<R> {
		resolver: R,
		state:    RegistryState,
	}
	
	impl<R> Registry<R> {
		pub fn new(resolver: R) -> Self { Registry { resolver, state: RegistryState::new() } }
	}
	
	impl<R: RegistryResolver> Registry<R> {
		pub fn serialize<T, S>(&self, serializer: S, it: &T) -> Result<S::Ok, S::Error>
		where
			T: serde::Serialize,
			S: serde::Serializer,
		{
			serde_context::serialize_with_context(
				it,
				serializer,
				(&self.resolver as &dyn DynRegistryResolver, &self.state),
			)
		}
	
		pub fn deserialize<'de, T, D>(&self, deserializer: D) -> Result<T, D::Error>
		where
			T: serde::Deserialize<'de>,
			D: serde::Deserializer<'de>,
		{
			serde_context::deserialize_with_context(
				deserializer,
				(&self.resolver as &dyn DynRegistryResolver, &self.state),
			)
		}
	
		pub fn resolve(&self, idx: RawEntryIdx) -> &Entry { self.state.resolve(idx) }
	
		pub fn resolve_typed<T: EntryKind>(&self, idx: EntryIdx<T>) -> &TypedEntry<T> {
			self.state.resolve_typed(idx)
		}
	
		pub fn build_package_ir(
			&mut self,
			package: PackageId,
			sym: Symbol,
			build: impl FnOnce(&mut EntryBuilder),
		) -> EntryIdx<Module> {
			self.state.build_package_ir::<R>(package, sym, build)
		}
	}
	
	impl<T: EntryKind, R: RegistryResolver> std::ops::Index<EntryIdx<T>> for Registry<R> {
		type Output = TypedEntry<T>;
	
		fn index(&self, index: EntryIdx<T>) -> &Self::Output { self.resolve_typed(index) }
	}
14
crates/nudox-ir/src/registry/arena.rs
Normal file
	@ -0,0 +1,14 @@
	use super::ArenaIdx;
	use crate::entry::Entry;
	
	pub(super) struct EntryArena {
		entries: Vec<Entry>,
	}
	
	impl EntryArena {
		pub(super) fn new(entries: Vec<Entry>) -> Self { EntryArena { entries } }
	
		pub(super) fn entry(&self, index: ArenaIdx) -> &Entry { &self.entries[index.index()] }
	
		pub(super) fn iter(&self) -> impl Iterator<Item = &Entry> { self.entries.iter() }
	}
167
crates/nudox-ir/src/registry/builder.rs
Normal file
	@ -0,0 +1,167 @@
	use super::{EntryIdx, EntryLink, RawEntryIdx};
	
	use crate::{entry::{Entry, Node}, kind::{EntryKind, KindDiscriminant}, symbol::Symbol};
	
	pub struct EntryBuilder {
		kind: KindDiscriminant,
	
		idx:      RawEntryIdx,
		next_idx: RawEntryIdx,
	
		entries: Vec<Entry>,
		links:   Vec<EntryLink>,
	}
	
	impl EntryBuilder {
		/// Create a new child entry and recieve an EntryIdx for it
		///
		/// This is the public API for building entries.
		pub fn create<T>(&mut self, sym: Symbol, build: impl FnOnce(&mut Self) -> T) -> EntryIdx<T>
		where
			T: EntryKind,
		{
			let (entries, links) = Self::build(sym, self.next_idx, Some(self.idx), build);
	
			let idx = self.next_idx.typed();
	
			self.next_idx = self.next_idx.inc_arena_idx(entries.len());
	
			self.entries.extend(entries);
			self.links.extend(links.iter());
	
			idx
		}
	
		/// Emits a link between the currently-being-built entry and another entry.
		pub fn link<T>(&mut self, idx: EntryIdx<T>)
		where
			T: EntryKind,
		{
			let link = EntryLink::new((self.idx, self.kind), (idx.into(), T::discriminant()));
			self.links.push(link);
		}
	
		/// Emits links between the currently-being-built entry and the entries
		/// provided
		pub fn link_many<T>(&mut self, entries: impl IntoIterator<Item = EntryIdx<T>>)
		where
			T: EntryKind,
		{
			for idx in entries {
				self.link(idx);
			}
		}
	
		/// Emits a link between two entry
		pub fn link_between<T, U>(&mut self, a: EntryIdx<T>, b: EntryIdx<U>)
		where
			T: EntryKind,
			U: EntryKind,
		{
			self.links.push(EntryLink::typed(a, b));
		}
	}
	
	impl EntryBuilder {
		pub(super) fn build<T>(
			sym: Symbol,
			idx: RawEntryIdx,
			parent: Option<RawEntryIdx>,
			build: impl FnOnce(&mut Self) -> T,
		) -> (Vec<Entry>, Vec<EntryLink>)
		where
			T: EntryKind,
		{
			let mut this = EntryBuilder {
				kind: T::discriminant(),
				idx,
				next_idx: idx.inc_arena_idx(1),
				entries: Vec::new(),
				links: Vec::new(),
			};
	
			let kind = build(&mut this).into_kind();
	
			let EntryBuilder { mut entries, links, .. } = this;
	
			let node = Node::build(parent, (1..=entries.len()).map(|i| idx.inc_arena_idx(i)));
	
			entries.insert(0, Entry::new(sym, node, kind));
	
			(entries, links)
		}
	}
	
	#[cfg(test)]
	mod tests {
		use super::*;
		use crate::test_helpers::*;
	
		fn build<T>(
			index: RawEntryIdx,
			sym: &str,
			build: impl FnOnce(&mut EntryBuilder) -> T,
		) -> (Vec<Entry>, Vec<EntryLink>)
		where
			T: EntryKind,
		{
			EntryBuilder::build(dummy_symbol(sym), index, None, build)
		}
	
		#[test]
		fn built_entries_single_entry_indices() {
			let (built, _links) = build(idx(0x100), "mod42", |_| Module);
	
			itertools::assert_equal(built, [entry("mod42", n::root(vec![]), Module)]);
		}
	
		#[test]
		fn built_entries_with_children() {
			let (built, _links) = build(idx(0x10), "struct67", |b| {
				let fields = ["field1", "field2", "field3"]
					.map(dummy_symbol)
					.map(|field| b.create(field, |_| Field {}))
					.into_iter()
					.collect();
	
				Record { fields }
			});
	
			itertools::assert_equal(built, [
				entry("struct67", n::root(vec![idx(0x11), idx(0x12), idx(0x13)]), Record {
					fields: slice![idx(0x11), idx(0x12), idx(0x13)],
				}),
				entry("field1", n::leaf(idx(0x10)), Field {}),
				entry("field2", n::leaf(idx(0x10)), Field {}),
				entry("field3", n::leaf(idx(0x10)), Field {}),
			]);
		}
	
		#[test]
		fn links_emitted_correctly() {
			let (_entries, links) = build(idx(0), "root", |b| {
				let struct_idx_1 = b.create(dummy_symbol("struct1"), |b| {
					let f1 = b.create(dummy_symbol("f1"), |_| Field {});
					b.link(f1);
	
					Record { fields: slice![f1] }
				});
	
				let struct_idx_2 = b.create(dummy_symbol("struct2"), |_| Record { fields: slice![] });
	
				b.link(struct_idx_1);
				b.link(struct_idx_2);
	
				b.link_between(struct_idx_1, struct_idx_2);
	
				Module
			});
	
			itertools::assert_equal(links, [
				EntryLink::typed(idx::<Record>(1), idx::<Field>(2)),
				EntryLink::typed(idx::<Module>(0), idx::<Record>(1)),
				EntryLink::typed(idx::<Module>(0), idx::<Record>(3)),
				EntryLink::typed(idx::<Record>(1), idx::<Record>(3)),
			]);
		}
	}
165
crates/nudox-ir/src/registry/idx.rs
Normal file
	@ -0,0 +1,165 @@
	use std::{fmt, hash, marker::PhantomData};
	
	use crate::kind::EntryKind;
	
	use super::{DynRegistryResolver, RegistryState};
	
	pub struct EntryIdx<T> {
		package_idx: PackageIdx,
		arena_idx:   ArenaIdx,
		_p:          PhantomData<fn() -> T>,
	}
	
	impl<T> EntryIdx<T> {
		pub(crate) fn new(package_idx: PackageIdx, arena_idx: ArenaIdx) -> Self {
			EntryIdx { package_idx, arena_idx, _p: PhantomData }
		}
	
		pub(crate) fn package_idx(self) -> PackageIdx { self.package_idx }
		pub(crate) fn arena_idx(self) -> ArenaIdx { self.arena_idx }
	
		pub(crate) fn raw(self) -> RawEntryIdx {
			EntryIdx {
				package_idx: self.package_idx,
				arena_idx:   self.arena_idx,
				_p:          PhantomData,
			}
		}
	
		pub(crate) fn typed<U>(self) -> EntryIdx<U> {
			EntryIdx {
				package_idx: self.package_idx,
				arena_idx:   self.arena_idx,
				_p:          PhantomData,
			}
		}
	}
	
	impl<T> serde::Serialize for EntryIdx<T> {
		fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
		where
			S: serde::Serializer,
		{
			use serde::ser::Error;
	
			serde_context::context_scope(|cx| {
				let registry = cx.get::<dyn DynRegistryResolver>().map_err(S::Error::custom)?;
				let state = cx.get::<RegistryState>().map_err(S::Error::custom)?;
	
				let value = registry.__idx_to_entry_id(self.raw(), state);
	
				erased_serde::serialize(&value, serializer)
			})
		}
	}
	
	impl<'de, T> serde::Deserialize<'de> for EntryIdx<T> {
		fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
		where
			D: serde::Deserializer<'de>,
		{
			use serde::de::Error;
	
			serde_context::context_scope(|cx| {
				let resolver = cx.get::<dyn DynRegistryResolver>().map_err(D::Error::custom)?;
				let state = cx.get::<RegistryState>().map_err(D::Error::custom)?;
	
				// TODO: investigate if there's a better way to do this, so that we don't have
				// to use D::Error::custom.
				let idx = resolver
					.__deser_entry_id_to_idx(&mut <dyn erased_serde::Deserializer>::erase(deserializer), state)
					.map_err(D::Error::custom)?;
	
				Ok(idx.typed())
			})
		}
	}
	
	impl<T> Clone for EntryIdx<T> {
		fn clone(&self) -> Self { *self }
	}
	
	impl<T> Copy for EntryIdx<T> {}
	
	impl<T> fmt::Debug for EntryIdx<T> {
		fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
			f.debug_struct("EntryIdx")
				.field("package", &self.package_idx)
				.field("index", &self.arena_idx)
				.finish()
		}
	}
	
	impl<T> PartialEq for EntryIdx<T> {
		fn eq(&self, other: &Self) -> bool {
			self.package_idx == other.package_idx && self.arena_idx == other.arena_idx
		}
	}
	
	impl<T> Eq for EntryIdx<T> {}
	
	impl<T> PartialOrd for EntryIdx<T> {
		fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
	}
	
	impl<T> Ord for EntryIdx<T> {
		fn cmp(&self, other: &Self) -> std::cmp::Ordering {
			self.package_idx.cmp(&other.package_idx).then(self.arena_idx.cmp(&other.arena_idx))
		}
	}
	
	impl<T> hash::Hash for EntryIdx<T> {
		fn hash<H: hash::Hasher>(&self, state: &mut H) {
			self.package_idx.hash(state);
			self.arena_idx.hash(state);
		}
	}
	
	pub type RawEntryIdx = EntryIdx<private::UntypedMarker>;
	
	impl RawEntryIdx {
		pub(super) fn inc_arena_idx(self, amount: usize) -> Self {
			let arena_idx = ArenaIdx::new(self.arena_idx.index() + amount);
	
			RawEntryIdx { arena_idx, ..self }
		}
	}
	
	// allow converting to a RawEntryIdx from any typed EntryIdx
	impl<T: EntryKind> From<EntryIdx<T>> for RawEntryIdx {
		fn from(idx: EntryIdx<T>) -> Self { idx.raw() }
	}
	
	mod private {
		pub struct UntypedMarker;
	}
	
	index_newtype!(PackageIdx);
	index_newtype!(ArenaIdx);
	
	macro_rules! index_newtype {
		($index:ident) => {
			#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
			pub(crate) struct $index {
				index: u32,
			}
	
			impl $index {
				#[cfg_attr(not(test), allow(unused))]
				pub(crate) fn new(index: usize) -> Self {
					debug_assert!(u32::try_from(index).is_ok());
					Self { index: index as u32 }
				}
	
				pub(crate) fn index(self) -> usize { self.index as usize }
			}
	
			impl std::fmt::Debug for $index {
				fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
					f.debug_tuple(stringify!($index)).field(&self.index).finish()
				}
			}
		};
	}
	
	use index_newtype;
28
crates/nudox-ir/src/registry/link.rs
Normal file
	@ -0,0 +1,28 @@
	use crate::{kind::{EntryKind, KindDiscriminant}, registry::RawEntryIdx};
	
	use super::EntryIdx;
	
	pub type LinkVertex = (RawEntryIdx, KindDiscriminant);
	
	// TODO: figure out how to make this cleanly two-way?
	//       or decide if we support directed edges
	#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
	pub struct EntryLink {
		inner: [(RawEntryIdx, KindDiscriminant); 2],
	}
	
	impl EntryLink {
		pub(crate) fn new(a: LinkVertex, b: LinkVertex) -> Self {
			let mut inner = [a, b];
			inner.sort_by_key(|&(idx, _)| idx);
			EntryLink { inner }
		}
	
		pub(crate) fn typed<T, U>(a: EntryIdx<T>, b: EntryIdx<U>) -> Self
		where
			T: EntryKind,
			U: EntryKind,
		{
			Self::new((a.raw(), T::discriminant()), (b.raw(), U::discriminant()))
		}
	}
70
crates/nudox-ir/src/registry/resolver.rs
Normal file
	@ -0,0 +1,70 @@
	use std::any::Any;
	
	use super::{RawEntryIdx, RegistryState};
	
	pub(super) use self::private::DynRegistryResolver;
	
	pub trait EntryId = serde::Serialize + serde::de::DeserializeOwned + Any;
	
	pub trait RegistryResolver: DynRegistryResolver {
		/// A type that can be used to uniquely identify an Entry between different
		/// packages within a registry. Should be constructable based on information
		/// available within the IR of a package that is consuming an external
		/// package's entry as the target.
		type EntryId: EntryId;
	
		/// resolves an `EntryId` to an actual `EntryIdx` that points to the given
		/// Entry.
		///
		/// This can also load other package IRs if needed, through the provided
		/// `RegistryState`
		// TODO: determine error handling for bad usage: is a panic OK?
		fn entry_id_to_idx(&self, id: Self::EntryId, state: &RegistryState) -> RawEntryIdx;
	
		/// resolves an EntryIdx to it's unique internal `EntryId`
		fn idx_to_entry_id(&self, idx: RawEntryIdx, state: &RegistryState) -> Self::EntryId;
	}
	
	impl<T: RegistryResolver> DynRegistryResolver for T {
		fn __idx_to_entry_id(
			&self,
			idx: RawEntryIdx,
			state: &RegistryState,
		) -> Box<dyn erased_serde::Serialize> {
			Box::new(self.idx_to_entry_id(idx, state))
		}
	
		fn __deser_entry_id_to_idx(
			&self,
			deserializer: &mut dyn erased_serde::Deserializer,
			state: &RegistryState,
		) -> erased_serde::Result<RawEntryIdx> {
			erased_serde::deserialize(deserializer).map(|id| self.entry_id_to_idx(id, state))
		}
	}
	
	mod private {
		use super::{RawEntryIdx, RegistryState};
	
		/// An internal-only auto-implemented subtrait of `Registry` that's used to do
		/// type-erased shenanigans to allow (de)serializing `EntryIdx`s when using
		/// our context helpers
		///
		/// essentially, it's the backing behind the mapping between `EntryIdx` that
		/// exists in-memory and the `EntryId` that's actually (de)serialized.
		pub trait DynRegistryResolver: 'static {
			/// gets the `EntryId` for the corresponding `RawEntryIdx` and converts it
			/// to a type-erased serializable type
			fn __idx_to_entry_id(
				&self,
				idx: RawEntryIdx,
				state: &RegistryState,
			) -> Box<dyn erased_serde::Serialize>;
	
			fn __deser_entry_id_to_idx(
				&self,
				deserializer: &mut dyn erased_serde::Deserializer,
				state: &RegistryState,
			) -> erased_serde::Result<RawEntryIdx>;
		}
	}
99
crates/nudox-ir/src/registry/state.rs
Normal file
	@ -0,0 +1,99 @@
	use bimap::BiHashMap;
	use elsa::sync::FrozenVec;
	use parking_lot::RwLock;
	use rustc_hash::FxHashSet;
	
	use crate::{entry::{Entry, TypedEntry}, kind::EntryKind, module::Module, package::PackageId};
	
	use super::{ArenaIdx, EntryArena, EntryBuilder, EntryIdx, EntryLink, PackageIdx, RawEntryIdx, RegistryResolver};
	
	type BiFxHashMap<L, R> = BiHashMap<L, R, rustc_hash::FxBuildHasher, rustc_hash::FxBuildHasher>;
	
	pub struct RegistryState {
		arenas: FrozenVec<Box<EntryArena>>,
	
		packages: RwLock<BiFxHashMap<PackageId, PackageIdx>>,
		links:    RwLock<FxHashSet<EntryLink>>,
	}
	
	// TODO: make this properly
	#[derive(Clone, Copy)]
	pub struct PackageIRView<'a> {
		package_idx: PackageIdx,
		arena:       &'a EntryArena,
	}
	
	impl<'a> PackageIRView<'a> {
		pub fn enumerate(self) -> impl Iterator<Item = (RawEntryIdx, &'a Entry)> {
			self.arena.iter().enumerate().map(move |(arena_idx, entry)| {
				(RawEntryIdx::new(self.package_idx, ArenaIdx::new(arena_idx)), entry)
			})
		}
	}
	
	impl RegistryState {
		pub fn resolve_package<R: RegistryResolver>(
			&self,
			id: &PackageId,
			_fetch: impl FnOnce(&Self) -> Vec<Entry>,
		) -> PackageIRView<'_> {
			match self.packages.read().get_by_left(&id) {
				Some(&idx) => PackageIRView { package_idx: idx, arena: self.arena(idx) },
				None => {
					// TODO: use fetch to resolve the IR to be loaded and insert it into the
					// registry state
					todo!()
				}
			}
		}
	
		pub fn resolve(&self, idx: RawEntryIdx) -> &Entry {
			self.arena(idx.package_idx()).entry(idx.arena_idx())
		}
	
		pub fn resolve_typed<T: EntryKind>(&self, idx: EntryIdx<T>) -> &TypedEntry<T> {
			TypedEntry::new(self.resolve(idx.raw()))
		}
	
		pub fn package_id_of(&self, idx: RawEntryIdx) -> PackageId {
			self.packages.read().get_by_right(&idx.package_idx()).cloned().expect("") // TODO: error message
		}
	}
	
	impl RegistryState {
		pub(super) fn new() -> Self {
			RegistryState {
				arenas:   FrozenVec::new(),
				packages: RwLock::new(BiFxHashMap::default()),
				links:    RwLock::new(FxHashSet::default()),
			}
		}
	
		pub(crate) fn build_package_ir<R: RegistryResolver>(
			&self,
			package_id: PackageId,
			sym: crate::symbol::Symbol,
			build: impl FnOnce(&mut EntryBuilder),
		) -> EntryIdx<crate::module::Module> {
			let package_idx = PackageIdx::new(self.arenas.len());
			let arena_idx = ArenaIdx::new(0);
	
			let idx = EntryIdx::new(package_idx, arena_idx);
	
			let (entries, links) = EntryBuilder::build(sym, idx, None, |b| {
				build(b);
				Module
			});
	
			self.arenas.push(Box::new(EntryArena::new(entries)));
	
			self.links.write().extend(links);
			self.packages.write().insert(package_id, package_idx);
	
			idx.typed()
		}
	
		fn arena(&self, idx: PackageIdx) -> &EntryArena {
			self.arenas.get(idx.index()).expect("") // TODO: error message
		}
	}
93
crates/nudox-ir/src/registry/tests.rs
Normal file
	@ -0,0 +1,93 @@
	use super::*;
	use crate::{registry::RegistryResolver, test_helpers::*};
	
	#[test]
	fn serialize_deserialize() {
		let registry = build_registry();
	
		let dummy_idx = RawEntryIdx::new(PackageIdx::new(1), ArenaIdx::new(8)); // field_8
	
		assert_eq!(registry.resolver.idx_to_entry_id(dummy_idx, &registry.state), ExampleEntryId {
			package: PackageId::path("/pkg-0"),
			symbol:  String::from("field_8"),
		});
	
		let mut sink = Vec::new();
		let serializer = &mut serde_json::Serializer::new(&mut sink);
	
		registry.serialize(serializer, &dummy_idx).expect("serialization failed");
	
		let serialized_json = String::from_utf8(sink).expect("wrote invalid utf8 during serialization");
	
		dbg!(&serialized_json);
	
		let deserializer = &mut serde_json::Deserializer::from_str(&serialized_json);
	
		let deserialized_dummy_idx: RawEntryIdx =
			registry.deserialize(deserializer).expect("deserialization failed");
	
		assert_eq!(dummy_idx, deserialized_dummy_idx);
	}
	
	fn build_registry() -> Registry<ExampleRegistryResolver> {
		let mut registry = Registry::new(ExampleRegistryResolver::default());
	
		registry.build_package_ir(PackageId::path("/pkg-0"), dummy_symbol("pkg-0"), |b| {
			b.create(dummy_symbol("mod_1"), |_| Module);
			b.create(dummy_symbol("record_2"), |_| Record { fields: slice![] });
		});
	
		registry.build_package_ir(PackageId::path("/pkg-1"), dummy_symbol("pkg-1"), |b| {
			b.create(dummy_symbol("mod_1"), |_| Module);
	
			b.create(dummy_symbol("record_2"), |_| Record { fields: slice![] });
	
			b.create(dummy_symbol("mod_3"), |b| {
				b.create(dummy_symbol("mod_4"), |_| Module);
				b.create(dummy_symbol("record_5"), |_| Record { fields: slice![] });
	
				Module
			});
	
			b.create(dummy_symbol("record_6"), |b| {
				Record::builder()
					.fields(
						["field_7", "field_8", "field_9"]
							.map(dummy_symbol)
							.map(|sym| b.create(sym, |_| Field {})),
					)
					.build(b)
			});
		});
	
		registry
	}
	
	#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	struct ExampleEntryId {
		package: PackageId,
		symbol:  String,
	}
	
	#[derive(Default)]
	struct ExampleRegistryResolver;
	
	impl RegistryResolver for ExampleRegistryResolver {
		type EntryId = ExampleEntryId;
	
		fn entry_id_to_idx(&self, id: Self::EntryId, state: &RegistryState) -> RawEntryIdx {
			let package = state.resolve_package::<Self>(&id.package, |_| unimplemented!());
	
			package
				.enumerate()
				.find_map(|(idx, e)| (e.sym.name == id.symbol).then_some(idx))
				.expect("failed to find symbol")
		}
	
		fn idx_to_entry_id(&self, idx: RawEntryIdx, state: &RegistryState) -> Self::EntryId {
			let package = state.package_id_of(idx);
			let symbol = state.resolve(idx).sym.name.clone();
	
			ExampleEntryId { package, symbol }
		}
	}
15
crates/nudox-ir/src/symbol.rs
Normal file
	@ -0,0 +1,15 @@
	use std::{ops::Range, path::PathBuf};
	
	#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub enum Visibility {
		Public,
	}
	
	#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub struct Symbol {
		pub name:          String,
		pub visibility:    Visibility,
		pub documentation: Option<String>,
		pub source:        PathBuf,
		pub span:          Range<usize>,
	}
58
crates/nudox-ir/src/test_helpers.rs
Normal file
	@ -0,0 +1,58 @@
	use std::path::PathBuf;
	
	use crate::{entry::{Entry, Node}, kind::EntryKind, registry::{ArenaIdx, EntryIdx, PackageIdx, RawEntryIdx, Registry, RegistryResolver, RegistryState}, symbol::{Symbol, Visibility}};
	
	pub use crate::{module::Module, record::{Field, Record}};
	
	pub fn entry<T>(name: &str, node: Node, kind: T) -> Entry
	where
		T: EntryKind,
	{
		Entry::new(dummy_symbol(name), node, kind.into_kind())
	}
	
	pub fn dummy_symbol(name: &str) -> Symbol {
		Symbol {
			name:          name.into(),
			visibility:    Visibility::Public,
			documentation: None,
			source:        PathBuf::new(),
			span:          0..0,
		}
	}
	
	#[expect(unused, reason = "for the future")]
	pub fn n(parent: RawEntryIdx, children: impl IntoIterator<Item = RawEntryIdx>) -> Node {
		Node::new(parent, children)
	}
	
	pub mod n {
		use crate::{entry::Node, registry::RawEntryIdx};
	
		pub fn root(children: impl IntoIterator<Item = RawEntryIdx>) -> Node { Node::root(children) }
	
		pub fn leaf(parent: RawEntryIdx) -> Node { Node::leaf(parent) }
	}
	
	pub fn idx<T>(index: usize) -> EntryIdx<T> {
		EntryIdx::new(PackageIdx::new(0), ArenaIdx::new(index))
	}
	
	pub fn dummy_registry() -> Registry<DummyRegistryResolver> { Registry::new(DummyRegistryResolver) }
	
	pub struct DummyRegistryResolver;
	
	impl RegistryResolver for DummyRegistryResolver {
		type EntryId = ();
	
		fn entry_id_to_idx(&self, _: Self::EntryId, _: &RegistryState) -> RawEntryIdx { unimplemented!() }
		fn idx_to_entry_id(&self, _: RawEntryIdx, _: &RegistryState) -> Self::EntryId { unimplemented!() }
	}
	
	macro_rules! slice {
		($($tt:tt)*) => {
			(::std::vec!($($tt)*)).into_boxed_slice()
		};
	}
	
	pub(crate) use slice;
39
crates/nudox-ir/src/ty.rs
Normal file
	@ -0,0 +1,39 @@
	use crate::{List, primitive::Primitive, registry::EntryIdx};
	
	#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
	pub enum Type {
		/// A receiver/self type such as Rust `Self` or TypeScript `this`.
		SelfType,
	
		/// A fundamental, language-level built-in type.
		/// Ex: `i32`, `f64`, `bool`.
		Primitive(Primitive),
	
		/// A fixed-length, heterogeneous collection of types.
		/// Ex: `(i32, String)`. An empty vec `()` represents the Unit type.
		Tuple(List<EntryIdx<Type>>),
	
		/// A dynamically-sized view into a contiguous sequence.
		/// Ex: `[u8]` or `[]T`.
		Slice(EntryIdx<Type>),
	
		/// A fixed-size contiguous sequence.
		/// Ex: `[i32; 4]` or `std::array<int, 4>`.
		Array { ty: EntryIdx<Type>, length: usize },
	
		/// An untagged union or sum of types.
		/// Ex: `string | number`.
		Union(List<EntryIdx<Type>>),
	
		/// An intersection or combination of types.
		/// Ex: `Serializable & Cloneable`.
		Intersection(List<EntryIdx<Type>>),
	
		/// Represents a type that cannot exist (Bottom Type).
		/// Ex: `!` in Rust, `never` in TypeScript, `NoReturn` in Python.
		Never,
	
		/// Represents the "All" type (Top Type).
		/// Ex: `any` or `unknown` in TypeScript, `Object` in Java.
		Any,
	}
2
rustfmt.toml
	@ -18,8 +18,6 @@ newline_style = "Unix"
	normalize_comments = true
	normalize_doc_attributes = false
	overflow_delimited_expr = true
	reorder_impl_items = true
	group_imports = "StdExternalCrate"
	reorder_modules = true
	struct_field_align_threshold = 99
	tab_spaces = 2
	
