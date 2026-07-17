//! [`EntryBuilder`] — high-level producer API for building `IrAtom` lists.
//!
//! Producers call the `add_*` methods to describe a package's symbols; the
//! builder maintains a local [`EntryArena`] and string interner. When done,
//! [`EntryBuilder::seal_payloads`] converts the arena to
//! `(IntroId, OwnedEntryPayload, Option<IntroId>)` triples. Finally,
//! [`record_ir_atoms`] diffs against a previous [`PristineIntroTable`] to emit
//! the minimal `Vec<IrAtom>` representing the incremental change.
//!
//! # Re-exports
//!
//! [`SymbolBuf`] is re-exported from [`crate::symbol`] so that `lib.rs`'s
//! `pub use builder::SymbolBuf` works.

use nudox_change::{IntroId, PackageLineageId};

use crate::entry::{Entry, EntryArena, EntryInner, Node, StringInterner};
use crate::index::{ArenaIdx, PackageIdx, RawEntryIdx, StrId};
use crate::intro::{bootstrap_intro_id, Disambiguator};
use crate::kind::{Kind, KindDiscriminant};
use crate::skeleton::function_signature_skeleton;
use crate::symbol::{ByteSpan, Deprecation, DocLink, Symbol, Visibility};
use crate::wire::{
    DeprecationWire, DocLinkWire, EntryPayloadFlags, FieldWire, FunctionWire, KindWire, ModuleWire,
    OwnedEntryPayload, ParamWire, RecordWire, SymbolWire, TypeRefWire, TypeWire,
};

// Re-export SymbolBuf so lib.rs's `pub use builder::SymbolBuf` resolves.
pub use crate::symbol::SymbolBuf;

// ---------------------------------------------------------------------------
// EntryBuilder
// ---------------------------------------------------------------------------

/// A progressive builder for a single package's entry arena.
///
/// Call `add_*` in declaration order. Call [`EntryBuilder::seal_payloads`] once
/// all symbols have been registered to obtain the intro-keyed payload list.
pub struct EntryBuilder {
    /// The underlying entry arena (entries + string interner).
    arena: EntryArena,
    /// Parallel-indexed wire kind bodies, one per arena entry (same `ArenaIdx`).
    ///
    /// The in-memory [`Kind`] enum in the arena is deliberately lossy for the MVP
    /// (it does not carry function params, record field lists, or the full type
    /// tree). This side table preserves the *complete* [`KindWire`] passed to the
    /// `add_*` methods so that [`seal_payloads`](Self::seal_payloads) can seal
    /// faithful payloads and compute the function-signature skeleton needed for
    /// the `Overload` disambiguator (design K10, K14).
    kind_wires: Vec<KindWire>,
}

