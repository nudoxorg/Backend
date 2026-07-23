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
//!
//! # Disambiguator selection (§4.3 collision-scoped rule)
//!
//! `seal_payloads` uses v2 ids. The rule is:
//! - Count how many entries in this generation share `(kind_disc, segments, name)`.
//! - If ≥2 **functions** share the key → `FnOverload(signature_skeleton)`.
//! - If the entry is an `Impl` → always `TraitImpl(trait_impl_skeleton)`.
//! - If ≥2 non-function entries share the key → `Span { start, end }`.
//! - Otherwise → `None` (the common, signature-stable case).

use std::collections::HashMap;

use crate::change::{IntroId, PackageLineageId};

use crate::entry::{Entry, EntryArena, EntryInner, Node, StringInterner};
use crate::index::{ArenaIdx, PackageIdx, RawEntryIdx, StrId};
use crate::intro::{bootstrap_intro_id_v2, DisambiguatorV2};
use crate::kind::{Kind, KindDiscriminant};
use crate::skeleton::{function_signature_skeleton, trait_impl_skeleton};
use crate::symbol::{ByteSpan, Deprecation, DocLink, Symbol, Visibility};
use crate::wire::{
    ConstWire, DeprecationWire, DocLinkWire, EnumWire, EntryPayloadFlags, FieldWire, FnSigFlags,
    FunctionWire, GenericParamWire, ImplFlags, ImplWire, KindWire, ModuleWire, OwnedEntryPayload,
    ParamWire, RecordForm, RecordWire, ReexportWire, StaticWire, SymbolWire, TraitFlags, TraitWire,
    TypeAliasWire, TypeRefWire, TypeWire, VariantForm, VariantWire, WherePredWire,
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
    /// the `FnOverload` disambiguator (design K10, K14).
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
            visibility: wire.visibility,
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
            visibility,
            documentation: doc.map(str::to_owned),
            source_path: source_path.to_owned(),
            span_start: span.start,
            span_end: span.end,
            aliases: Vec::new(),
            deprecation: None,
            doc_links: Vec::new(),
            attrs: Vec::new(),
            cfg: None,
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
            KindWire::Record(RecordWire {
                form: RecordForm::Struct,
                fields: Box::new([]),
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
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

    /// Register a function entry with full signature metadata.
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
        self.add_function_with_sig(
            name, visibility, source_path, span, parent, doc,
            inputs, outputs, FnSigFlags::default(), Box::new([]), Box::new([]),
        )
    }

    /// Register a function entry with full signature, generics, and where-clauses.
    pub fn add_function_with_sig(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        inputs: Vec<ParamWire>,
        outputs: Vec<ParamWire>,
        sig: FnSigFlags,
        generics: Box<[GenericParamWire]>,
        wheres: Box<[WherePredWire]>,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        self.push_entry(
            sym_wire,
            Kind::Function,
            KindWire::Function(FunctionWire {
                input_params: inputs.into_boxed_slice(),
                output_params: outputs.into_boxed_slice(),
                sig,
                generics,
                wheres,
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
        self.push_entry(
            sym_wire,
            Kind::Type(crate::kind::Type::Any), // placeholder; real type in KindWire
            KindWire::Type(TypeAliasWire {
                ty,
                generics: Box::new([]),
                wheres: Box::new([]),
                auto: Box::new([]),
            }),
            parent,
        )
    }

    /// Register a trait definition entry.
    pub fn add_trait(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        supers: Box<[TypeRefWire]>,
        flags: TraitFlags,
        generics: Box<[GenericParamWire]>,
        wheres: Box<[WherePredWire]>,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        self.push_entry(
            sym_wire,
            Kind::Module, // placeholder — Kind does not have a Trait variant; wire carries truth
            KindWire::Trait(TraitWire { supers, flags, generics, wheres }),
            parent,
        )
    }

    /// Register an impl block entry.
    ///
    /// The `name` for impls is conventionally `"impl"`.
    pub fn add_impl(
        &mut self,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        of: Option<TypeRefWire>,
        self_ty: TypeWire,
        flags: ImplFlags,
        generics: Box<[GenericParamWire]>,
        wheres: Box<[WherePredWire]>,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire("impl", visibility, source_path, span, doc);
        self.push_entry(
            sym_wire,
            Kind::Module, // placeholder
            KindWire::Impl(ImplWire { of, self_ty, flags, generics, wheres }),
            parent,
        )
    }

    /// Register an enum type entry.
    pub fn add_enum(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        generics: Box<[GenericParamWire]>,
        wheres: Box<[WherePredWire]>,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        self.push_entry(
            sym_wire,
            Kind::Record, // placeholder
            KindWire::Enum(EnumWire {
                variants: Box::new([]),
                generics,
                wheres,
                auto: Box::new([]),
            }),
            parent,
        )
    }

    /// Register an enum variant entry.
    pub fn add_variant(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        form: VariantForm,
        discr: Option<String>,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        self.push_entry(
            sym_wire,
            Kind::Field, // placeholder
            KindWire::Variant(VariantWire { form, discr, fields: Box::new([]) }),
            parent,
        )
    }

    /// Register a constant declaration.
    pub fn add_const(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        ty: TypeRefWire,
        value: Option<String>,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        self.push_entry(
            sym_wire,
            Kind::Type(crate::kind::Type::Any), // placeholder
            KindWire::Const(ConstWire { ty, value }),
            parent,
        )
    }

    /// Register a static declaration.
    pub fn add_static(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        ty: TypeRefWire,
        mutable: bool,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        self.push_entry(
            sym_wire,
            Kind::Type(crate::kind::Type::Any), // placeholder
            KindWire::Static(StaticWire { ty, mutable }),
            parent,
        )
    }

    /// Register a re-export entry (`pub use …`).
    pub fn add_reexport(
        &mut self,
        name: &str,
        visibility: Visibility,
        source_path: &str,
        span: ByteSpan,
        parent: Option<ArenaIdx>,
        doc: Option<&str>,
        target: crate::change::StableRef,
    ) -> ArenaIdx {
        let sym_wire = Self::make_symbol_wire(name, visibility, source_path, span, doc);
        self.push_entry(
            sym_wire,
            Kind::Module, // placeholder
            KindWire::Reexport(ReexportWire { target }),
            parent,
        )
    }

    // -----------------------------------------------------------------------
    // seal_payloads
    // -----------------------------------------------------------------------

    /// Convert the arena to a list of `(IntroId, OwnedEntryPayload, Option<IntroId>)` triples.
    ///
    /// Uses `bootstrap_intro_id_v2` with the collision-scoped disambiguator rule (§4.3):
    ///
    /// 1. Build a base-key map `(kind_disc, segments, name) → count`.
    /// 2. Select the disambiguator per entry:
    ///    - Function with ≥2 siblings at the same key → `FnOverload(signature_skeleton)`.
    ///    - Impl → always `TraitImpl(trait_impl_skeleton)`.
    ///    - Any other kind with ≥2 siblings → `Span { start, end }`.
    ///    - Unique → `None`.
    ///
    // The two passes deliberately index by `i`: each step needs both the arena
    // entry at `ArenaIdx(i)` and the parallel `intro_ids[i]` / `kind_wires[i]`
    // slots, so a plain iterator would not carry enough state.
    #[allow(clippy::needless_range_loop)]
    pub fn seal_payloads(
        &self,
        package: &PackageLineageId,
    ) -> Vec<(IntroId, OwnedEntryPayload, Option<IntroId>)> {
        let n = self.arena.len();

        // ----------------------------------------------------------------
        // Pre-pass: build ancestor-segment chains and count collisions.
        // ----------------------------------------------------------------
        // Compute segments for every entry first so we can count collisions.
        let mut all_segments: Vec<Vec<String>> = Vec::with_capacity(n);
        let mut all_names: Vec<String> = Vec::with_capacity(n);
        let mut all_kind_discs: Vec<KindDiscriminant> = Vec::with_capacity(n);

        for i in 0..n {
            let entry = match self.arena.get(ArenaIdx(i as u32)) {
                Some(e) => e,
                None => {
                    all_segments.push(Vec::new());
                    all_names.push(String::new());
                    all_kind_discs.push(KindDiscriminant::Module);
                    continue;
                }
            };

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
            segments.reverse();

            let name = self.arena.strings.resolve(entry.sym.name).unwrap_or("").to_owned();
            let kind_disc = self.kind_wires[i].discriminant();

            all_segments.push(segments);
            all_names.push(name);
            all_kind_discs.push(kind_disc);
        }

        // Count how many entries share each (kind_disc, segments, name) key.
        // Key = (kind_disc_u16, segments.join("/"), name) — just hash the tuple.
        let mut key_counts: HashMap<(u16, String, String), u32> = HashMap::new();
        for i in 0..n {
            if self.arena.get(ArenaIdx(i as u32)).is_none() {
                continue;
            }
            let key = (
                all_kind_discs[i].as_u16(),
                all_segments[i].join("\x00"), // NUL-joined; segments never contain NUL
                all_names[i].clone(),
            );
            *key_counts.entry(key).or_insert(0) += 1;
        }

        // ----------------------------------------------------------------
        // First pass: compute IntroIds.
        // ----------------------------------------------------------------
        let mut intro_ids: Vec<Option<IntroId>> = vec![None; n];

        for i in 0..n {
            let entry = match self.arena.get(ArenaIdx(i as u32)) {
                Some(e) => e,
                None => continue,
            };

            let kind_wire = &self.kind_wires[i];
            let kind_disc = all_kind_discs[i];
            let segments = &all_segments[i];
            let name = &all_names[i];

            let key = (
                kind_disc.as_u16(),
                segments.join("\x00"),
                name.clone(),
            );
            let count = *key_counts.get(&key).unwrap_or(&1);

            // Select disambiguator per the §4.3 collision-scoped rule.
            let disambiguator = match kind_wire {
                KindWire::Function(fw) if count >= 2 => {
                    let skel = function_signature_skeleton(
                        &fw.input_params,
                        &fw.output_params,
                    );
                    DisambiguatorV2::FnOverload(skel.into_boxed_slice())
                }
                KindWire::Impl(iw) => {
                    let skel = trait_impl_skeleton(
                        iw.of.as_ref(),
                        &iw.self_ty,
                    );
                    DisambiguatorV2::TraitImpl(skel.into_boxed_slice())
                }
                _ if count >= 2 => {
                    DisambiguatorV2::Span {
                        start: entry.sym.span.start,
                        end: entry.sym.span.end,
                    }
                }
                _ => DisambiguatorV2::None,
            };

            let seg_refs: Vec<&str> = segments.iter().map(String::as_str).collect();
            let intro = bootstrap_intro_id_v2(package, kind_disc, &seg_refs, name, &disambiguator);
            intro_ids[i] = Some(intro);
        }

        // ----------------------------------------------------------------
        // Second pass: build payloads.
        // ----------------------------------------------------------------
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

            let sym_wire = self.symbol_to_wire(entry);
            let kind_wire = self.kind_wires[i].clone();
            let kind_disc = kind_wire.discriminant();

            let mut flags = EntryPayloadFlags::default();
            if entry.sym.deprecation.is_some() {
                flags.set(EntryPayloadFlags::HAS_DEPRECATION);
            }
            // Note: IS_REFERENCE is retired in v2; Reexport kind carries the target.

            let payload = OwnedEntryPayload::sealed(sym_wire, kind_disc, kind_wire, flags);

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
            visibility: sym.visibility,
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
            // attrs and cfg are not stored in the in-memory Symbol; they remain
            // empty here. Producers that need to record attrs/cfg should build
            // the SymbolWire directly and use the lower-level API.
            attrs: Vec::new(),
            cfg: None,
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
    use crate::wire::{ImplFlags, TraitFlags, TypeWire, VariantForm};

    fn cargo_pkg() -> PackageLineageId {
        use crate::change::{EcosystemId, PackageName};
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

    #[test]
    fn unique_function_gets_none_disambiguator() {
        // A unique-named function must use DisambiguatorV2::None so its id is
        // signature-stable (the I2 fix from §4.3).
        let mut builder = EntryBuilder::new();
        let pkg = cargo_pkg();
        builder.add_function("unique_fn", Visibility::Public, "src/lib.rs", ByteSpan::ZERO, None, None, vec![], vec![]);
        let payloads = builder.seal_payloads(&pkg);
        assert_eq!(payloads.len(), 1);
        // The id must equal what bootstrap_intro_id_v2 produces with None disambiguator.
        let expected = crate::intro::bootstrap_intro_id_v2(
            &pkg,
            KindDiscriminant::Function,
            &[],
            "unique_fn",
            &DisambiguatorV2::None,
        );
        assert_eq!(payloads[0].0, expected);
    }

    #[test]
    fn overloaded_functions_get_distinct_intros() {
        use crate::wire::ParamWire;
        use crate::change::IntroId;

        let mut builder = EntryBuilder::new();
        let pkg = cargo_pkg();
        // Two functions with the same name but different signatures.
        let ty_a = TypeRefWire::Same(IntroId::from_raw([0xAA; 32]));
        let ty_b = TypeRefWire::Same(IntroId::from_raw([0xBB; 32]));
        builder.add_function(
            "overloaded", Visibility::Public, "src/lib.rs", ByteSpan::new(0, 10),
            None, None,
            vec![ParamWire { name: None, ty: ty_a }],
            vec![],
        );
        builder.add_function(
            "overloaded", Visibility::Public, "src/lib.rs", ByteSpan::new(20, 30),
            None, None,
            vec![ParamWire { name: None, ty: ty_b }],
            vec![],
        );
        let payloads = builder.seal_payloads(&pkg);
        assert_eq!(payloads.len(), 2);
        assert_ne!(payloads[0].0, payloads[1].0, "overloads must have distinct ids");
    }

    #[test]
    fn impl_always_uses_trait_impl_disambiguator() {
        let mut builder = EntryBuilder::new();
        let pkg = cargo_pkg();
        builder.add_impl(
            Visibility::Public,
            "src/lib.rs",
            ByteSpan::new(0, 10),
            None,
            None,
            None,
            TypeWire::SelfType,
            ImplFlags::default(),
            Box::new([]),
            Box::new([]),
        );
        let payloads = builder.seal_payloads(&pkg);
        assert_eq!(payloads.len(), 1);
        // The id must equal what bootstrap_intro_id_v2 produces with TraitImpl disambiguator.
        let skel = trait_impl_skeleton(None, &TypeWire::SelfType);
        let expected = crate::intro::bootstrap_intro_id_v2(
            &pkg,
            KindDiscriminant::Impl,
            &[],
            "impl",
            &DisambiguatorV2::TraitImpl(skel.into_boxed_slice()),
        );
        assert_eq!(payloads[0].0, expected);
    }

    #[test]
    fn add_new_kinds_all_compile() {
        // Smoke test: verifies all add_* variants are callable and produce payloads.
        use crate::change::{EcosystemId, IntroId, PackageLineageId, PackageName, StableRef};

        let pkg = PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("smoke"));
        let mut b = EntryBuilder::new();
        let span = ByteSpan::ZERO;
        let vis = Visibility::Public;
        let src = "src/lib.rs";

        b.add_trait("MyTrait", vis, src, span, None, None,
            Box::new([]), TraitFlags::default(), Box::new([]), Box::new([]));
        b.add_impl(vis, src, span, None, None, None, TypeWire::SelfType,
            ImplFlags::default(), Box::new([]), Box::new([]));
        b.add_enum("MyEnum", vis, src, span, None, None, Box::new([]), Box::new([]));
        b.add_variant("VarA", vis, src, span, None, None, VariantForm::Unit, None);
        b.add_const("MY_CONST", vis, src, span, None, None,
            TypeRefWire::Same(IntroId::from_raw([0u8; 32])), Some("42".into()));
        b.add_static("MY_STATIC", vis, src, span, None, None,
            TypeRefWire::Same(IntroId::from_raw([0u8; 32])), false);
        b.add_reexport("MyReexport", vis, src, span, None, None,
            StableRef::new(pkg.clone(), IntroId::from_raw([1u8; 32])));

        let payloads = b.seal_payloads(&pkg);
        assert_eq!(payloads.len(), 7);
    }

    /// C-3 (I2 fix): a **unique-named** function's IntroId is signature-STABLE —
    /// changing a param type across generations keeps the same id with no matcher,
    /// because a unique name gets `DisambiguatorV2::None` (empty), so the signature
    /// skeleton never enters the preimage.
    #[test]
    fn unique_fn_id_stable_across_signature_change() {
        use crate::change::IntroId;

        let pkg = cargo_pkg();
        let param = |raw: u8| ParamWire { name: Some("x".into()), ty: TypeRefWire::Same(IntroId::from_raw([raw; 32])) };

        let mut gen1 = EntryBuilder::new();
        gen1.add_function("solo", Visibility::Public, "src/lib.rs", ByteSpan::new(1, 2), None, None, vec![param(0x11)], vec![]);
        let id1 = gen1.seal_payloads(&pkg)[0].0;

        let mut gen2 = EntryBuilder::new();
        gen2.add_function("solo", Visibility::Public, "src/lib.rs", ByteSpan::new(1, 2), None, None, vec![param(0x22)], vec![]);
        let id2 = gen2.seal_payloads(&pkg)[0].0;

        assert_eq!(id1, id2, "unique-named fn must keep its IntroId under a param-type change");
    }

    /// C-6 (overload-set churn): adding a second overload of `f` flips the FIRST
    /// `f`'s disambiguator (None → FnOverload), so its wire id changes — the host
    /// matcher then reunifies at High from the identical payload (that reunion is
    /// tested in nudox-ir-vcs; here we only pin the wire-id flip).
    #[test]
    fn overload_set_churn_flips_first_fn_id() {
        let pkg = cargo_pkg();

        let mut gen1 = EntryBuilder::new();
        gen1.add_function("f", Visibility::Public, "src/lib.rs", ByteSpan::new(1, 2), None, None, vec![], vec![]);
        let solo_id = gen1.seal_payloads(&pkg)[0].0;

        let mut gen2 = EntryBuilder::new();
        gen2.add_function("f", Visibility::Public, "src/lib.rs", ByteSpan::new(1, 2), None, None, vec![], vec![]);
        gen2.add_function(
            "f", Visibility::Public, "src/lib.rs", ByteSpan::new(3, 4), None, None,
            vec![ParamWire { name: None, ty: TypeRefWire::Same(crate::change::IntroId::from_raw([9u8; 32])) }],
            vec![],
        );
        let flipped_id = gen2.seal_payloads(&pkg)[0].0;

        assert_ne!(solo_id, flipped_id, "adding a 2nd overload must flip the first fn's wire id");
    }
}
