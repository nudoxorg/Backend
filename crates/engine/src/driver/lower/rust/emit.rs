//! Runs the Rust semantic emitter passes over one borrowed authority.
use super::*;

impl<'authority, 'analysis, 'source> Emitter<'authority, 'analysis, 'source> {
    /// Opens one emitter over a borrowed rust-analyzer authority and the shared fact lane.
    pub(super) fn new(
        authority: &'authority RustAuthority<'analysis>,
        source: &'source [u8],
        facts: &'authority mut FactSet<'source>,
    ) -> Self {
        Self {
            database: authority.database,
            authority,
            source,
            facts,
            rows: Vec::new(),
            definitions: Vec::new(),
            fields: Vec::new(),
            covered_defs: Vec::new(),
            covered_fields: Vec::new(),
            covered_impls: Vec::new(),
            ordinals: Vec::new(),
            foreign_rows: Vec::new(),
            carrier_rows: Vec::new(),
            macro_sites: Vec::new(),
            scoped_names: std::collections::HashSet::new(),
            owner_order: Vec::new(),
            computed_owner: None,
        }
    }

    /// Streams the complete semantic plane in lane order.
    pub(super) fn run(&mut self) -> Result<(), RustAuthorityError> {
        let declarations = self.materialize_declarations()?;
        self.ordinals = declarations.iter().map(|_| None).collect();
        self.collect_macro_sites()?;
        self.emit_type_roots(&declarations)?;
        self.emit_members(&declarations)?;
        self.emit_reexports()?;
        self.rebuild_owner_order();
        self.emit_parentage()?;
        self.attach_macros()?;
        self.emit_occurrences()?;
        self.emit_computed()?;
        self.emit_docs(&declarations)?;
        self.emit_reexport_docs()?;
        Ok(())
    }