impl EntryBuilder {
    /// Create an empty builder.
    pub fn new() -> Self {
        Self { arena: EntryArena::new(), kind_wires: Vec::new() }
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Convert a `SymbolWire` (owned strings) to an interned [`Symbol`] by
    /// pushing all strings into the arena's interner.
    fn intern_symbol_wire(strings: &mut StringInterner, wire: &SymbolWire) -> Symbol {
        let name = strings.intern(&wire.name);
        let source_path = strings.intern(&wire.source_path);
        let documentation =
            wire.documentation.as_deref().map(|s| strings.intern(s));
        let aliases: Box<[StrId]> = wire
            .aliases
            .iter()
            .map(|a| strings.intern(a))
            .collect();
        let deprecation = wire.deprecation.as_ref().map(|d| Deprecation {
            note: d.note.as_deref().map(|s| strings.intern(s)),
            since: d.since.as_deref().map(|s| strings.intern(s)),
        });
        let doc_links: Box<[DocLink]> = wire
            .doc_links
            .iter()
            .map(|dl| DocLink {
                target: dl.target.clone(),
                label: dl.label.as_deref().map(|s| strings.intern(s)),
            })
            .collect();
        Symbol {
            name,
            visibility: Visibility::from_u8(wire.visibility).unwrap_or(Visibility::Public),
            documentation,
            source_path,
            span: ByteSpan::new(wire.span_start, wire.span_end),
            aliases,
            deprecation,
            doc_links,
        }
    }

    /// Build a [`SymbolWire`] from raw producer arguments.
    fn make_symbol_wire(
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        doc: Option<&str>,
    ) -> SymbolWire {
        SymbolWire {
            name: name.to_owned(),
            visibility: visibility as u8,
            documentation: doc.map(str::to_owned),
            source_path: source_path.to_owned(),
            span_start: span.start,
            span_end: span.end,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
        }
    }

    /// Push a new entry into the arena, optionally registering it as a child of
    /// `parent`.
    fn push_entry(
        &mut self,
        sym_wire: SymbolWire,
        kind: Kind,
        kind_wire: KindWire,
        parent: Option<ArenaIdx>,
    ) -> ArenaIdx {
        // Intern the symbol.
        let sym = Self::intern_symbol_wire(&mut self.arena.strings, &sym_wire);
        // Build node with optional parent.
        let pkg_idx = PackageIdx(0); // builder is always single-package
        let raw_parent =
            parent.map(|a| RawEntryIdx::new(pkg_idx, a));
        let node = Node::leaf(raw_parent);
        let entry = Entry { sym, node, kind: EntryInner::Owned(kind) };
        let idx = self.arena.push(entry);
        // Keep the full wire kind body in the parallel side table so no producer
        // data is lost between `add_*` and `seal_payloads`.
        debug_assert_eq!(self.kind_wires.len(), idx.0 as usize);
        self.kind_wires.push(kind_wire);

        // Register as a child of parent.
        if let Some(parent_idx) = parent {
            let raw_self = RawEntryIdx::new(pkg_idx, idx);
            if let Some(parent_entry) = self.arena.get_mut(parent_idx) {
                let mut children = parent_entry.node.children.to_vec();
                children.push(raw_self);
                parent_entry.node.children = children.into_boxed_slice();
            }
        }

        idx
    }

    // -----------------------------------------------------------------------
    // Public add_* API
    // -----------------------------------------------------------------------

    /// Register a module / namespace entry.
    pub fn add_module(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        self.push_entry(sym_wire, Kind::Module, KindWire::Module(ModuleWire {}), parent)
    }

    /// Register a record / struct / class entry.
    pub fn add_record(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        // Fields are added by later add_field calls; start with empty field list.
        self.push_entry(
            sym_wire,
            Kind::Record,
            KindWire::Record(RecordWire { fields: Box::new([]) }),
            parent,
        )
    }

    /// Register a field entry.
    pub fn add_field(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        ty: Option<TypeRefWire>,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        self.push_entry(
            sym_wire,
            Kind::Field,
            KindWire::Field(FieldWire { ty }),
            parent,
        )
    }

    /// Register a function entry.
    pub fn add_function(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        inputs: Vec<ParamWire>,
        outputs: Vec<ParamWire>,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        self.push_entry(
            sym_wire,
            Kind::Function,
            KindWire::Function(FunctionWire {
                input_params: inputs.into_boxed_slice(),
                output_params: outputs.into_boxed_slice(),
            }),
            parent,
        )
    }

    /// Register a type-alias / typedef entry.
    pub fn add_type_entry(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        ty: TypeWire,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        // For the in-memory Kind, store the wire type as-is (producers don't
        // need the richer in-memory tree; they work with wire types).
        self.push_entry(
            sym_wire,
            Kind::Type(crate::kind::Type::Any), // placeholder; real type in KindWire
            KindWire::Type(ty),
            parent,
        )
    }

    // -----------------------------------------------------------------------
    // seal_payloads
    // -----------------------------------------------------------------------

    /// Convert the arena to a list of `(IntroId, OwnedEntryPayload, Option<IntroId>)` triples.
    ///
    /// For each arena entry (in push order):
    /// 1. Build the ancestor name chain (`segments`) by walking parent links.
    /// 2. Determine the disambiguator (Overload for functions, None otherwise).
    /// 3. Derive the `IntroId` via [`bootstrap_intro_id`].
    /// 4. Seal an [`OwnedEntryPayload`] from the wire symbol + kind.
    /// 5. Map the arena-local parent index to a parent `IntroId` using the
    ///    mapping built in a first pass.
    // The two passes deliberately index by `i`: each step needs both the arena
    // entry at `ArenaIdx(i)` and the parallel `intro_ids[i]` / `kind_wires[i]`
    // slots, so a plain iterator would not carry enough state.
    #[allow(clippy::needless_range_loop)]
    pub fn seal_payloads(
        &self,
        package: &PackageLineageId,
    ) -> Vec<(IntroId, OwnedEntryPayload, Option<IntroId>)> {
        let n = self.arena.len();
        // First pass: compute IntroIds in order so parents are known before children.
        let mut intro_ids: Vec<Option<IntroId>> = vec![None; n];
        // We process in push order (index 0..n). Because push_entry registers
        // parents before children (producers call add_module before add_field),
        // this ordering is correct.
        for i in 0..n {
            let entry = match self.arena.get(ArenaIdx(i as u32)) {
                Some(e) => e,
                None => continue,
            };

            // Build ancestor segments.
            let mut segments: Vec<String> = Vec::new();
            let mut cur_parent = entry.node.parent;
            while let Some(parent_raw) = cur_parent {
                let parent_entry = match self.arena.get(parent_raw.arena_idx) {
                    Some(e) => e,
                    None => break,
                };
                let parent_name = self.arena.strings.resolve(parent_entry.sym.name)
                    .unwrap_or("")
                    .to_owned();
                segments.push(parent_name);
                cur_parent = parent_entry.node.parent;
            }
            segments.reverse(); // root → parent order

            // Resolve leaf name.
            let name = self.arena.strings.resolve(entry.sym.name).unwrap_or("").to_owned();

            // Determine discriminant and disambiguator. The disambiguator is
            // derived from the preserved wire kind body (design K10): functions
            // use their signature skeleton (`Overload`) so two same-named
            // overloads get distinct IntroIds; everything else uses `None`.
            let kind_wire = &self.kind_wires[i];
            let (kind_disc, disambiguator) = match kind_wire {
                KindWire::Module(_) => (KindDiscriminant::Module, Disambiguator::None),
                KindWire::Record(_) => (KindDiscriminant::Record, Disambiguator::None),
                KindWire::Field(_) => (KindDiscriminant::Field, Disambiguator::None),
                KindWire::Function(function_wire) => {
                    let skeleton = function_signature_skeleton(
                        &function_wire.input_params,
                        &function_wire.output_params,
                    );
                    (
                        KindDiscriminant::Function,
                        Disambiguator::Overload(skeleton.into_boxed_slice()),
                    )
                }
                KindWire::Type(_) => (KindDiscriminant::Type, Disambiguator::None),
            };

            let seg_refs: Vec<&str> = segments.iter().map(String::as_str).collect();
            let intro = bootstrap_intro_id(package, kind_disc, &seg_refs, &name, &disambiguator);
            intro_ids[i] = Some(intro);
        }

        // Second pass: build payloads.
        let mut result = Vec::with_capacity(n);
        for i in 0..n {
            let intro = match intro_ids[i] {
                Some(id) => id,
                None => continue,
            };

            let entry = match self.arena.get(ArenaIdx(i as u32)) {
                Some(e) => e,
                None => continue,
            };

            // Reconstruct SymbolWire from interned Symbol.
            let sym_wire = self.symbol_to_wire(entry);

            // Rebuild the faithful KindWire from the preserved side table. This
            // carries the exact function params, record field lists, and type
            // tree the producer supplied — nothing is lost.
            let kind_wire = self.kind_wires[i].clone();
            let kind_disc = kind_wire.discriminant();

            let mut flags = EntryPayloadFlags::default();
            if entry.sym.deprecation.is_some() {
                flags.set(EntryPayloadFlags::HAS_DEPRECATION);
            }
            if matches!(entry.kind, EntryInner::Reference(_)) {
                flags.set(EntryPayloadFlags::IS_REFERENCE);
            }

            let payload = OwnedEntryPayload::sealed(sym_wire, kind_disc, kind_wire, flags);

            // Resolve parent IntroId.
            let parent_intro = entry
                .node
                .parent
                .and_then(|p| intro_ids[p.arena_idx.0 as usize]);

            result.push((intro, payload, parent_intro));
        }

        result
    }

    /// Reconstruct a [`SymbolWire`] from an interned [`Entry`]'s [`Symbol`].
    fn symbol_to_wire(&self, entry: &Entry) -> SymbolWire {
        let sym = &entry.sym;
        SymbolWire {
            name: self.arena.strings.resolve(sym.name).unwrap_or("").to_owned(),
            visibility: sym.visibility as u8,
            documentation: sym.documentation
                .and_then(|id| self.arena.strings.resolve(id))
                .map(str::to_owned),
            source_path: self.arena.strings.resolve(sym.source_path).unwrap_or("").to_owned(),
            span_start: sym.span.start,
            span_end: sym.span.end,
            aliases: sym.aliases
                .iter()
                .filter_map(|id| self.arena.strings.resolve(*id))
                .map(str::to_owned)
                .collect(),
            deprecation: sym.deprecation.as_ref().map(|d| DeprecationWire {
                note: d.note.and_then(|id| self.arena.strings.resolve(id)).map(str::to_owned),
                since: d.since.and_then(|id| self.arena.strings.resolve(id)).map(str::to_owned),
            }),
            doc_links: sym.doc_links
                .iter()
                .map(|dl| DocLinkWire {
                    target: dl.target.clone(),
                    label: dl.label.and_then(|id| self.arena.strings.resolve(id)).map(str::to_owned),
                })
                .collect(),
        }
    }
}

impl Default for EntryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbol::ByteSpan;

