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