    /// Borrows exact source bytes for a previously validated span.
    fn bytes_of(&self, span: ByteSpan) -> Result<&'source [u8], RustAuthorityError> {
        let start = usize::try_from(span.start)
            .map_err(|source| RustAuthorityError::Coordinate { span, source })?;
        let end = usize::try_from(span.end)
            .map_err(|source| RustAuthorityError::Coordinate { span, source })?;
        self.source
            .get(start..end)
            .ok_or(RustAuthorityError::InvalidSpan {
                span,
                source_bytes: self.source.len(),
            })
    }

    /// Borrows exact source bytes for one syntax node's validated span.
    fn bytes_of_node(&self, node: &SyntaxNode) -> Result<&'source [u8], RustAuthorityError> {
        let span = self.authority.span(node)?;
        self.bytes_of(span)
    }

    /// Locates one borrowed doc line's exact byte span in the caller source.
    /// The parsed tree's text lives in its own buffer, so the line is found
    /// by content; a line absent from the source stays unborrowed.
    fn span_of_text(&self, text: &str) -> Option<ByteSpan> {
        let needle = text.as_bytes();
        if needle.is_empty() || self.source.len() < needle.len() {
            return None;
        }
        let first = needle[0];
        self.source
            .iter()
            .enumerate()
            .filter(|(_, byte)| **byte == first)
            .map(|(start, _)| start)
            .find(|&start| {
                self.source
                    .get(start..start + needle.len())
                    .is_some_and(|window| window == needle)
            })
            .and_then(|start| {
                let start = u32::try_from(start).ok()?;
                let end = start.checked_add(u32::try_from(needle.len()).ok()?)?;
                Some(ByteSpan { start, end })
            })
    }

    /// Materializes every lane-admissible declaration once, with its exact
    /// name bytes and whole-item span. A `macro_rules!` definition keeps its
    /// closed `Macro` row, so a crate whose only declaration is a macro
    /// lowers like any other. After the written syntax walk, the HIR
    /// module-scope walk appends the uncovered remainder: declarations that
    /// exist only through macro expansion and tuple-struct fields the
    /// written tree does not cast.
    fn materialize_declarations(&mut self) -> Result<Vec<Decl<'source>>, RustAuthorityError> {
        let authority = self.authority;
        let mut declarations = Vec::new();
        // rust-analyzer projects a `#[cfg]`-disabled module through the written
        // syntax walk even though it omits every other cfg-disabled item from
        // HIR, and its module lookup resolves both same-name branches to the
        // one enabled definition. Its whole subtree therefore leaks with it
        // (the nested items are syntactically present). Such a module is
        // dropped, subtree and all, exactly as the module-scope walk already
        // omits it.
        let crate_cfg = self.crate_cfg_options();
        let mut disabled_spans: Vec<ByteSpan> = Vec::new();
        for declaration in authority.declarations() {
            let span = authority.span(&declaration.syntax)?;
            if let RustDefinition::Module(_) = &declaration.definition
                && let Some(item) = ast::Module::cast(declaration.syntax.clone())
                && module_cfg_disabled(&item, &crate_cfg)
            {
                disabled_spans.push(span);
                continue;
            }
            if disabled_spans
                .iter()
                .any(|disabled| disabled.start <= span.start && span.end <= disabled.end)
            {
                continue;
            }
            // An anonymous `const _: () = …;` carries no name of its own. The
            // authority's first-identifier fallback would otherwise name it
            // after an incidental path in its initializer (`assert`),
            // collapsing every such compile-time assertion at one scope onto a
            // fabricated duplicate. Anonymous items are not name-addressable;
            // they stay out, exactly as the module-scope walk drops an absent
            // name.
            if let RustDefinition::Constant(_) = &declaration.definition
                && let Some(item) = ast::Const::cast(declaration.syntax.clone())
                && item.name().is_none()
            {
                continue;
            }
            let name_span = self.declaration_name(&declaration)?;
            let name = self.bytes_of(name_span)?;
            // Rust's single-name law (E0428) makes any two same-name items
            // in one scope cfg-disjoint twins; an authority that does not
            // evaluate the gate (a tool attribute such as
            // `#[rustversion::since]`) streams both. Commit the first and
            // keep each later twin out rather than minting byte-identical
            // declaration identities the image build must reject.
            //
            // An implementation is keyed on its full written signature
            // instead of its bare name. E0428 never governs impl blocks, so
            // a source legally carries many written impls whose self type
            // spells the same bytes — several inherent blocks for one type,
            // or several traits impl'd for one self type (`IntoIterator` and
            // `TryFrom<&[T]>` both written `for &'a GenericArray<T, N>`).
            // `declaration_name` deliberately narrows an impl's name to its
            // self type alone (see its doc comment), so keying this gate on
            // that name alone would treat every one of those legal siblings
            // as the same cfg twin and silently drop all but the first,
            // orphaning its members to a fabricated root scope. The richer
            // signature key (trait spelling, self type, generics/where
            // text, and ordered member names) mirrors what the
            // trait/generics/member-aware declaration identity already
            // discriminates real siblings on, so distinct siblings pass
            // this gate untouched while a genuine tool-attribute twin
            // (identical trait, self type, generics, and members) still
            // dedups here exactly as every other declaration kind does.
            let scope = self.enclosing_item_span(&declaration.syntax)?;
            let key = if declaration.kind == SemanticKind::Implementation {
                self.impl_signature_key(&declaration, name)?
            } else {
                name.to_vec()
            };
            let admitted = self
                .scoped_names
                .insert((scope, declaration.kind as u8, key));
            if !admitted {
                // A written implementation the HIR module-scope walk below
                // can independently rediscover (unlike an ordinary named
                // item, which that walk never re-enumerates once the
                // written-syntax pass has cast it) must still be marked
                // covered here, or the twin this gate just dropped comes
                // back as a second, byte-identical admission and the image
                // build honestly — but wrongly — rejects it as a
                // `DuplicateDeclarationIdentity` collision with itself.
                if let RustDefinition::Implementation(implementation) = &declaration.definition {
                    self.covered_impls.push(*implementation);
                }
                continue;
            }
            match &declaration.definition {
                RustDefinition::Field(field) => self.covered_fields.push(*field),
                RustDefinition::Implementation(implementation) => {
                    self.covered_impls.push(*implementation);
                }
                other => {
                    if let Some(definition) = module_def(other) {
                        self.covered_defs.push(definition);
                    }
                }
            }
            declarations.push(Decl {
                kind: declaration.kind,
                definition: declaration.definition,
                syntax: declaration.syntax,
                name,
                span,
                expanded: false,
            });
        }
        for declaration in authority.module_declarations() {
            let ModuleDeclaration {
                definition,
                item,
                name: name_span,
                syntax,
            } = declaration;
            let SourceOrigin::Local(span) = item else {
                continue;
            };
            if self.is_covered(&definition) {
                continue;
            }
            // A walked field without a name node is a tuple field; it keeps
            // its canonical positional spelling, exactly the name Rust uses
            // for `.0`-style access. Any other unnamed position stays out.
            let name = match name_span {
                Some(name_span) => self.bytes_of(name_span)?,
                None => {
                    let RustDefinition::Field(field) = &definition else {
                        continue;
                    };
                    let index = field.index();
                    let Some(name) = tuple_field_name(usize::from(index)) else {
                        continue;
                    };
                    if field.name(self.database).as_str().as_bytes() != name {
                        // A named expansion field whose projected name is
                        // unavailable is not a positional field. Keeping it
                        // would mint a false `.0`-style declaration.
                        continue;
                    }
                    name
                }
            };
            let expanded = authority.is_macro_expansion(&syntax);
            declarations.push(Decl {
                kind: definition.kind(),
                definition,
                syntax,
                name,
                span,
                expanded,
            });
        }
        Ok(declarations)
    }

    /// The exact span of the innermost enclosing item, so single-name-law
    /// twins are keyed by their true scope: two nested helpers in different
    /// functions stay distinct, while two same-name items in one function
    /// body are the E0428 twins the image build cannot represent twice.
    /// `None` marks a top-level item, whose scope is the crate file itself.
    fn enclosing_item_span(
        &self,
        syntax: &SyntaxNode,
    ) -> Result<Option<(u32, u32)>, RustAuthorityError> {
        let Some(item) = syntax.ancestors().skip(1).find_map(ast::Item::cast) else {
            return Ok(None);
        };
        let span = self.authority.span(item.syntax())?;
        Ok(Some((span.start, span.end)))
    }

    /// Clones this source crate's resolved conditional-compilation options,
    /// owned so the written walk can hold them while it mutates its cover
    /// sets. The options already include the caller's Cargo feature policy and
    /// target, so they evaluate exactly as rust-analyzer's own HIR did.
    fn crate_cfg_options(&self) -> ra_ap_hir::CfgOptions {
        self.authority
            .semantics
            .hir_file_to_module_def(self.authority.source_file)
            .map(|root| root.krate(self.database).cfg(self.database).clone())
            .unwrap_or_default()
    }

    /// True when the written syntax walk already materialized one HIR
    /// definition, so the module-scope walk must not emit it twice.
    fn is_covered(&self, definition: &RustDefinition) -> bool {
        match definition {
            RustDefinition::Field(field) => self.covered_fields.contains(field),
            RustDefinition::Implementation(implementation) => {
                self.covered_impls.contains(implementation)
            }
            other => module_def(other).is_some_and(|key| self.covered_defs.contains(&key)),
        }
    }

    /// Borrows the exact name bytes of one declaration. Implementations name
    /// by their whole written self type (the closed lane has no anonymous
    /// rows), so two blocks whose self types differ only in generic
    /// arguments (`U<N, false>` versus `U<N, true>`) commit distinct
    /// declarations instead of byte-identical twins; falling back to the
    /// last path segment alone would collapse them and the image build
    /// would honestly reject the collision. A non-path self type (`&[u8]`,
    /// a tuple, a trait object) owns no name segment; its exact written
    /// spelling is likewise the only self-describing name — the authority's
    /// first-identifier rule would otherwise name the block after its first
    /// method or generic parameter and collapse distinct impls together.
    fn declaration_name(
        &self,
        declaration: &RustDeclaration,
    ) -> Result<ByteSpan, RustAuthorityError> {
        if declaration.kind == SemanticKind::Implementation
            && let Some(implementation) = ast::Impl::cast(declaration.syntax.clone())
            && let Some(self_ty) = implementation.self_ty()
        {
            return self.authority.span(self_ty.syntax());
        }
        self.authority.declaration_name(declaration)
    }

    /// Borrows one declaration's exact name bytes from the caller source.
    fn name_of(&self, declaration: &Decl<'source>) -> Result<&'source [u8], RustAuthorityError> {
        Ok(declaration.name)
    }

    /// Builds the written-syntax same-name gate's key for one implementation:
    /// its exact written signature rather than its bare self-type name.
    /// E0428 never governs impl blocks, so the gate must not collapse two
    /// distinct legal siblings — different traits impl'd for one self type,
    /// or several inherent blocks with different members — that merely
    /// spell the same self type. It must still collapse two byte-identical
    /// written twins straddling an unevaluated tool-attribute cfg such as
    /// `#[rustversion::since]`/`#[rustversion::before]` (a pattern `anyhow`,
    /// `thiserror`, `serde`, `proc-macro2`, and `semver` all use), so every
    /// axis the declaration identity itself later discriminates on enters
    /// this key too: the trait spelling (or its absence, for an inherent
    /// block), the self type spelling, the written generic parameter and
    /// where-clause text, and the ordered list of associated item names.
    /// A twin pair writes byte-identical text on both branches by
    /// definition, so it produces one key and collapses exactly as before;
    /// two siblings that differ on any of these axes produce distinct keys
    /// and both survive to reach the trait/generics/member-aware identity
    /// pass.
    fn impl_signature_key(
        &self,
        declaration: &RustDeclaration,
        self_type: &[u8],
    ) -> Result<Vec<u8>, RustAuthorityError> {
        fn push_optional(out: &mut Vec<u8>, spelling: Option<&[u8]>) {
            match spelling {
                Some(spelling) => {
                    out.push(1);
                    out.extend_from_slice(&(spelling.len() as u64).to_le_bytes());
                    out.extend_from_slice(spelling);
                }
                None => out.push(0),
            }
        }

        let mut key = Vec::with_capacity(64 + self_type.len());
        let Some(item) = ast::Impl::cast(declaration.syntax.clone()) else {
            // No borrowable written syntax (a macro-expansion projection):
            // the self type spelling is the only honest signature left.
            key.extend_from_slice(self_type);
            return Ok(key);
        };
        match item.trait_() {
            Some(trait_ty) => push_optional(&mut key, Some(self.bytes_of_node(trait_ty.syntax())?)),
            None => push_optional(&mut key, None),
        }
        push_optional(&mut key, Some(self_type));
        match item.generic_param_list() {
            Some(generics) => push_optional(&mut key, Some(self.bytes_of_node(generics.syntax())?)),
            None => push_optional(&mut key, None),
        }
        match item.where_clause() {
            Some(where_clause) => {
                push_optional(&mut key, Some(self.bytes_of_node(where_clause.syntax())?));
            }
            None => push_optional(&mut key, None),
        }
        let members: Vec<ast::AssocItem> = item
            .assoc_item_list()
            .map(|list| list.assoc_items().collect())
            .unwrap_or_default();
        key.extend_from_slice(&(members.len() as u64).to_le_bytes());
        for member in &members {
            let (tag, spelling) = match member {
                ast::AssocItem::Const(item) => (
                    0u8,
                    item.name()
                        .map(|name| self.bytes_of_node(name.syntax()))
                        .transpose()?,
                ),
                ast::AssocItem::Fn(item) => (
                    1,
                    item.name()
                        .map(|name| self.bytes_of_node(name.syntax()))
                        .transpose()?,
                ),
                ast::AssocItem::TypeAlias(item) => (
                    2,
                    item.name()
                        .map(|name| self.bytes_of_node(name.syntax()))
                        .transpose()?,
                ),
                ast::AssocItem::MacroCall(item) => (
                    3,
                    item.path()
                        .map(|path| self.bytes_of_node(path.syntax()))
                        .transpose()?,
                ),
            };
            key.push(tag);
            push_optional(&mut key, spelling);
        }
        Ok(key)
    }

    /// Borrows the written leaf name of one type position, when the position
    /// spells a path (`Option`, `std::vec::Vec` yields `Vec`).
    fn written_type_name(&self, anchor: Option<&ast::Type>) -> Option<&'source [u8]> {
        let ast::Type::PathType(path_type) = anchor? else {
            return None;
        };
        let segment = path_type.path()?.segments().last()?;
        let name_ref = segment.name_ref()?;
        let span = self.authority.span(name_ref.syntax()).ok()?;
        self.bytes_of(span).ok()
    }

    /// The unresolved record for a foreign named type: the exact written
    /// spelling when the source wrote one, otherwise the gap reason whose
    /// record lawfully carries no text cell.
    fn unresolved_record(&self, anchor: Option<&ast::Type>) -> SemanticTypeRecord<'source> {
        match self.written_type_name(anchor) {
            Some(text) => unknown_record(TypeReason::UnresolvedExternal, Some(text)),
            None => unknown_record(TypeReason::OracleGap, None),
        }
    }

    /// Collects every macro invocation site once: written path spelling,
    /// exact call span, and whether rust-analyzer resolved the definition.
    fn collect_macro_sites(&mut self) -> Result<(), RustAuthorityError> {
        let authority = self.authority;
        for call in authority.macro_calls() {
            let Some(path) = call.path() else {
                continue;
            };
            let spelling = self.bytes_of_node(path.syntax())?;
            // The occurrence site is the written macro path — the name a
            // reader searched for — never the whole invocation call, whose
            // argument text can carry arbitrary bytes.
            let span = authority.span(path.syntax())?;
            let resolved = authority.resolve_macro(&call).is_some();
            self.macro_sites.push(MacroSite {
                spelling,
                span,
                resolved,
            });
        }
        Ok(())
    }

    /// Reserves every module, record, enum, and trait with its legal diagonal
    /// self-nominal, so any later type can name any type root. The reservation
    /// pass carries no extension, because a local trait declared later in the
    /// file has no ordinal yet when an earlier root's bounds are built; the
    /// second pass rebuilds each root's real extension after every local
    /// ordinal exists and reattaches it against the exact parameter range
    /// captured at that declaration's own close point.
    fn emit_type_roots(
        &mut self,
        declarations: &[Decl<'source>],
    ) -> Result<(), RustAuthorityError> {
        let placeholder = self.empty_extension(RustOwnership::Value)?;
        for index in 0..declarations.len() {
            let kind = declarations[index].kind;
            if !is_type_root(kind) {
                continue;
            }
            let declaration = &declarations[index];
            let mut fact = SemanticFact::new(
                entity_kind(kind)?,
                self.name_of(declaration)?,
                constructor(kind)?,
            )
            .with_visibility(self.declaration_visibility(declaration));
            if kind != SemanticKind::Module {
                let own_ordinal = coordinate(self.facts.len())?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                record.nominal = Some(NominalRef::Local(EntityId::new(own_ordinal)));
                fact = fact.typed(record);
            }
            let ordinal = push(self.facts, fact)?;
            self.register(ordinal, index, declaration, placeholder)?;
        }
        for index in 0..declarations.len() {
            let kind = declarations[index].kind;
            if !is_type_root(kind) {
                continue;
            }
            let declaration = &declarations[index];
            let extension = self.base_extension(RustOwnership::Value, declaration)?;
            let Some(ordinal) = self.ordinals.get(index).copied().flatten() else {
                continue;
            };
            let range = self
                .facts
                .type_parameter_range(extension.where_clauses.raw)
                .map_err(|_| admission())?;
            let free_range = self
                .facts
                .free_predicate_range(extension.free_predicates.raw)
                .map_err(|_| admission())?;
            let slot = usize::try_from(ordinal).map_err(|_| admission())?;
            self.facts
                .attach_extension_with_type_parameters(
                    slot,
                    EmissionExtension::Rust(extension),
                    range,
                )
                .map_err(|_| admission())?;
            self.facts
                .set_free_predicate_range(slot, &EmissionExtension::Rust(extension), free_range)
                .map_err(|_| admission())?;
            if let Some(row) = self.rows.iter_mut().find(|row| row.ordinal == ordinal) {
                row.extension = extension;
            }
        }
        Ok(())
    }

    /// Pass two: fields, variants, implementations, callables, aliases, and
    /// value bindings in source order, each with its projected declared type.
    fn emit_members(&mut self, declarations: &[Decl<'source>]) -> Result<(), RustAuthorityError> {
        for index in 0..declarations.len() {
            let declaration = &declarations[index];
            match declaration.kind {
                SemanticKind::Field => self.emit_field(index, declaration)?,
                SemanticKind::Variant => self.emit_variant(index, declaration)?,
                SemanticKind::Implementation => self.emit_implementation(index, declaration)?,
                SemanticKind::Function => self.emit_function(index, declaration)?,
                SemanticKind::TypeAlias | SemanticKind::Constant | SemanticKind::Static => {
                    self.emit_binding(index, declaration)?
                }
                SemanticKind::Macro => self.emit_macro(index, declaration)?,
                SemanticKind::Module
                | SemanticKind::Record
                | SemanticKind::Enum
                | SemanticKind::Trait
                | SemanticKind::LocalBinding
                | SemanticKind::GenericParameter
                | SemanticKind::Builtin => {}
            }
        }
        Ok(())
    }

    /// Emits one record field with its HIR-projected declared type.
    fn emit_field(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Field(field) = &declaration.definition else {
            return Ok(());
        };
        let semantic = field.ty(self.database);
        let anchor = if declaration.expanded {
            None
        } else {
            ast::RecordField::cast(declaration.syntax.clone()).and_then(|item| item.ty())
        };
        let lowered = self.lower_pending_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
        self.push_typed(index, declaration, lowered, RustOwnership::Value)
    }

    /// Emits one enum variant as the constructor it is: its declared type is
    /// the `FunctionPointer` over one hosted row per field type plus the
    /// parent enum's fact ordinal, with the result flag set. A variant whose
    /// field run cannot fit the bounded type-child lane, or whose parent enum
    /// carries no lane ordinal, keeps the parent enum's plain nominal row
    /// instead — still proven, just not the constructor signature.
    fn emit_variant(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Variant(variant) = &declaration.definition else {
            return Ok(());
        };
        let parent = variant.parent_enum(self.database);
        let parent_ordinal = self.ordinal_of_adt(ra_ap_hir::Adt::from(parent));
        let fields = variant.fields(self.database);
        let commits_signature = parent_ordinal.is_some() && fields.len() + 1 <= MAX_TYPE_CHILDREN;
        if !commits_signature {
            let record = match parent_ordinal {
                Some(ordinal) => {
                    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                    record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
                    record
                }
                None => unknown_record(TypeReason::OracleGap, None),
            };
            return self.push_typed(
                index,
                declaration,
                Lowered::leaf(record),
                RustOwnership::Value,
            );
        }
        let variant_syntax = if declaration.expanded {
            None
        } else {
            ast::Variant::cast(declaration.syntax.clone())
        };
        let mut children = Vec::new();
        for (position, field) in fields.iter().enumerate() {
            let semantic = field.ty(self.database);
            let anchor = variant_syntax
                .as_ref()
                .and_then(|variant| variant_field_anchor(variant, position));
            match self.lower_target(&semantic, anchor, MAX_TYPE_DEPTH - 1)? {
                Some(target) => children.push(target),
                None => {
                    let record = match parent_ordinal {
                        Some(ordinal) => {
                            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                            record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
                            record
                        }
                        None => unknown_record(TypeReason::OracleGap, None),
                    };
                    return self.push_typed(
                        index,
                        declaration,
                        Lowered::leaf(record),
                        RustOwnership::Value,
                    );
                }
            }
        }
        children.push(parent_ordinal.unwrap_or_default());
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        self.push_typed(
            index,
            declaration,
            Lowered { record, children },
            RustOwnership::Value,
        )
    }

    /// Emits one inherent or trait implementation with its self type. The
    /// trait reference is recorded beside the fact so coordinate-free
    /// identity can distinguish `impl Trait for Self` splits that share one
    /// written self type; an inherent implementation records its distinct
    /// tag instead of a fabricated trait shape.
    fn emit_implementation(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Implementation(implementation) = &declaration.definition else {
            return Ok(());
        };
        let semantic = implementation.self_ty(self.database);
        let anchor = if declaration.expanded {
            None
        } else {
            ast::Impl::cast(declaration.syntax.clone()).and_then(|item| item.self_ty())
        };
        let lowered = self.lower_pending_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
        let trait_ref = self.impl_trait_ref(declaration)?;
        self.push_typed(index, declaration, lowered, RustOwnership::Value)?;
        if let Some(trait_ref) = trait_ref
            && let Some(Some(ordinal)) = self.ordinals.get(index).copied()
        {
            self.facts
                .set_impl_trait(ordinal, trait_ref)
                .map_err(|_| admission())?;
        }
        Ok(())
    }

    /// Resolves one implementation's trait reference without inventing a
    /// shape. An inherent block keeps its tag; a trait block naming a
    /// committed lane-local trait keeps that ordinal, while a foreign or
    /// unresolved trait keeps its exact written spelling. `None` means the
    /// expanded syntax owns no borrowable spelling, so identity falls back
    /// to the inherent tag rather than manufacturing bytes.
    fn impl_trait_ref(
        &mut self,
        declaration: &Decl<'source>,
    ) -> Result<Option<crate::driver::lower::StagedImplTrait<'source>>, RustAuthorityError> {
        if declaration.expanded {
            return Ok(None);
        }
        let Some(item) = ast::Impl::cast(declaration.syntax.clone()) else {
            return Ok(None);
        };
        let Some(trait_ty) = item.trait_() else {
            return Ok(Some(crate::driver::lower::StagedImplTrait::Inherent));
        };
        match self.trait_bound_constraint(&trait_ty)? {
            TraitBoundTarget::Committed(ordinal) => {
                Ok(Some(crate::driver::lower::StagedImplTrait::Local {
                    target: ordinal,
                    spelling: self.bytes_of_node(trait_ty.syntax())?,
                }))
            }
            TraitBoundTarget::Foreign | TraitBoundTarget::Unresolved => {
                let spelling = self.bytes_of_node(trait_ty.syntax())?;
                Ok(Some(crate::driver::lower::StagedImplTrait::Foreign(
                    spelling,
                )))
            }
        }
    }

    /// Emits one type alias, constant, or static binding from its HIR
    /// semantic type, anchored on the written type when the source spells
    /// one. `static mut` carries the mutable-borrow ownership cell; every
    /// other binding is a plain value.
    fn emit_binding(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let semantic = declaration.definition.semantic_type(self.database).ok_or(
            RustAuthorityError::MissingSemanticFact {
                fact: declaration.kind,
            },
        )?;
        let anchor = if declaration.expanded {
            None
        } else {
            written_binding_type(&declaration.syntax)
        };
        let ownership = match &declaration.definition {
            RustDefinition::Static(static_) if static_.is_mut(self.database) => {
                RustOwnership::MutableBorrow
            }
            _ => RustOwnership::Value,
        };
        let lowered = self.lower_pending_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
        self.push_typed(index, declaration, lowered, ownership)
    }

    /// Emits one `macro_rules!` definition with the honest unwritten type:
    /// the lane proves the macro's name and extent, never a replacement-list
    /// type, so the fact keeps the unannotated record exactly as the C lane
    /// keeps its macros. The written visibility prefix stays the lane's only
    /// visibility witness (`#[macro_export]` is not a written `pub`).
    fn emit_macro(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let extension = self.base_extension(RustOwnership::Value, declaration)?;
        let fact = SemanticFact::new(
            EntityKind::Macro,
            self.name_of(declaration)?,
            constructor(declaration.kind)?,
        )
        .with_visibility(self.declaration_visibility(declaration))
        .typed(unknown_record(TypeReason::Unannotated, None))
        .with_extension(EmissionExtension::Rust(extension));
        let ordinal = push(self.facts, fact)?;
        self.register(ordinal, index, declaration, extension)
    }

    /// Emits one function: its receiver, parameters, and result slot first,
    /// then the function fact whose product and `FunctionPointer` type
    /// children are exactly those rows.
    fn emit_function(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Function(function) = &declaration.definition else {
            return Ok(());
        };
        let fn_item = if declaration.expanded {
            None
        } else {
            ast::Fn::cast(declaration.syntax.clone())
        };
        if fn_item.is_none() && !declaration.expanded {
            return Err(RustAuthorityError::MissingSemanticFact {
                fact: SemanticKind::Function,
            });
        }
        let mut parameter_ordinals = Vec::new();
        let mut signature_children = Vec::new();

        if let Some(receiver) = function.self_param(self.database) {
            let semantic = receiver.ty(self.database);
            let lowered = self.lower_pending_type(&semantic, None, MAX_TYPE_DEPTH)?;
            let ownership = receiver_ownership(receiver.access(self.database));
            let ordinal = self.push_parameter(SELF_NAME, lowered, ownership, None)?;
            parameter_ordinals.push(ordinal);
            signature_children.push(ordinal);
        }

        let parameters = function.params_without_self(self.database);
        let written: Vec<ast::Param> = fn_item
            .as_ref()
            .and_then(|item| item.param_list())
            .map(|list| list.params().collect())
            .unwrap_or_default();
        for (position, parameter) in parameters.iter().enumerate() {
            let semantic = parameter.ty().clone();
            let anchor = written.get(position).and_then(|param| param.ty());
            let name = self.parameter_name(
                written.get(position),
                parameter.name(self.database),
                position,
            );
            let lowered = self.lower_pending_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
            let ownership = parameter_ownership(self.database, &semantic);
            let wildcard = name == b"_";
            let ordinal =
                self.push_parameter(name, lowered, ownership, wildcard.then_some(position))?;
            parameter_ordinals.push(ordinal);
            signature_children.push(ordinal);
        }

        let returns = function.ret_type(self.database);
        let result_anchor = fn_item
            .as_ref()
            .and_then(|item| item.ret_type())
            .and_then(|ret| ret.ty());
        let lowered = self.lower_pending_type(&returns, result_anchor.as_ref(), MAX_TYPE_DEPTH)?;
        let mut fact = SemanticFact::new(
            EntityKind::Parameter,
            self.name_of(declaration)?,
            LEAF_PRODUCT,
        )
        .with_visibility(self.declaration_visibility(declaration))
        .typed(lowered.record)
        .with_extension(EmissionExtension::Rust(
            self.empty_extension(RustOwnership::Value)?,
        ));
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        let result_ordinal = coordinate(push(self.facts, fact)?)?;
        signature_children.push(result_ordinal);

        let arity = coordinate(parameter_ordinals.len())?;
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        let extension = self.base_extension(RustOwnership::Value, declaration)?;
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            self.name_of(declaration)?,
            SemanticProductConstructor::function(arity, 1),
        )
        .with_visibility(self.declaration_visibility(declaration))
        .typed(record)
        .with_extension(EmissionExtension::Rust(extension));
        for ordinal in &parameter_ordinals {
            fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
        }
        fact = fact.child(ProductChildRole::FunctionResult, result_ordinal);
        for ordinal in &signature_children {
            fact = fact.type_child(*ordinal, None, 0);
        }
        let ordinal = push(self.facts, fact)?;
        // Signature carriers share names and types across functions; bind
        // each to its executable so identical carriers stay distinct.
        let executable = coordinate(ordinal)?;
        let name_len = declaration.name.len();
        for carrier in parameter_ordinals.iter().copied().chain([result_ordinal]) {
            self.facts
                .attach_parent(carrier, executable)
                .map_err(|fault| parentage_fault(carrier, name_len, fault))?;
        }
        self.register(ordinal, index, declaration, extension)
    }

    /// Pushes one signature carrier fact (receiver or parameter) with its
    /// lowered record and the ownership-only extension row its position owns.
    fn push_parameter(
        &mut self,
        name: &'source [u8],
        lowered: Lowered<'source>,
        ownership: RustOwnership,
        wildcard_position: Option<usize>,
    ) -> Result<u32, RustAuthorityError> {
        let mut fact = SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
            .typed(lowered.record)
            .with_extension(EmissionExtension::Rust(self.empty_extension(ownership)?));
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        if let Some(position) = wildcard_position {
            fact = fact.with_identity_discriminator(wildcard_param_discriminator(position));
        }
        coordinate(push(self.facts, fact)?)
    }

    /// Borrows one written parameter name: the binding identifier when the
    /// pattern spells one, otherwise the analyzer's own parameter name found
    /// in the source text, otherwise the static fallback binding name. A
    /// wildcard pattern binds nothing, so two same-typed `_` parameters would
    /// otherwise mint byte-identical siblings; the display name stays `_` and
    /// the positional index is carried in the identity discriminator so a
    /// wildcard cannot collide with a parameter the source actually named
    /// `_0`.
    fn parameter_name(
        &self,
        written: Option<&ast::Param>,
        hir_name: Option<ra_ap_hir::Name>,
        _position: usize,
    ) -> &'source [u8] {
        if let Some(param) = written {
            if let Some(pattern) = param.pat() {
                if let ast::Pat::IdentPat(ident) = &pattern
                    && let Some(name) = ident.name()
                    && let Ok(bytes) = self.bytes_of_node(name.syntax())
                {
                    return bytes;
                }
                if matches!(&pattern, ast::Pat::WildcardPat(_)) {
                    return b"_";
                }
                if let Ok(bytes) = self.bytes_of_node(pattern.syntax()) {
                    return bytes;
                }
            }
        }
        if let Some(name) = hir_name
            && let Some(span) = self.span_of_text(name.as_str())
            && let Ok(bytes) = self.bytes_of(span)
        {
            return bytes;
        }
        PARAM_FALLBACK_NAME
    }

    /// Captures only the written visibility prefix; omitted visibility is Rust-private.
    fn declaration_visibility(
        &self,
        declaration: &Decl<'source>,
    ) -> backend_semantic::ir::Visibility {
        if declaration.expanded {
            return backend_semantic::ir::Visibility::Unknown;
        }
        let Some(visibility) = ast::AnyHasVisibility::cast(declaration.syntax.clone())
            .and_then(|item| item.visibility())
        else {
            return backend_semantic::ir::Visibility::Private;
        };
        let range = visibility.syntax().text_range();
        let Some(start) = usize::try_from(u32::from(range.start())).ok() else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        let Some(end) = usize::try_from(u32::from(range.end())).ok() else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        let Some(bytes) = self.source.get(start..end) else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        if bytes == b"pub(crate)" {
            backend_semantic::ir::Visibility::Package
        } else if bytes.starts_with(b"pub(") {
            backend_semantic::ir::Visibility::Restricted
        } else {
            backend_semantic::ir::Visibility::Public
        }
    }

    /// Pushes one member fact with a projected record and its base Rust
    /// extension row, then registers the row.
    fn push_typed(
        &mut self,
        index: usize,
        declaration: &Decl<'source>,
        lowered: Lowered<'source>,
        ownership: RustOwnership,
    ) -> Result<(), RustAuthorityError> {
        let extension = self.base_extension(ownership, declaration)?;
        let mut fact = SemanticFact::new(
            entity_kind(declaration.kind)?,
            self.name_of(declaration)?,
            constructor(declaration.kind)?,
        )
        .with_visibility(self.declaration_visibility(declaration))
        .typed(lowered.record)
        .with_extension(EmissionExtension::Rust(extension));
        if let Some(discriminator) = self.expanded_impl_discriminator(declaration) {
            fact = fact.with_identity_discriminator(discriminator);
        }
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        let ordinal = push(self.facts, fact)?;
        self.register(ordinal, index, declaration, extension)
    }

    /// Mints the identity discriminator of one macro-expanded implementation
    /// from the expansion's own trait and self-type spelling. A macro that
    /// stamps several impl blocks for one self type projects them all onto the
    /// single invocation span, so the written projections cannot separate
    /// them: the whole-item span cannot frame a per-impl trait reference, and
    /// member names have no same-source spelling at a shared call-site range.
    /// The expansion buffer is the authority's own projected Rust syntax for
    /// those impls, so the header it spells is authority truth; only its
    /// domain-separated digest enters identity, keeping every written impl's
    /// variant byte-identical.
    fn expanded_impl_discriminator(&self, declaration: &Decl<'source>) -> Option<[u8; 16]> {
        if !declaration.expanded {
            return None;
        }
        let item = ast::Impl::cast(declaration.syntax.clone())?;
        let trait_spelling = item
            .trait_()
            .map(|trait_ty| trait_ty.syntax().to_string())
            .unwrap_or_else(|| "inherent".to_owned());
        let self_spelling = item.self_ty()?.syntax().to_string();
        let mut hash = Sha256::new();
        hash.update(b"compiler.rust.expanded-impl.v1\0");
        hash.update(trait_spelling.as_bytes());
        hash.update([0]);
        hash.update(self_spelling.as_bytes());
        let mut discriminator = [0_u8; 16];
        discriminator.copy_from_slice(&hash.finalize()[..16]);
        Some(discriminator)
    }

    /// Builds one declaration's base Rust extension row: the ownership cell,
    /// its written lifetime spellings, and its where-clause coordinate. The
    /// coordinate is the shared zero-based staging start; its exact length is
    /// captured with the extension transaction, so an empty list never needs
    /// a language-specific one-based sentinel. Macro spellings attach
    /// in a later phase. Expansion-derived declarations carry the empty
    /// lifetime list and no bounds, because their generic syntax lives in
    /// the expansion buffer, not this source.
    fn base_extension(
        &mut self,
        ownership: RustOwnership,
        declaration: &Decl<'source>,
    ) -> Result<RustFacts, RustAuthorityError> {
        if declaration.expanded {
            return self.empty_extension(ownership);
        }
        let syntax = &declaration.syntax;
        let lifetimes = self.lifetime_atoms(syntax)?;
        let free_start = coordinate(self.facts.free_predicate_len)?;
        let (where_clauses, const_defaults) =
            match self.where_rows(&declaration.definition, syntax)? {
                Some((start, list)) => (TypeParameterListId::new(start), list),
                None => (
                    TypeParameterListId::new(coordinate(self.facts.type_parameter_len)?),
                    AtomListId::new(0),
                ),
            };
        Ok(RustFacts {
            ownership,
            lifetimes,
            where_clauses,
            macros: AtomListId::new(0),
            const_defaults,
            free_predicates: backend_semantic::ir::FreePredicateListId::new(free_start),
        })
    }

    /// Builds the ownership-only extension row of a signature carrier: no
    /// lifetimes, no where rows of its own, and no macro spellings yet.
    fn empty_extension(
        &mut self,
        ownership: RustOwnership,
    ) -> Result<RustFacts, RustAuthorityError> {
        let empty_atoms = self.facts.intern_atom_list(&[]).map_err(|_| admission())?;
        Ok(RustFacts {
            ownership,
            lifetimes: empty_atoms,
            where_clauses: TypeParameterListId::new(coordinate(self.facts.type_parameter_len)?),
            macros: empty_atoms,
            const_defaults: empty_atoms,
            free_predicates: backend_semantic::ir::FreePredicateListId::new(coordinate(
                self.facts.free_predicate_len,
            )?),
        })
    }

    /// Interns one declaration's written lifetime parameters as extension
    /// atoms and returns their pooled list.
    fn lifetime_atoms(&mut self, syntax: &SyntaxNode) -> Result<AtomListId, RustAuthorityError> {
        let mut atoms = Vec::new();
        let generics = ast::AnyHasGenericParams::cast(syntax.clone());
        if let Some(list) = generics.as_ref().and_then(|item| item.generic_param_list()) {
            for generic in list.generic_params() {
                let ast::GenericParam::LifetimeParam(lifetime) = generic else {
                    continue;
                };
                let Some(lifetime) = lifetime.lifetime() else {
                    continue;
                };
                let spelling = self.bytes_of_node(lifetime.syntax())?;
                let atom = self.facts.intern_atom(spelling).map_err(|_| admission())?;
                atoms.push(atom);
            }
        }
        self.facts.intern_atom_list(&atoms).map_err(|_| admission())
    }

    /// Pushes exactly one pooled row per HIR generic parameter, in written
    /// order, and preserves each written bound run. HIR gives the ordered
    /// parameter set and kinds (including `Self` and argument `impl Trait`,
    /// which have no written row and are skipped); the exact `T: A + B`
    /// spelling and order come from the mapped written `type_bound_list`.
    /// Inline bounds come first, then every matching `where` predicate in
    /// source order, merged into that parameter's single run so a bound is
    /// never duplicated onto a second parameter row. A lifetime parameter
    /// keeps its lifetime bounds, a const parameter keeps its declared type,
    /// and a written parameter with no HIR counterpart (or a kind mismatch) is
    /// an exact unsupported terminal rather than a shifted or invented row.
    /// A `where` predicate whose subject is not a declared parameter lowers
    /// into the free-predicate lane instead of erasing the predicate.
    /// Const-generic value defaults are preserved as their exact written
    /// expression spellings in a suffix atom list paired with the trailing
    /// const parameters; because Rust makes every declared default trailing
    /// (E0128), no const parameter without a default can follow one that has a
    /// default, and no empty atom is ever emitted.
    fn where_rows(
        &mut self,
        definition: &RustDefinition,
        syntax: &SyntaxNode,
    ) -> Result<Option<(u32, AtomListId)>, RustAuthorityError> {
        let start = coordinate(self.facts.type_parameter_len)?;
        let generics = ast::AnyHasGenericParams::cast(syntax.clone());
        let written: Vec<ast::GenericParam> = generics
            .as_ref()
            .and_then(|generics| generics.generic_param_list())
            .map(|list| list.generic_params().collect())
            .unwrap_or_default();
        let mut written = written.into_iter();
        let mut parameters_written = false;
        // Const-generic value defaults are preserved as their exact written
        // expression spellings, in written order, with a const parameter that
        // declares no default contributing nothing. Rust requires every
        // generic parameter with a default to be trailing (E0128), so the
        // declared const defaults are always a suffix of the const parameter
        // sequence; a consumer pairs this list with that suffix. A parameter
        // without a default is never given an empty atom, which the fragment
        // wire format rejects as invalid.
        let mut const_atoms: Vec<Option<u32>> = Vec::new();
        // Stage every `where` predicate bound in source order, keyed by its
        // written subject spelling, before pairing parameters. A predicate
        // augments a declared parameter when its subject is a simple
        // parameter path (`T`, `'a`); any other subject (`Vec<T>`,
        // `T::Item`, `Self`, and every higher-ranked `for<'a> &'a L: Into`
        // predicate, whose binder names no declaration-scoped row) stages
        // for the free-predicate lane with its exact written subject and
        // bound.
        let mut where_bounds: Vec<WhereBound<'source>> = Vec::new();
        if let Some(where_clause) = generics
            .as_ref()
            .and_then(|generics| generics.where_clause())
        {
            for predicate in where_clause.predicates() {
                let (subject, ty) = if let Some(lifetime) = predicate.lifetime() {
                    (self.bytes_of_node(lifetime.syntax())?, None)
                } else if let Some(ty) = predicate.ty() {
                    let subject = match self.simple_parameter_subject(&ty) {
                        Some(subject) => subject,
                        None => self.bytes_of_node(ty.syntax())?,
                    };
                    (subject, Some(ty))
                } else {
                    return Err(unsupported_generic());
                };
                let Some(bounds) = predicate.type_bound_list() else {
                    return Err(unsupported_generic());
                };
                let bounds: Vec<ast::TypeBound> = bounds.bounds().collect();
                if bounds.is_empty() {
                    return Err(unsupported_generic());
                }
                for bound in bounds {
                    where_bounds.push(WhereBound {
                        subject,
                        ty: ty.clone(),
                        bound,
                        matched: false,
                    });
                }
            }
        }
        // HIR lists lifetimes before type/const parameters regardless of
        // written order, and positional pairing would silently misalign
        // `fn f<T, 'a>`. Pair in written order by kind and name instead:
        // implicit HIR parameters (`Self`, `impl Trait` arguments) have no
        // written row and never pair.
        let mut hir: Vec<ra_ap_hir::GenericParam> = Vec::new();
        for parameter in definition.generic_params(self.database) {
            if matches!(
                parameter,
                ra_ap_hir::GenericParam::TypeParam(type_param)
                    if type_param.is_implicit(self.database)
            ) {
                continue;
            }
            hir.push(parameter);
        }
        for written in written.by_ref() {
            // Written bytes of the parameter name for HIR pairing.
            // Lifetimes keep their leading quote (`'a`); raw identifiers
            // shed `r#` to match HIR's unescaped symbol text.
            let (kind, spelled) = match &written {
                ast::GenericParam::TypeParam(param) => {
                    let Some(name) = param.name() else {
                        continue;
                    };
                    (0_u8, self.bytes_of_node(name.syntax())?)
                }
                ast::GenericParam::ConstParam(param) => {
                    let Some(name) = param.name() else {
                        continue;
                    };
                    (1_u8, self.bytes_of_node(name.syntax())?)
                }
                ast::GenericParam::LifetimeParam(param) => {
                    let Some(lifetime) = param.lifetime() else {
                        continue;
                    };
                    (2_u8, self.bytes_of_node(lifetime.syntax())?)
                }
            };
            let spelled = spelled.strip_prefix(b"r#").unwrap_or(spelled);
            let Some(position) = hir.iter().position(|parameter| {
                let (hir_kind, hir_name) = match parameter {
                    ra_ap_hir::GenericParam::TypeParam(_) => (0_u8, parameter.name(self.database)),
                    ra_ap_hir::GenericParam::ConstParam(_) => (1_u8, parameter.name(self.database)),
                    ra_ap_hir::GenericParam::LifetimeParam(_) => {
                        (2_u8, parameter.name(self.database))
                    }
                };
                hir_kind == kind && hir_name.as_str().as_bytes() == spelled
            }) else {
                return Err(unsupported_generic());
            };
            let parameter = hir.remove(position);
            match (parameter, written) {
                (
                    ra_ap_hir::GenericParam::TypeParam(type_param),
                    ast::GenericParam::TypeParam(param),
                ) => {
                    let Some(name) = param.name() else {
                        continue;
                    };
                    let name = self.bytes_of_node(name.syntax())?;
                    let bounds = param
                        .type_bound_list()
                        .map(|bounds| bounds.bounds().collect::<Vec<_>>())
                        .unwrap_or_default();
                    // A default type binds the declaration-scoped parameter
                    // it augments. Lower it through the same target path as
                    // any other written type so local nominals stay ordinal
                    // references and foreign spellings stay unresolved
                    // externals; a default the lane cannot encode is an
                    // exact terminal, never an erased None.
                    let default = match (param.default_type(), type_param.default(self.database)) {
                        (None, None) => None,
                        (Some(written), semantic) => {
                            Some(self.default_type_target(&written, semantic.as_ref())?)
                        }
                        // A HIR default with no written row cannot be placed
                        // without inventing source evidence.
                        (None, Some(_)) => return Err(unsupported_generic()),
                    };
                    let mut staged_bounds = Vec::with_capacity(bounds.len());
                    for bound in &bounds {
                        staged_bounds.push(self.staged_bound(bound)?);
                    }
                    for where_bound in where_bounds.iter_mut() {
                        if where_bound.subject == name {
                            let staged = self.staged_bound(&where_bound.bound)?;
                            staged_bounds.push(staged);
                            where_bound.matched = true;
                        }
                    }
                    self.facts
                        .push_type_parameter_with_bounds(
                            name,
                            &staged_bounds,
                            default,
                            backend_semantic::ir::ExtensionTypeParameterKind::Type {
                                inference: backend_semantic::ir::TypeParameterInference::Ordinary,
                            },
                            backend_semantic::ir::Variance::Invariant,
                            backend_semantic::ir::TypeParameterRequirements::none(),
                        )
                        .map_err(|_| admission())?;
                    parameters_written = true;
                }
                (ra_ap_hir::GenericParam::ConstParam(_), ast::GenericParam::ConstParam(param)) => {
                    let Some(name) = param.name() else {
                        continue;
                    };
                    // A const value default is preserved as its exact written
                    // expression spelling; a parameter without one contributes
                    // no atom to the defaults suffix.
                    let default_atom = match param.default_val() {
                        Some(value) => {
                            let spelling = self.bytes_of_node(value.syntax())?;
                            Some(self.facts.intern_atom(spelling).map_err(|_| admission())?)
                        }
                        None => None,
                    };
                    let Some(value_type) = param.ty() else {
                        continue;
                    };
                    let spelling = self.bytes_of_node(value_type.syntax())?;
                    let value_type = self
                        .host(
                            Lowered::leaf(unknown_record(
                                TypeReason::NoIrRepresentation,
                                Some(spelling),
                            )),
                            Some(&value_type),
                        )?
                        .ok_or_else(admission)?;
                    self.facts
                        .push_type_parameter_with_bounds(
                            self.bytes_of_node(name.syntax())?,
                            &[],
                            None,
                            backend_semantic::ir::ExtensionTypeParameterKind::ConstValue {
                                value_type,
                            },
                            backend_semantic::ir::Variance::Invariant,
                            backend_semantic::ir::TypeParameterRequirements::none(),
                        )
                        .map_err(|_| admission())?;
                    const_atoms.push(default_atom);
                    parameters_written = true;
                }
                (
                    ra_ap_hir::GenericParam::LifetimeParam(_),
                    ast::GenericParam::LifetimeParam(param),
                ) => {
                    let Some(lifetime) = param.lifetime() else {
                        continue;
                    };
                    let lifetime_bytes = self.bytes_of_node(lifetime.syntax())?;
                    let bounds = param
                        .type_bound_list()
                        .map(|bounds| bounds.bounds().collect::<Vec<_>>())
                        .unwrap_or_default();
                    let mut staged_bounds = Vec::with_capacity(bounds.len());
                    for bound in &bounds {
                        let Some(bound_lifetime) = bound.lifetime() else {
                            return Err(unsupported_generic());
                        };
                        staged_bounds.push(
                            backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(
                                self.bytes_of_node(bound_lifetime.syntax())?,
                            ),
                        );
                    }
                    for where_bound in where_bounds.iter_mut() {
                        if where_bound.subject == lifetime_bytes {
                            let Some(bound_lifetime) = where_bound.bound.lifetime() else {
                                return Err(unsupported_generic());
                            };
                            let staged =
                                backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(
                                    self.bytes_of_node(bound_lifetime.syntax())?,
                                );
                            staged_bounds.push(staged);
                            where_bound.matched = true;
                        }
                    }
                    self.facts
                        .push_type_parameter_with_bounds(
                            lifetime_bytes,
                            &staged_bounds,
                            None,
                            backend_semantic::ir::ExtensionTypeParameterKind::Lifetime,
                            backend_semantic::ir::Variance::Invariant,
                            backend_semantic::ir::TypeParameterRequirements::none(),
                        )
                        .map_err(|_| admission())?;
                    parameters_written = true;
                }
                // A written parameter whose HIR kind disagrees, or a written
                // parameter the HIR does not model, cannot be placed without
                // inventing a declaration-scoped binding.
                _ => return Err(unsupported_generic()),
            }
        }
        if !hir.is_empty() {
            // A modeled HIR parameter with no written row cannot be placed
            // without inventing a declaration-scoped binding.
            return Err(unsupported_generic());
        }
        for where_bound in where_bounds.iter().filter(|bound| !bound.matched) {
            // A `where` predicate whose subject is not a declared parameter
            // (`Vec<T>: Clone`, `T::Item: Clone`, `Self: Sized`) lowers its
            // subject type and ordered bound into the free-predicate lane. A
            // lifetime subject has no subject type row and stays an exact
            // terminal rather than being erased.
            let Some(ty) = where_bound.ty.clone() else {
                return Err(unsupported_generic());
            };
            let subject = self.free_subject_target(&ty)?;
            let bound = self.staged_bound(&where_bound.bound)?;
            self.facts
                .push_free_predicate(subject, &[bound])
                .map_err(|_| admission())?;
        }
        // E0128 makes every declared default trailing, so a const parameter
        // without a default may never follow one that declares a default. A
        // producer that violates the invariant cannot be paired with the
        // suffix list and stays an exact terminal rather than shifting it.
        if let Some(first) = const_atoms.iter().position(Option::is_some)
            && const_atoms[first..].iter().any(Option::is_none)
        {
            return Err(unsupported_generic());
        }
        let const_atoms: Vec<u32> = const_atoms.into_iter().flatten().collect();
        let const_defaults = self
            .facts
            .intern_atom_list(&const_atoms)
            .map_err(|_| admission())?;
        Ok(parameters_written.then_some((start, const_defaults)))
    }

    /// Borrows the written subject of one `where` predicate that augments a
    /// declared parameter: a path with a single, argument-free segment. A
    /// qualified path (`T::Item`), an applied type (`Vec<T>`), a bare `Self`,
    /// or any non-path type has no declared-parameter row and returns `None`.
    fn simple_parameter_subject(&self, ty: &ast::Type) -> Option<&'source [u8]> {
        let ast::Type::PathType(path_type) = ty else {
            return None;
        };
        let path = path_type.path()?;
        let mut segments = path.segments();
        let segment = segments.next()?;
        if segments.next().is_some() || segment.generic_arg_list().is_some() {
            return None;
        }
        let name_ref = segment.name_ref()?;
        let span = self.authority.span(name_ref.syntax()).ok()?;
        self.bytes_of(span).ok()
    }

    /// Lowers one free-predicate subject to its hosted staged coordinate. A
    /// bare `Self` keeps its `SelfType` leaf, a simple path keeps its exact
    /// spelling as a `TypeVar` row, and any other written subject keeps its
    /// full spelling as an honest `NoIrRepresentation` unknown rather than a
    /// fabricated application shape.
    fn free_subject_target(&mut self, ty: &ast::Type) -> Result<u32, RustAuthorityError> {
        if let ast::Type::PathType(path_type) = ty
            && let Some(path) = path_type.path()
            && path.qualifier().is_none()
        {
            let mut segments = path.segments();
            if let Some(segment) = segments.next()
                && segments.next().is_none()
                && segment.generic_arg_list().is_none()
                && let Some(name_ref) = segment.name_ref()
                && let Ok(spelling) = self.bytes_of_node(name_ref.syntax())
                && spelling == b"Self"
            {
                return self
                    .host(
                        Lowered::leaf(SemanticTypeRecord::leaf(SemanticTypeTag::SelfType)),
                        None,
                    )?
                    .ok_or_else(admission);
            }
        }
        if let Some(spelling) = self.simple_parameter_subject(ty) {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
            record.text = Some(spelling);
            return self
                .host(Lowered::leaf(record), None)?
                .ok_or_else(admission);
        }
        let spelling = self.bytes_of_node(ty.syntax())?;
        self.host(
            Lowered::leaf(unknown_record(
                TypeReason::NoIrRepresentation,
                Some(spelling),
            )),
            None,
        )?
        .ok_or_else(admission)
    }

    /// Stages one written bound: a lifetime bound keeps its exact spelling, and
    /// a trait bound resolves through the same committed/foreign/unresolved
    /// lattice as an inline bound.
    fn staged_bound(
        &mut self,
        bound: &ast::TypeBound,
    ) -> Result<backend_semantic::ir::ExtensionTypeParameterBound<'source>, RustAuthorityError>
    {
        if let Some(lifetime) = bound.lifetime() {
            return Ok(backend_semantic::ir::ExtensionTypeParameterBound::Lifetime(
                self.bytes_of_node(lifetime.syntax())?,
            ));
        }
        if let Some(bound_ty) = bound.ty() {
            return Ok(backend_semantic::ir::ExtensionTypeParameterBound::Type(
                self.generic_type_bound_target(&bound_ty)?,
            ));
        }
        Err(unsupported_generic())
    }

    /// Resolves one written trait bound. A committed local trait keeps its lane
    /// ordinal; a foreign trait is named by the caller as an unresolved
    /// external row; a local trait with no reserved lane root stays an exact
    /// terminal instead of being invented as an external symbol.
    fn trait_bound_constraint(
        &mut self,
        bound_ty: &ast::Type,
    ) -> Result<TraitBoundTarget, RustAuthorityError> {
        let ast::Type::PathType(path_type) = bound_ty else {
            return Ok(TraitBoundTarget::Unresolved);
        };
        let Some(path) = path_type.path() else {
            return Ok(TraitBoundTarget::Unresolved);
        };
        let Some((ra_ap_hir::PathResolution::Def(ra_ap_hir::ModuleDef::Trait(trait_)), _)) =
            self.authority.resolve_path(&path)
        else {
            return Ok(TraitBoundTarget::Unresolved);
        };
        if let Some(ordinal) = self.ordinal_of_trait(trait_) {
            return Ok(TraitBoundTarget::Committed(ordinal));
        }
        // Every lane-admissible local trait is a reserved type root before any
        // bound resolves, so a local lookup failure means the trait has no
        // lane row at all (e.g. declared in a function body). That cannot be
        // downgraded to an external symbol, so it stays an exact terminal.
        if self
            .authority
            .definition_origin(trait_, SemanticKind::Trait)
            .is_local()
        {
            return Err(unsupported_generic());
        }
        Ok(TraitBoundTarget::Foreign)
    }

    /// Lowers one default type to its scoped row. A bare path naming a
    /// committed lane-local type keeps that type's fact ordinal, so the
    /// default binds the declaration it augments; every other form lowers
    /// through the shared target path (foreign spellings stay unresolved
    /// externals, never fabricated nominals). When the HIR lends no semantic
    /// type for an otherwise-written default (rust-analyzer does not model
    /// every alias generic default), the written spelling is preserved as an
    /// honest `NoIrRepresentation` unknown instead of rejecting the whole
    /// declaration. A default the lane cannot even borrow stays an exact
    /// terminal.
    fn default_type_target(
        &mut self,
        written: &ast::Type,
        semantic: Option<&ra_ap_hir::Type<'_>>,
    ) -> Result<u32, RustAuthorityError> {
        if let ast::Type::PathType(path_type) = written
            && let Some(path) = path_type.path()
            && path.qualifier().is_none()
            && path
                .segments()
                .all(|segment| segment.generic_arg_list().is_none())
            && let Some((ra_ap_hir::PathResolution::Def(ra_ap_hir::ModuleDef::Adt(adt)), _)) =
                self.authority.resolve_path(&path)
            && let Some(ordinal) = self.ordinal_of_adt(adt)
        {
            return Ok(ordinal);
        }
        if let Some(semantic) = semantic {
            return self
                .lower_target(semantic, Some(written.clone()), MAX_TYPE_DEPTH - 1)?
                .ok_or_else(unsupported_generic);
        }
        let spelling = self.bytes_of_node(written.syntax())?;
        self.host(
            Lowered::leaf(unknown_record(
                TypeReason::NoIrRepresentation,
                Some(spelling),
            )),
            Some(written),
        )?
        .ok_or_else(unsupported_generic)
    }

    /// Preserves every non-lifetime bound. A foreign trait keeps its exact
    /// written spelling as an unresolved external row (never a fabricated
    /// nominal); a bound form the HIR does not expose as a trait keeps its
    /// exact written spelling as an explicit `NoIrRepresentation` unknown.
    fn generic_type_bound_target(&mut self, bound: &ast::Type) -> Result<u32, RustAuthorityError> {
        match self.trait_bound_constraint(bound)? {
            TraitBoundTarget::Committed(ordinal) => {
                return self.lower_trait_bound_application(bound, ordinal, MAX_TYPE_DEPTH - 1);
            }
            TraitBoundTarget::Foreign => {
                let spelling = self.bytes_of_node(bound.syntax())?;
                let record = self.unresolved_record_with(Some(spelling));
                return self
                    .host(Lowered::leaf(record), Some(bound))?
                    .ok_or_else(unsupported_generic);
            }
            TraitBoundTarget::Unresolved => {}
        }
        let spelling = self.bytes_of_node(bound.syntax())?;
        self.host(
            Lowered::leaf(unknown_record(
                TypeReason::NoIrRepresentation,
                Some(spelling),
            )),
            Some(bound),
        )?
        .ok_or_else(unsupported_generic)
    }

    /// Registers one pushed declaration row, its HIR definition ordinal, and
    /// its lane ordinal for the later phases. The authority's whole-item span
    /// is bound here as the row's provenance span: it is the exact basis every
    /// later owner-relative occurrence span is measured against, so the
    /// shared containment law holds by construction.
    fn register(
        &mut self,
        ordinal: usize,
        index: usize,
        declaration: &Decl<'source>,
        extension: RustFacts,
    ) -> Result<(), RustAuthorityError> {
        let Some(ordinal) = u32::try_from(ordinal).ok() else {
            return Ok(());
        };
        let span = StagedSourceSpan::new(declaration.span.start, declaration.span.end)
            .ok_or_else(admission)?;
        self.facts
            .attach_source_span(ordinal, span)
            .map_err(|fault| parentage_fault(ordinal, declaration.name.len(), fault))?;
        self.rows.push(Row {
            ordinal,
            name: self.name_of(declaration)?,
            span: declaration.span,
            extension,
        });
        match &declaration.definition {
            RustDefinition::Field(field) => self.fields.push((*field, ordinal)),
            other => {
                if let Some(definition) = module_def(other) {
                    self.definitions.push((definition, ordinal));
                }
            }
        }
        if let Some(slot) = self.ordinals.get_mut(index) {
            *slot = Some(ordinal);
        }
        Ok(())
    }

    /// Looks up the pushed ordinal of one lane-local ADT.
    fn ordinal_of_adt(&self, adt: ra_ap_hir::Adt) -> Option<u32> {
        self.definitions
            .iter()
            .find(|(definition, _)| definition == &ra_ap_hir::ModuleDef::Adt(adt))
            .map(|(_, ordinal)| *ordinal)
    }

    /// Looks up the pushed ordinal of one lane-local trait.
    fn ordinal_of_trait(&self, trait_: ra_ap_hir::Trait) -> Option<u32> {
        self.definitions
            .iter()
            .find(|(definition, _)| definition == &ra_ap_hir::ModuleDef::Trait(trait_))
            .map(|(_, ordinal)| *ordinal)
    }

    /// Looks up the pushed ordinal of any pushed HIR definition.
    fn ordinal_of_definition(&self, definition: &ra_ap_hir::ModuleDef) -> Option<u32> {
        self.definitions
            .iter()
            .find(|(known, _)| known == definition)
            .map(|(_, ordinal)| *ordinal)
    }

    /// Looks up the pushed ordinal of one pushed named field.
    fn ordinal_of_field(&self, field: &ra_ap_hir::Field) -> Option<u32> {
        self.fields
            .iter()
            .find(|(known, _)| known == field)
            .map(|(_, ordinal)| *ordinal)
    }

    /// Picks the innermost pushed row whose span contains `span`.
    fn owner_of(&self, span: ByteSpan) -> Option<u32> {
        let mut low = 0;
        let mut high = self.owner_order.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let row = &self.rows[self.owner_order[middle]];
            if row.span.start <= span.start {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        self.owner_order[..low].iter().rev().find_map(|index| {
            let row = &self.rows[*index];
            (span.end <= row.span.end).then_some(row.ordinal)
        })
    }

    /// Picks the innermost strictly-containing pushed row for `span`,
    /// excluding the row itself. Siblings from macro expansion may share a
    /// span with their site; equal spans never parent each other, so such
    /// siblings resolve to their shared outer container (or root) instead
    /// of an arbitrary sibling. A row with no strictly-containing row is a
    /// parentage root.
    fn enclosing_row(&self, span: ByteSpan, ordinal: u32) -> Option<u32> {
        let mut low = 0;
        let mut high = self.owner_order.len();
        while low < high {
            let middle = low + (high - low) / 2;
            let row = &self.rows[self.owner_order[middle]];
            if row.span.start <= span.start {
                low = middle + 1;
            } else {
                high = middle;
            }
        }
        self.owner_order[..low].iter().rev().find_map(|index| {
            let row = &self.rows[*index];
            (row.ordinal != ordinal
                && span.end <= row.span.end
                && (row.span.start < span.start || span.end < row.span.end))
                .then_some(row.ordinal)
        })
    }

    /// Binds lexical parentage from declaration spans after every row
    /// exists. Only relationships whose owner survived emission are bound:
    /// a row with no strictly-containing row is an explicit parentage
    /// root, never a fabricated child. Signature carriers are bound by
    /// their emitters; anonymous hosted rows keep their honest unavailable
    /// state until a use-site owner exists.
    fn emit_parentage(&mut self) -> Result<(), RustAuthorityError> {
        for index in 0..self.rows.len() {
            let (ordinal, span, name_len) = {
                let row = &self.rows[index];
                (row.ordinal, row.span, row.name.len())
            };
            match self.enclosing_row(span, ordinal) {
                Some(parent) => self
                    .facts
                    .attach_parent(ordinal, parent)
                    .map_err(|fault| parentage_fault(ordinal, name_len, fault))?,
                None => self
                    .facts
                    .mark_parentage_root(ordinal)
                    .map_err(|fault| parentage_fault(ordinal, name_len, fault))?,
            }
        }
        Ok(())
    }

    /// Builds the source-ordered declaration index once, after all declaration
    /// rows exist. Rows are nested or disjoint in the syntax tree; reverse
    /// search therefore selects the innermost containing declaration while
    /// avoiding a full scan for every occurrence.
    fn rebuild_owner_order(&mut self) {
        self.owner_order = (0..self.rows.len()).collect();
        self.owner_order.sort_by_key(|index| {
            let span = self.rows[*index].span;
            (span.start, span.end)
        });
    }

    /// Lowers a type against the fact ordinal that the caller will push next.
    fn lower_pending_type(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        self.lower_type(semantic, anchor, depth)
    }

    /// Hosts one lowered child position and returns the backward coordinate
    /// its parent record references: a nominal leaf names its fact ordinal
    /// directly, every other leaf interns as a zero-child anonymous row
    /// (foreign unknowns deduplicated by spelling), and every compound
    /// becomes a backward `Parameter` carrier fact whose own type children
    /// are the hosted coordinates of its components. Anonymous rows never
    /// carry pooled children — the pooled lane re-lays children per row, so
    /// a child-bearing row cannot be addressed soundly — and compounds never
    /// stay rowless, so no proven shape is ever erased. `Ok(None)` means the
    /// position could not be hosted at all (no pushed fact exists yet to own
    /// a row), and the parent folds to a rowless honest unknown.
    fn host(
        &mut self,
        lowered: Lowered<'source>,
        anchor: Option<&ast::Type>,
    ) -> Result<Option<u32>, RustAuthorityError> {
        // A computed (inferred) type's nested positions are never a pending
        // declared fact: no new row follows this lowering, so the reserved-
        // anchor convention below (which assumes the caller pushes a fact at
        // exactly `self.facts.len()` immediately after) would stage an
        // anonymous row owned by an ordinal that is never fulfilled. Route
        // straight into the computed lane instead, which owns every child
        // position through the already-hosted computed row.
        if self.computed_owner.is_some() {
            return self.host_computed(lowered).map(Some);
        }
        if lowered.children.is_empty() {
            if let Some(NominalRef::Local(target)) = lowered.record.nominal {
                return Ok(Some(target.raw));
            }
            let foreign = match (lowered.record.tag, lowered.record.nominal) {
                (SemanticTypeTag::Unknown, _) => lowered.record.text.map(|text| (text, None)),
                (SemanticTypeTag::Nominal, Some(NominalRef::External(external))) => {
                    lowered.record.text.map(|text| (text, Some(external)))
                }
                _ => None,
            };
            if let Some((text, external)) = foreign {
                for (known, known_external, ordinal) in &self.foreign_rows {
                    if *known == text && *known_external == external {
                        return Ok(Some(*ordinal));
                    }
                }
                // The dedup table is a bounded lane: a distinct foreign
                // spelling beyond its cap must reject exactly, never silently
                // mint an unbounded duplicate row.
                if self.foreign_rows.len() >= MAX_DEDUPED_FOREIGN_ROWS {
                    return Err(admission());
                }
            }
            let anchor_row = coordinate(self.facts.len())?;
            let row = self
                .facts
                .intern_reserved_anchor_type_row(anchor_row, lowered.record)
                .map_err(|_| admission())?;
            if let Some((text, external)) = foreign {
                self.foreign_rows.push((text, external, row));
            }
            return Ok(Some(row));
        }
        let Some(name) = self.carrier_name(anchor) else {
            return self.host(
                Lowered::leaf(unknown_record(TypeReason::OracleGap, None)),
                None,
            );
        };
        // A compound spelling lowers to the same carrier every time it is
        // written, so a repeat occurrence must reuse the first carrier
        // ordinal. Emitting one row per occurrence minted byte-identical
        // twins that the image build honestly rejected as a duplicate.
        for (known, ordinal) in &self.carrier_rows {
            if *known == name {
                return Ok(Some(*ordinal));
            }
        }
        if self.carrier_rows.len() >= MAX_DEDUPED_CARRIER_ROWS {
            return Err(admission());
        }
        let extension = self.empty_extension(RustOwnership::Value)?;
        let mut fact = SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
            .typed(lowered.record)
            .with_extension(EmissionExtension::Rust(extension));
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        let ordinal = coordinate(push(self.facts, fact)?)?;
        self.carrier_rows.push((name, ordinal));
        Ok(Some(ordinal))
    }

    /// Borrows the exact written text of one compound anchor as its carrier
    /// fact's name, falling back to the written leaf spelling; `None` when
    /// the anchor spells nothing this source owns.
    fn carrier_name(&self, anchor: Option<&ast::Type>) -> Option<&'source [u8]> {
        let anchor = anchor?;
        if let Ok(bytes) = self.bytes_of_node(anchor.syntax()) {
            return Some(bytes);
        }
        self.written_type_name(Some(anchor))
    }

    /// Lowers one HIR type position into its lattice record, using the
    /// written syntax only to borrow exact spellings (foreign leaf names,
    /// array lengths, reference lifetimes).
    fn lower_type(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        if depth == 0 {
            return Ok(Lowered::leaf(unknown_record(
                TypeReason::TruncatedAtDepthLimit,
                None,
            )));
        }
        if semantic.is_unknown() {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        }
        if let Some((inner, mutability)) = semantic.as_reference() {
            return self.lower_reference(&inner, mutability, anchor, depth);
        }
        if let Some((inner, mutability)) = semantic.as_raw_ptr() {
            return self.lower_pointer(&inner, mutability, anchor, depth);
        }
        if let Some(builtin) = semantic.as_builtin() {
            return Ok(Lowered::leaf(builtin_record(builtin)));
        }
        if semantic.is_never() {
            return Ok(Lowered::leaf(SemanticTypeRecord::leaf(
                SemanticTypeTag::Never,
            )));
        }
        if semantic.is_unit() {
            return Ok(Lowered::leaf(SemanticTypeRecord::leaf(
                SemanticTypeTag::Tuple,
            )));
        }
        if semantic.is_tuple() {
            return self.lower_tuple(semantic, anchor, depth);
        }
        if let Some(element) = semantic.as_slice() {
            return self.lower_child_pair(&element, SemanticTypeTag::Slice, anchor, depth);
        }
        if let Some((element, _length)) = semantic.as_array(self.database) {
            return self.lower_array(&element, anchor, depth);
        }
        if semantic.is_closure() || semantic.as_callable(self.database).is_some() {
            return self.lower_callable(semantic, anchor, depth);
        }
        if let Some((adt, arguments)) = semantic.as_adt_with_args() {
            return self.lower_adt(adt, &arguments, anchor, depth);
        }
        if let Some(trait_) = semantic.as_dyn_trait() {
            let written = self.written_type_name(anchor);
            return match self.ordinal_of_trait(trait_) {
                Some(ordinal) => {
                    let bound_ty = written_trait_bounds(anchor)
                        .first()
                        .and_then(|bound| bound.ty());
                    let target = match bound_ty {
                        Some(bound_ty) => {
                            self.lower_trait_bound_application(&bound_ty, ordinal, depth - 1)?
                        }
                        None => ordinal,
                    };
                    Ok(Lowered {
                        record: SemanticTypeRecord::leaf(SemanticTypeTag::DynTrait),
                        children: vec![target],
                    })
                }
                None => Ok(Lowered::leaf(self.unresolved_record_with(written))),
            };
        }
        if let Some(bounds) = semantic.as_impl_traits(self.database) {
            return self.lower_impl_trait(bounds.collect(), anchor);
        }
        if let Some(parameter) = semantic.as_type_param(self.database) {
            return self.lower_type_param(parameter, anchor);
        }
        Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)))
    }

    /// Lowers one shared or exclusive borrow over its referent row.
    fn lower_reference(
        &mut self,
        inner: &ra_ap_hir::Type<'_>,
        mutability: ra_ap_hir::Mutability,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(inner, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        let mut record = primitive_record(PrimitiveShape::Reference, 0);
        if matches!(mutability, ra_ap_hir::Mutability::Mut) {
            record.payload1 = REFERENCE_MUTABLE_FLAG;
        }
        record.text = self.written_lifetime(anchor);
        Ok(Lowered {
            record,
            children: vec![target],
        })
    }

    /// Lowers one raw pointer over its pointee row.
    fn lower_pointer(
        &mut self,
        inner: &ra_ap_hir::Type<'_>,
        mutability: ra_ap_hir::Mutability,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(inner, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        let shape = match mutability {
            ra_ap_hir::Mutability::Mut => PrimitiveShape::MutPointer,
            ra_ap_hir::Mutability::Shared => PrimitiveShape::ConstPointer,
        };
        Ok(Lowered {
            record: primitive_record(shape, 0),
            children: vec![target],
        })
    }

    /// Lowers one one-child compound (a slice) over its element row.
    fn lower_child_pair(
        &mut self,
        inner: &ra_ap_hir::Type<'_>,
        tag: SemanticTypeTag,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(inner, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(tag),
            children: vec![target],
        })
    }

    /// Lowers one array: the element row plus the exact written length text
    /// the Array tag requires. Without a written length the position folds to
    /// the honest gap reason instead of a fabricated size.
    fn lower_array(
        &mut self,
        element: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(element, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        let Some(text) = (match anchor {
            Some(ast::Type::ArrayType(array)) => array
                .const_arg()
                .and_then(|argument| argument.expr())
                .and_then(|expr| self.bytes_of_node(expr.syntax()).ok()),
            _ => None,
        }) else {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::ArrayConstExpression);
        record.text = Some(text);
        Ok(Lowered {
            record,
            children: vec![target],
        })
    }

    /// Lowers one tuple: one child per element, unlabeled, because HIR tuple
    /// positions carry no element names.
    fn lower_tuple(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let fields = semantic.tuple_fields(self.database);
        if fields.len() > MAX_COMPOUND_CHILDREN {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        }
        let mut children = Vec::new();
        for (position, element) in fields.iter().enumerate() {
            match self.lower_target(element, child_anchor(anchor, position), depth - 1)? {
                Some(target) => children.push(target),
                None => return Ok(self.folded_rowless(anchor)),
            }
        }
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(SemanticTypeTag::Tuple),
            children,
        })
    }

    /// Lowers one callable position (function value, closure, or fn trait
    /// impl) as a structural `FunctionPointer` over its signature.
    fn lower_callable(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let Some(callable) = semantic.as_callable(self.database) else {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        let parameters = callable.params();
        let returns = callable.return_type().clone();
        if parameters.len() + usize::from(!returns.is_unit()) > MAX_COMPOUND_CHILDREN {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        }
        let mut children = Vec::new();
        for (position, parameter) in parameters.iter().enumerate() {
            let lowered_parameter = parameter.ty().clone();
            let written = callable_anchor(anchor, CallablePart::Parameter(position));
            match self.lower_target(&lowered_parameter, written, depth - 1)? {
                Some(target) => children.push(target),
                None => return Ok(self.folded_rowless(anchor)),
            }
        }
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        if !returns.is_unit() {
            let written = callable_anchor(anchor, CallablePart::Return);
            match self.lower_target(&returns, written, depth - 1)? {
                Some(target) => children.push(target),
                None => return Ok(self.folded_rowless(anchor)),
            }
            record.payload1 = SemanticTypeRecord::FUNCTION_RESULT_COUNT_ONE;
        }
        Ok(Lowered { record, children })
    }

    /// Lowers one ADT position: a lane-local ADT names its backward nominal
    /// row directly (or an `Apply` over its written arguments), and a foreign
    /// ADT keeps its exact written spelling as the honest unresolved row.
    fn lower_adt(
        &mut self,
        adt: ra_ap_hir::Adt,
        arguments: &[Option<ra_ap_hir::Type<'_>>],
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let written = self.written_type_name(anchor);
        let Some(ordinal) = self.ordinal_of_adt(adt) else {
            let base_record = self.foreign_adt_record(adt, written);
            let argument_children =
                self.lower_written_application_arguments(arguments, anchor, depth)?;
            if argument_children.is_empty() {
                return Ok(Lowered::leaf(base_record));
            }
            if argument_children.len() + 1 > MAX_COMPOUND_CHILDREN {
                return Ok(Lowered::leaf(match written {
                    Some(text) => unknown_record(TypeReason::NoIrRepresentation, Some(text)),
                    None => unknown_record(TypeReason::OracleGap, None),
                }));
            }
            let Some(base) = self.host(Lowered::leaf(base_record), None)? else {
                return Ok(self.folded_rowless(anchor));
            };
            let mut children = vec![base];
            children.extend(argument_children);
            return Ok(Lowered {
                record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                children,
            });
        };
        let argument_children =
            self.lower_written_application_arguments(arguments, anchor, depth)?;
        if argument_children.is_empty() {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
            record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
            return Ok(Lowered::leaf(record));
        }
        if argument_children.len() + 1 > MAX_COMPOUND_CHILDREN {
            return Ok(Lowered::leaf(match written {
                Some(text) => unknown_record(TypeReason::NoIrRepresentation, Some(text)),
                None => unknown_record(TypeReason::OracleGap, None),
            }));
        }
        let mut children = vec![ordinal];
        children.extend(argument_children);
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
            children,
        })
    }

    /// Lowers every written generic argument on one application anchor,
    /// including associated bindings, const arguments, and lifetimes.
    fn lower_written_application_arguments(
        &mut self,
        arguments: &[Option<ra_ap_hir::Type<'_>>],
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Vec<u32>, RustAuthorityError> {
        let written_count = written_generic_argument_count(anchor);
        if written_count == 0 {
            return Ok(Vec::new());
        }
        let hir_type_arguments: Vec<ra_ap_hir::Type<'_>> = arguments
            .iter()
            .filter_map(|argument| argument.as_ref())
            .cloned()
            .collect();
        let mut hir_cursor = 0usize;
        let mut children = Vec::with_capacity(written_count);
        for position in 0..written_count {
            let Some(generic_argument) = generic_argument_at(anchor, position) else {
                return Ok(Vec::new());
            };
            let target = match generic_argument {
                ast::GenericArg::TypeArg(_) => {
                    let Some(hir_argument) = hir_type_arguments.get(hir_cursor) else {
                        return Ok(Vec::new());
                    };
                    hir_cursor += 1;
                    self.lower_target(
                        hir_argument,
                        type_argument_anchor(anchor, position),
                        depth - 1,
                    )?
                }
                other => {
                    let lowered = self.lower_written_generic_argument(other, depth - 1)?;
                    self.host(lowered, anchor)?
                }
            };
            match target {
                Some(target) => children.push(target),
                None => return Ok(Vec::new()),
            }
        }
        Ok(children)
    }

    /// Lowers one non-`TypeArg` generic argument from its written syntax.
    fn lower_written_generic_argument(
        &mut self,
        argument: ast::GenericArg,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        match argument {
            ast::GenericArg::AssocTypeArg(binding) => {
                let name = binding
                    .name_ref()
                    .and_then(|name| self.bytes_of_node(name.syntax()).ok());
                let value = if let Some(ty) = binding.ty() {
                    match self.authority.semantics.resolve_type(&ty) {
                        Some(semantic) => self.lower_type(&semantic, Some(&ty), depth)?,
                        None => Lowered::leaf(unknown_record(TypeReason::OracleGap, None)),
                    }
                } else if let Some(konst) = binding.const_arg().and_then(|argument| argument.expr())
                {
                    self.const_argument_lowered(self.bytes_of_node(konst.syntax()).ok())
                } else {
                    Lowered::leaf(unknown_record(TypeReason::OracleGap, None))
                };
                let Some(target) = self.host(value, binding.ty().as_ref())? else {
                    return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
                };
                Ok(self.assoc_binding_lowered(name, target))
            }
            ast::GenericArg::ConstArg(konst) => {
                let text = konst
                    .expr()
                    .and_then(|expr| self.bytes_of_node(expr.syntax()).ok());
                Ok(self.const_argument_lowered(text))
            }
            ast::GenericArg::LifetimeArg(lifetime) => {
                let text = lifetime
                    .lifetime()
                    .and_then(|lifetime| self.bytes_of_node(lifetime.syntax()).ok());
                Ok(self.lifetime_argument_lowered(text))
            }
            ast::GenericArg::TypeArg(type_argument) => {
                if let Some(ty) = type_argument.ty() {
                    match self.authority.semantics.resolve_type(&ty) {
                        Some(semantic) => self.lower_type(&semantic, Some(&ty), depth),
                        None => Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None))),
                    }
                } else {
                    Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)))
                }
            }
        }
    }

    /// `Item = u8` is not `Item = String`, and neither is a bare `Iterator`.
    fn assoc_binding_lowered(&self, name: Option<&'source [u8]>, value: u32) -> Lowered<'source> {
        let Some(name) = name else {
            return Lowered::leaf(unknown_record(TypeReason::OracleGap, None));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::QualifiedPath);
        record.text = Some(name);
        Lowered {
            record,
            children: vec![value],
        }
    }

    /// `Foo<N>` and `Foo<M>` stay distinct even when the const is not a type.
    fn const_argument_lowered(&self, text: Option<&'source [u8]>) -> Lowered<'source> {
        let Some(text) = text else {
            return Lowered::leaf(unknown_record(TypeReason::OracleGap, None));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
        record.text = Some(text);
        Lowered::leaf(record)
    }

    /// A lifetime generic argument keeps its exact written spelling.
    fn lifetime_argument_lowered(&self, text: Option<&'source [u8]>) -> Lowered<'source> {
        let Some(text) = text else {
            return Lowered::leaf(unknown_record(TypeReason::OracleGap, None));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Inferred);
        record.text = Some(text);
        Lowered::leaf(record)
    }

    /// Lowers one committed trait bound with its written generic arguments.
    fn lower_trait_bound_application(
        &mut self,
        bound: &ast::Type,
        trait_ordinal: u32,
        depth: usize,
    ) -> Result<u32, RustAuthorityError> {
        let argument_children =
            self.lower_written_application_arguments(&[], Some(bound), depth)?;
        if argument_children.is_empty() {
            return Ok(trait_ordinal);
        }
        if argument_children.len() + 1 > MAX_COMPOUND_CHILDREN {
            return self
                .host(
                    Lowered::leaf(unknown_record(
                        TypeReason::NoIrRepresentation,
                        self.bytes_of_node(bound.syntax()).ok(),
                    )),
                    Some(bound),
                )?
                .ok_or_else(unsupported_generic);
        }
        let mut children = vec![trait_ordinal];
        children.extend(argument_children);
        self.host(
            Lowered {
                record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                children,
            },
            Some(bound),
        )?
        .ok_or_else(unsupported_generic)
    }

    /// Lowers one `impl Trait` position over its local bound rows; any bound
    /// resolving outside the fragment folds to the written unresolved row.
    /// `impl Trait` spells no path, so the shared leaf-name reader returns
    /// `None` for it; the whole written bound run is borrowed instead,
    /// mirroring the declaration-site foreign bound's exact written text.
    fn lower_impl_trait(
        &mut self,
        bounds: Vec<ra_ap_hir::Trait>,
        anchor: Option<&ast::Type>,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let written = anchor.and_then(|anchor| self.bytes_of_node(anchor.syntax()).ok());
        if bounds.len() > MAX_COMPOUND_CHILDREN {
            return Ok(Lowered::leaf(match written {
                Some(text) => unknown_record(TypeReason::NoIrRepresentation, Some(text)),
                None => unknown_record(TypeReason::OracleGap, None),
            }));
        }
        let written_bounds = written_trait_bounds(anchor);
        let mut children = Vec::new();
        for (index, trait_) in bounds.into_iter().enumerate() {
            match self.ordinal_of_trait(trait_) {
                Some(ordinal) => {
                    let bound_ty = written_bounds.get(index).and_then(|bound| bound.ty());
                    let target = match bound_ty {
                        Some(bound_ty) => self.lower_trait_bound_application(
                            &bound_ty,
                            ordinal,
                            MAX_TYPE_DEPTH - 1,
                        )?,
                        None => ordinal,
                    };
                    children.push(target);
                }
                None => {
                    return Ok(Lowered::leaf(self.unresolved_record_with(written)));
                }
            }
        }
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(SemanticTypeTag::ImplTrait),
            children,
        })
    }

    /// Lowers one generic-parameter use: the implicit trait `Self` stays a
    /// `SelfType` leaf, every written parameter keeps its exact spelling as
    /// a `TypeVar` row, and an unwritten use folds to the gap reason.
    fn lower_type_param(
        &mut self,
        parameter: ra_ap_hir::TypeParam,
        anchor: Option<&ast::Type>,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        if parameter.is_implicit(self.database) {
            return Ok(Lowered::leaf(SemanticTypeRecord::leaf(
                SemanticTypeTag::SelfType,
            )));
        }
        let Some(text) = self.written_type_name(anchor) else {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
        record.text = Some(text);
        Ok(Lowered::leaf(record))
    }

    /// Lowers one position and interns it into a pooled row coordinate (or
    /// returns the nominal fact ordinal it names); `None` folds the parent.
    fn lower_target(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<ast::Type>,
        depth: usize,
    ) -> Result<Option<u32>, RustAuthorityError> {
        let lowered = self.lower_type(semantic, anchor.as_ref(), depth)?;
        self.host(lowered, anchor.as_ref())
    }

    /// The rowless fold for a compound whose pooled rows cannot be lawfully
    /// interned yet: the exact written spelling, or the gap reason.
    fn folded_rowless(&self, anchor: Option<&ast::Type>) -> Lowered<'source> {
        Lowered::leaf(self.unresolved_record(anchor))
    }

    /// Same as [`Emitter::unresolved_record`] for an already-borrowed
    /// spelling.
    fn unresolved_record_with(
        &self,
        written: Option<&'source [u8]>,
    ) -> SemanticTypeRecord<'source> {
        match written {
            Some(text) => unknown_record(TypeReason::UnresolvedExternal, Some(text)),
            None => unknown_record(TypeReason::OracleGap, None),
        }
    }

    /// The record for one foreign ADT that rust-analyzer resolved: an
    /// external nominal whose authority is the ADT's defining crate module
    /// (`core::option`, `alloc::boxed`), displayed by the exact written
    /// spelling. The authority is derived from the oracle's resolution, never
    /// from the spelling, so std and prelude types are never unresolved.
    /// Without a written spelling the display cell cannot be borrowed and the
    /// position keeps the gap reason.
    fn foreign_adt_record(
        &self,
        adt: ra_ap_hir::Adt,
        written: Option<&'source [u8]>,
    ) -> SemanticTypeRecord<'source> {
        let (Some(text), Some(fragment)) = (written, self.foreign_module_fragment(adt)) else {
            return self.unresolved_record_with(written);
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
        record.nominal = Some(NominalRef::External(ExternalEntityRef::bind(fragment, 0)));
        record.text = Some(text);
        record
    }

    /// The external fragment authority of one foreign ADT's defining module:
    /// the canonical crate name followed by the module path from the crate
    /// root, the same module-granular authority the TypeScript lane binds.
    fn foreign_module_fragment(&self, adt: ra_ap_hir::Adt) -> Option<ExternalFragmentId> {
        let module = adt.module(self.database);
        let krate = module.krate(self.database).display_name(self.database)?;
        let mut canonical = String::from(CARGO_ECOSYSTEM);
        canonical.push(':');
        canonical.push_str(krate.canonical_name().as_str());
        for segment in module.path_to_root(self.database).into_iter().rev() {
            if let Some(name) = segment.name(self.database) {
                canonical.push_str("::");
                canonical.push_str(name.as_str());
            }
        }
        Some(ExternalFragmentId::from_canonical_bytes(
            canonical.as_bytes(),
        ))
    }

    /// Borrows the written lifetime behind one reference anchor.
    fn written_lifetime(&self, anchor: Option<&ast::Type>) -> Option<&'source [u8]> {
        match anchor? {
            ast::Type::RefType(reference) => {
                let lifetime = reference.lifetime()?;
                let span = self.authority.span(lifetime.syntax()).ok()?;
                self.bytes_of(span).ok()
            }
            _ => None,
        }
    }

    /// Attaches each macro invocation spelling to the innermost pushed
    /// declaration containing it, replacing that row's extension facts.
    fn attach_macros(&mut self) -> Result<(), RustAuthorityError> {
        if self.macro_sites.is_empty() {
            return Ok(());
        }
        let mut owners: HashMap<u32, usize> = HashMap::new();
        let mut owned_atoms: Vec<Vec<u32>> = Vec::new();
        let mut spellings: HashMap<&'source [u8], u32> = HashMap::new();
        for site in &self.macro_sites {
            let Some(owner) = self.owner_of(site.span) else {
                continue;
            };
            let interned = match spellings.get(site.spelling) {
                Some(atom) => *atom,
                None => {
                    let atom = self
                        .facts
                        .intern_atom(site.spelling)
                        .map_err(|_| admission())?;
                    spellings.insert(site.spelling, atom);
                    atom
                }
            };
            let position = match owners.get(&owner) {
                Some(position) => *position,
                None => {
                    let position = owned_atoms.len();
                    owners.insert(owner, position);
                    owned_atoms.push(Vec::new());
                    position
                }
            };
            if let Some(atoms) = owned_atoms.get_mut(position) {
                atoms.push(interned);
            }
        }
        for (owner, position) in owners {
            let Some(atoms) = owned_atoms.get(position) else {
                continue;
            };
            let list = self
                .facts
                .intern_atom_list(atoms)
                .map_err(|_| admission())?;
            let Some(row) = self.rows.iter_mut().find(|row| row.ordinal == owner) else {
                continue;
            };
            row.extension.macros = list;
            let updated = row.extension;
            let ordinal = usize::try_from(owner).map_err(|_| admission())?;
            self.facts
                .attach_extension_with_type_parameters(
                    ordinal,
                    EmissionExtension::Rust(updated),
                    self.facts
                        .captured_type_parameter_range(ordinal)
                        .map_err(|_| admission())?,
                )
                .map_err(|_| admission())?;
            self.facts
                .set_free_predicate_range(
                    ordinal,
                    &EmissionExtension::Rust(updated),
                    self.facts
                        .captured_free_predicate_range(ordinal)
                        .map_err(|_| admission())?,
                )
                .map_err(|_| admission())?;
        }
        Ok(())
    }

    /// Emits every oracle-resolved occurrence: method calls, top-level path
    /// references, and macro invocations, each owned by the innermost
    /// containing declaration and measured relative to that owner.
    fn emit_occurrences(&mut self) -> Result<(), RustAuthorityError> {
        let authority = self.authority;
        let method_calls: Vec<_> = authority.method_calls().collect();
        let mut emitted_method_spans = Vec::new();
        for call in &method_calls {
            let Some(name) = call.syntax.name_ref() else {
                continue;
            };
            let span = match call.projected_span {
                Some(span) => span,
                None => authority.span(name.syntax())?,
            };
            if emitted_method_spans.contains(&span) {
                continue;
            }
            emitted_method_spans.push(span);
            let definition = call.target.map(ra_ap_hir::ModuleDef::from);
            // A dispatch the oracle resolved is oracle tier; a method call
            // rust-analyzer could not resolve stays syntactic confidence
            // with its written spelling as a foreign key.
            let confidence = occurrence_confidence(call.target.is_some());
            self.emit_one_occurrence(
                span,
                ReferenceKind::MethodCall,
                ResolvedTarget::Definition(definition),
                confidence,
                None,
            )?;
        }
        let accesses: Vec<RustFieldAccess> = authority.field_accesses().collect();
        let mut emitted_field_spans = Vec::new();
        for access in &accesses {
            let Some(name) = access.syntax.name_ref() else {
                continue;
            };
            let span = authority.span(name.syntax())?;
            if emitted_field_spans.contains(&span) {
                continue;
            }
            emitted_field_spans.push(span);
            let confidence = occurrence_confidence(access.target.is_some());
            let target = access
                .target
                .map_or(ResolvedTarget::Definition(None), ResolvedTarget::NamedField);
            self.emit_one_occurrence(span, ReferenceKind::FieldAccess, target, confidence, None)?;
        }
        for (span, target) in self.macro_field_accesses()? {
            if emitted_field_spans.contains(&span) {
                continue;
            }
            emitted_field_spans.push(span);
            let confidence = occurrence_confidence(target.is_some());
            let target =
                target.map_or(ResolvedTarget::Definition(None), ResolvedTarget::NamedField);
            self.emit_one_occurrence(span, ReferenceKind::FieldAccess, target, confidence, None)?;
        }
        let paths: Vec<_> = authority.top_level_paths().collect();
        for path in &paths {
            // A path rust-analyzer could not resolve keeps its exact written
            // spelling as a self-describing foreign key instead of being
            // dropped: the reference site is authority-proven even where the
            // target is not.
            let resolved = authority
                .resolve_path(path)
                .map(|(resolution, _)| resolution);
            // The occurrence site is the written name extent — the final
            // path segment's identifier — never the whole qualified or
            // generic path, so the committed site's source bytes are the
            // identifier a reader searched for. The key text stays the whole
            // written path, which is the self-describing foreign spelling.
            let full = authority.span(path.syntax())?;
            let span = path
                .segments()
                .last()
                .and_then(|segment| segment.name_ref())
                .and_then(|name| authority.span(name.syntax()).ok())
                .unwrap_or(full);
            let written = self.bytes_of(full)?;
            let kind = match &resolved {
                Some(resolution) => reference_kind(path, resolution),
                None => unresolved_reference_kind(path),
            };
            let confidence = match resolved {
                Some(_) => OccurrenceConfidence::Oracle,
                None => OccurrenceConfidence::Syntactic,
            };
            self.emit_one_occurrence(
                span,
                kind,
                ResolvedTarget::Definition(resolved.as_ref().and_then(path_definition)),
                confidence,
                Some(written),
            )?;
        }
        let sites: Vec<MacroSite<'source>> = self.macro_sites.to_vec();
        for site in &sites {
            let confidence = occurrence_confidence(site.resolved);
            self.emit_one_occurrence(
                site.span,
                ReferenceKind::MacroInvocation,
                ResolvedTarget::Definition(None),
                confidence,
                Some(site.spelling),
            )?;
        }
        Ok(())
    }

    /// Streams field accesses discovered through macro expansion. A macro
    /// argument is a token tree in the source file; descending each token
    /// reaches the expanded `FieldExpr` rust-analyzer inferred and projects
    /// the written field identifier back onto this source buffer.
    fn macro_field_accesses(
        &self,
    ) -> Result<Vec<(ByteSpan, Option<ra_ap_hir::Field>)>, RustAuthorityError> {
        let authority = self.authority;
        let mut accesses = Vec::new();
        for macro_call in authority.macro_calls() {
            let Some(token_tree) = macro_call.token_tree() else {
                continue;
            };
            for token in token_tree
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
            {
                for descended in authority
                    .semantics
                    .descend_into_macros_no_opaque(token, false)
                {
                    let Some(syntax) = descended
                        .value
                        .parent()
                        .and_then(|node| node.ancestors().find_map(ast::FieldExpr::cast))
                    else {
                        continue;
                    };
                    let Some(name) = syntax.name_ref() else {
                        continue;
                    };
                    let Ok(Some(projected_span)) = authority.projected_span(name.syntax()) else {
                        continue;
                    };
                    let target = authority.resolve_field_target(&syntax);
                    accesses.push((projected_span, target));
                }
            }
        }
        Ok(accesses)
    }

    /// Emits one occurrence fact, resolving the target through the pushed
    /// definition and field tables and folding to a self-describing foreign
    /// key when the target lives outside this fragment. The reference site
    /// (`span`) is the written name extent; `key` carries the canonical
    /// written spelling the foreign key names when it differs from the site
    /// (a whole qualified path, a macro path).
    ///
    /// Owner-less disposition: positions with no owning declaration
    /// (module-level `use` items) stay unowned and unemitted. The shared
    /// wire format carries no owner-less occurrence — `push_occurrence`
    /// requires an owning fact ordinal — and the shared containment law
    /// rejects an occurrence span that escapes its owner's provenance span
    /// at build time, so the only possible emission would fabricate a
    /// containing relation that no declaration proves: the exact boundary
    /// the C lane records as "containment is not ownership". The skip is
    /// the honest unowned disposition, never a silent truncation of the
    /// written spelling, which the foreign key still carries whenever an
    /// owned sibling site or a doc link names it.
    fn emit_one_occurrence(
        &mut self,
        span: ByteSpan,
        kind: ReferenceKind,
        resolved: ResolvedTarget,
        confidence: OccurrenceConfidence,
        key: Option<&'source [u8]>,
    ) -> Result<(), RustAuthorityError> {
        let Some(owner) = self.owner_of(span) else {
            return Ok(());
        };
        let owner_span = self
            .rows
            .iter()
            .find(|row| row.ordinal == owner)
            .map(|row| row.span);
        let Some(owner_span) = owner_span else {
            return Ok(());
        };
        // A macro invocation's written reference site is its path spelling,
        // not the whole invocation call: the call's token text can carry a
        // line-continuation backslash inside a string literal, which is not a
        // canonical foreign path. Every other occurrence kind borrows the
        // exact written span.
        let written = match key {
            Some(key) => key,
            None => self.bytes_of(span)?,
        };
        let ordinal = match resolved {
            ResolvedTarget::Definition(Some(definition)) => self.ordinal_of_definition(&definition),
            ResolvedTarget::NamedField(field) => self.ordinal_of_field(&field),
            ResolvedTarget::Definition(None) => None,
        };
        let target = match ordinal {
            Some(ordinal) => OccurrenceTarget::Local(EntityId::new(ordinal)),
            None => foreign_universe(written)?,
        };
        let relative = relative_span(span, owner_span)?;
        self.facts
            .push_occurrence(
                owner,
                Occurrence {
                    target,
                    kind,
                    confidence,
                    span: relative,
                },
            )
            .map_err(|_| admission())
    }

    /// Streams every declaration's Rustdoc into borrowed fragments: prose
    /// lines, fenced code, and intra-doc links whose target names a pushed
    /// declaration link locally.
    fn emit_docs(&mut self, declarations: &[Decl<'source>]) -> Result<(), RustAuthorityError> {
        for index in 0..declarations.len() {
            let Some(Some(owner)) = self.ordinals.get(index).copied() else {
                continue;
            };
            // The RA declaration visitor owns the Rustdoc plane even when it
            // lends no lines for this declaration.
            self.facts
                .mark_documentation_captured(owner)
                .map_err(|_| admission())?;
            let declaration = &declarations[index];
            let mut lines: Vec<&'source [u8]> = Vec::new();
            {
                let authority = self.authority;
                let emitter = &*self;
                authority.visit_documentation(&declaration.syntax, |line| {
                    if let Some(span) = emitter.span_of_text(line)
                        && let Ok(bytes) = emitter.bytes_of(span)
                    {
                        lines.push(bytes);
                    }
                });
            }
            self.push_doc_lines(owner, &lines)?;
        }
        Ok(())
    }

    /// Pushes the borrowed doc lines of one declaration as fragments.
    fn push_doc_lines(
        &mut self,
        owner: u32,
        lines: &[&'source [u8]],
    ) -> Result<(), RustAuthorityError> {
        let mut fragments: Vec<DocFragmentInput<'source>> = Vec::new();
        let mut inside_fence = false;
        for line in lines {
            if line.is_empty() {
                if !matches!(fragments.last(), Some(DocFragmentInput::SoftBreak)) {
                    fragments.push(DocFragmentInput::SoftBreak);
                }
                continue;
            }
            if line.starts_with(b"```") {
                inside_fence = !inside_fence;
                continue;
            }
            if inside_fence {
                fragments.push(DocFragmentInput::Code(line));
                continue;
            }
            split_links(line, &mut fragments);
        }
        let locals: Vec<(&'source [u8], u32)> = self
            .rows
            .iter()
            .map(|row| (row.name, row.ordinal))
            .collect();
        for fragment in fragments
            .iter()
            .copied()
            .map(|fragment| resolve_link(fragment, &locals))
        {
            self.facts
                .push_doc(owner, fragment)
                .map_err(|_| admission())?;
        }
        Ok(())
    }

    /// Emits one leaf `Reexport` entity per resolvable local binding in a
    /// written `use` item; a glob import proves no honest single-name
    /// declaration, so its written law admits no fact at all.
    fn emit_reexports(&mut self) -> Result<(), RustAuthorityError> {
        for reexport in self.authority.reexports() {
            if !reexport.resolved {
                continue;
            }
            let name_span = self.authority.span(&reexport.name)?;
            let name = self.bytes_of(name_span)?;
            // Rust's single-name law (E0428) applies to a reexported binding
            // exactly as it does to any other declaration: an authority that
            // does not evaluate every cfg gate (or a facade re-exporting the
            // same name from two branches) can stream cfg-disjoint twins.
            // Reuse the declaration walk's dedup key so a later twin is
            // dropped instead of minting a byte-identical `Reexport`
            // identity the image build must reject.
            let scope = self.enclosing_item_span(reexport.item.syntax())?;
            if !self
                .scoped_names
                .insert((scope, EntityKind::Reexport as u8, name.to_vec()))
            {
                continue;
            }
            let extension = self.empty_extension(RustOwnership::Value)?;
            let fact = SemanticFact::new(EntityKind::Reexport, name, LEAF_PRODUCT)
                .with_visibility(self.visibility_of(&reexport.item.syntax().clone()))
                .typed(unknown_record(TypeReason::NoIrRepresentation, Some(name)))
                .with_extension(EmissionExtension::Rust(extension));
            let ordinal = coordinate(push(self.facts, fact)?)?;
            self.rows.push(Row {
                ordinal,
                name,
                span: self.authority.span(reexport.item.syntax())?,
                extension,
            });
        }
        Ok(())
    }

    /// Captures the written visibility prefix of one syntax item.
    fn visibility_of(&self, syntax: &ra_ap_syntax::SyntaxNode) -> backend_semantic::ir::Visibility {
        let Some(visibility) =
            ast::AnyHasVisibility::cast(syntax.clone()).and_then(|item| item.visibility())
        else {
            return backend_semantic::ir::Visibility::Private;
        };
        let range = visibility.syntax().text_range();
        let Some(start) = usize::try_from(u32::from(range.start())).ok() else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        let Some(end) = usize::try_from(u32::from(range.end())).ok() else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        let Some(bytes) = self.source.get(start..end) else {
            return backend_semantic::ir::Visibility::Unknown;
        };
        if bytes == b"pub(crate)" {
            backend_semantic::ir::Visibility::Package
        } else if bytes.starts_with(b"pub(") {
            backend_semantic::ir::Visibility::Restricted
        } else {
            backend_semantic::ir::Visibility::Public
        }
    }

    /// Emits proven let initializer and method-call result types in source order.
    fn emit_computed(&mut self) -> Result<(), RustAuthorityError> {
        let mut expressions = Vec::new();
        for expression in self.authority.inferred_let_initializers() {
            let span = self.authority.span(expression.expression.syntax())?;
            if let Some(inferred) = expression
                .inferred
                .filter(|inferred| !inferred.original.is_unknown())
            {
                expressions.push((span, inferred.original, None));
            }
        }
        for call in self.authority.method_calls() {
            let span = match call.projected_span {
                Some(span) => span,
                None => self.authority.span(call.syntax.syntax())?,
            };
            if let Some(inferred) = call
                .inferred
                .filter(|inferred| !inferred.original.is_unknown())
            {
                expressions.push((span, inferred.original, None));
            }
        }
        expressions.sort_by_key(|(span, _, _)| span.start);
        for (span, semantic, anchor) in expressions {
            let Some(owner) = self.owner_of(span) else {
                continue;
            };
            self.computed_owner = Some(owner);
            let lowered = self.lower_pending_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
            self.host_computed(lowered)?;
            self.computed_owner = None;
        }
        Ok(())
    }

    /// Commits one lowered inferred type into the schema-2 computed segment.
    fn host_computed(&mut self, lowered: Lowered<'source>) -> Result<u32, RustAuthorityError> {
        let owner = self.computed_owner.ok_or_else(admission)?;
        for target in lowered.children {
            self.facts
                .computed_type_child(target, None, 0)
                .map_err(|_| admission())?;
        }
        self.facts
            .intern_computed_type_row(owner, lowered.record)
            .map_err(|_| admission())
    }

    /// Pushes documentation belonging to each emitted re-export declaration.
    fn emit_reexport_docs(&mut self) -> Result<(), RustAuthorityError> {
        for reexport in self.authority.reexports() {
            if !reexport.resolved {
                continue;
            }
            let name = self.bytes_of(self.authority.span(&reexport.name)?)?;
            let item_span = self.authority.span(reexport.item.syntax())?;
            let Some(owner) = self
                .rows
                .iter()
                .find(|row| row.name == name && row.span == item_span)
                .map(|row| row.ordinal)
            else {
                continue;
            };
            let mut lines = Vec::new();
            {
                let authority = self.authority;
                let emitter = &*self;
                authority.visit_documentation(reexport.item.syntax(), |line| {
                    if let Some(span) = emitter.span_of_text(line)
                        && let Ok(bytes) = emitter.bytes_of(span)
                    {
                        lines.push(bytes);
                    }
                });
            }
            self.push_doc_lines(owner, &lines)?;
        }
        Ok(())
    }
}