    fn cargo_pkg() -> PackageLineageId {
        use nudox_change::{EcosystemId, PackageName};
        PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("mylib"))
    }

    #[test]
    fn add_module_and_function() {
        let mut builder = EntryBuilder::new();
        let pkg = cargo_pkg();
        let mod_idx = builder.add_module("root", Visibility::Public, "src/lib.rs", ByteSpan::ZERO, None, None);
        let fn_idx = builder.add_function(
            "do_thing",
            Visibility::Public,
            "src/lib.rs",
            ByteSpan::new(10, 50),
            Some(mod_idx),
            None,
            vec![],
            vec![],
        );

        let payloads = builder.seal_payloads(&pkg);
        assert_eq!(payloads.len(), 2);
        // Function's parent IntroId should be the module's IntroId.
        let (_fn_intro, _, fn_parent) = &payloads[fn_idx.0 as usize];
        let (mod_intro, _, _) = &payloads[mod_idx.0 as usize];
        assert_eq!(fn_parent.as_ref(), Some(mod_intro));
    }

    #[test]
    fn different_names_get_different_intros() {
        let mut builder = EntryBuilder::new();
        let pkg = cargo_pkg();
        builder.add_module("Alpha", Visibility::Public, "src/lib.rs", ByteSpan::ZERO, None, None);
        builder.add_module("Beta", Visibility::Public, "src/lib.rs", ByteSpan::ZERO, None, None);
        let payloads = builder.seal_payloads(&pkg);
        let id0 = payloads[0].0;
        let id1 = payloads[1].0;
        assert_ne!(id0, id1);
    }

    #[test]
    fn seal_payloads_feeds_a_container() {
        // The builder produces (intro, payload, parent) triples that populate a
        // `PristineIntroTable` — the input the libpijul-backed VCS records.
        use crate::apply::PristineIntroTable;
        let mut builder = EntryBuilder::new();
        let root = builder.add_module("Foo", Visibility::Public, "src/lib.rs", ByteSpan::ZERO, None, None);
        builder.add_function("bar", Visibility::Public, "src/lib.rs", ByteSpan::new(1, 2), Some(root), None, vec![], vec![]);
        let payloads = builder.seal_payloads(&cargo_pkg());
        assert_eq!(payloads.len(), 2);

        let mut table = PristineIntroTable::new();
        for (intro, payload, parent) in payloads {
            table.insert_live(intro, payload, parent);
        }
        assert_eq!(table.len(), 2);
    }
}
