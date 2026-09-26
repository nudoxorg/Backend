//! Queries one borrowed rust-analyzer authority for declarations, spans, and occurrences.
use super::*;

impl<'analysis> RustAuthority<'analysis> {
    /// Streams HIR-backed declarations in source traversal order without materializing a fact list.
    pub fn declarations(&self) -> impl Iterator<Item = RustDeclaration> + '_ {
        self.root
            .syntax()
            .descendants()
            .filter_map(|syntax| self.declaration(syntax))
    }

    /// Returns the exact identifier span selected by rust-analyzer syntax for one declaration.
    ///
    /// # Errors
    ///
    /// Returns a missing-semantic-fact terminal when the HIR-backed declaration
    /// has no source identifier (for example, an anonymous implementation), or
    /// a coordinate failure without falling back to text scanning.
    pub fn declaration_name(
        &self,
        declaration: &RustDeclaration,
    ) -> Result<ByteSpan, RustAuthorityError> {
        let name = declaration
            .syntax
            .descendants()
            .find_map(ast::Name::cast)
            .map(|name| name.syntax().clone())
            .or_else(|| {
                declaration
                    .syntax
                    .descendants()
                    .find_map(ast::NameRef::cast)
                    .map(|name| name.syntax().clone())
            })
            .ok_or(RustAuthorityError::MissingSemanticFact {
                fact: declaration.kind,
            })?;
        self.span(&name)
    }

    /// Streams method calls with the actual inferred receiver/call type and resolved function.
    pub fn method_calls(&self) -> impl Iterator<Item = RustMethodCall<'analysis>> + '_ {
        let mut calls = Vec::new();
        // Several expanded tokens can project to one written call. Keep the
        // first-seen order while making duplicate detection independent of
        // the number of calls already collected.
        let mut projected_calls = HashSet::new();
        for syntax in self.root.syntax().descendants() {
            let Some(syntax) = ast::MethodCallExpr::cast(syntax) else {
                continue;
            };
            calls.push(RustMethodCall {
                inferred: self.semantics.type_of_expr(&syntax.clone().into()),
                target: self.semantics.resolve_method_call(&syntax),
                syntax,
                projected_span: None,
            });
        }

        // A macro argument is parsed as a token tree in the source file. Descending
        // each token gives us the expanded syntax node that rust-analyzer inferred;
        // its original-range map still points at the written argument.
        for macro_call in self.macro_calls() {
            let Some(token_tree) = macro_call.token_tree() else {
                continue;
            };
            for token in token_tree
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
            {
                for descended in self.semantics.descend_into_macros_no_opaque(token, false) {
                    let Some(syntax) = descended
                        .value
                        .parent()
                        .and_then(|node| node.ancestors().find_map(ast::MethodCallExpr::cast))
                    else {
                        continue;
                    };
                    let Some(name) = syntax.name_ref() else {
                        continue;
                    };
                    let Ok(Some(projected_span)) = self.projected_span(name.syntax()) else {
                        continue;
                    };
                    if !projected_calls.insert(projected_span) {
                        continue;
                    }
                    calls.push(RustMethodCall {
                        inferred: self.semantics.type_of_expr(&syntax.clone().into()),
                        target: self.semantics.resolve_method_call(&syntax),
                        syntax,
                        projected_span: Some(projected_span),
                    });
                }
            }
        }
        calls.into_iter()
    }

    /// Streams path syntax so callers can retain rust-analyzer resolution and substitutions.
    pub fn paths(&self) -> impl Iterator<Item = ast::Path> + '_ {
        self.root.syntax().descendants().filter_map(ast::Path::cast)
    }

    /// Streams only the top path of every path chain, skipping qualifier
    /// children (`a::b` yields one `a::b`, never an extra `a`), so consumers
    /// emit exactly one reference fact per written path chain.
    pub fn top_level_paths(&self) -> impl Iterator<Item = ast::Path> + '_ {
        self.root.syntax().descendants().filter_map(|syntax| {
            let path = ast::Path::cast(syntax)?;
            let nested = path
                .syntax()
                .parent()
                .is_some_and(|parent| parent.kind() == ra_ap_syntax::SyntaxKind::PATH);
            (!nested).then_some(path)
        })
    }

    /// Streams macro invocations with their unexpanded call-site syntax intact.
    pub fn macro_calls(&self) -> impl Iterator<Item = ast::MacroCall> + '_ {
        self.root
            .syntax()
            .descendants()
            .filter_map(ast::MacroCall::cast)
    }

    /// Resolves a path and keeps rust-analyzer's generic substitution unrendered.
    #[must_use]
    pub fn resolve_path(
        &self,
        path: &ast::Path,
    ) -> Option<(
        PathResolution,
        Option<ra_ap_hir::GenericSubstitution<'analysis>>,
    )> {
        self.semantics.resolve_path_with_subst(path)
    }

    /// Resolves a macro invocation to its definition without pretending expanded tokens are source.
    #[must_use]
    pub fn resolve_macro(&self, call: &ast::MacroCall) -> Option<Macro> {
        self.semantics.resolve_macro_call(call)
    }

    /// Returns the inferred type of one expression without rendering it to lossy text.
    #[must_use]
    pub fn inferred_type(&self, expression: &ast::Expr) -> Option<TypeInfo<'analysis>> {
        self.semantics.type_of_expr(expression)
    }

    /// Streams written let initializers with their analyzer-proven result types.
    pub fn inferred_let_initializers(
        &self,
    ) -> impl Iterator<Item = RustInferredExpression<'analysis>> + '_ {
        self.root.syntax().descendants().filter_map(|syntax| {
            let statement = ast::LetStmt::cast(syntax)?;
            let expression = statement.initializer()?;
            Some(RustInferredExpression {
                expression: expression.clone(),
                inferred: self.inferred_type(&expression),
            })
        })
    }

    /// Returns resolvable written bindings from source-level `use` items.
    pub fn reexports(&self) -> Vec<RustReexport> {
        let mut result = Vec::new();
        for syntax in self.root.syntax().descendants() {
            let Some(item) = ast::Use::cast(syntax) else {
                continue;
            };
            // A private `use` is a local alias, not a re-export: it never
            // enters the crate's public surface, so admitting it here can
            // mint a `Reexport` entity that shadows the identity of the same
            // name's genuine public binding in a different scope (observed
            // on `generic-array@1.4.5`, whose crate root privately
            // `use`-imports two names a nested `pub mod` also re-exports
            // under `#[cfg(feature = "internals")]`).
            if item.visibility().is_none() {
                continue;
            }
            let Some(tree) = item.use_tree() else {
                continue;
            };
            collect_reexports(self, &item, &tree, &mut result);
        }
        result
    }

    /// Converts one syntax node's local range into an exact validated original-byte span.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError`] if rust-analyzer produced coordinates outside `source`.
    pub fn span(&self, syntax: &ra_ap_syntax::SyntaxNode) -> Result<ByteSpan, RustAuthorityError> {
        checked_span(syntax, self.source)
    }

    /// Borrows exact original bytes for a previously validated span.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError`] when `span` lies outside this authority's source buffer.
    pub fn source_at(&self, span: ByteSpan) -> Result<&'analysis [u8], RustAuthorityError> {
        bytes_at(self.source, span)
    }

    /// Visits raw Rustdoc fragments without allocating or normalizing author text.
    pub fn visit_documentation(
        &self,
        syntax: &ra_ap_syntax::SyntaxNode,
        mut receive: impl FnMut(&str),
    ) {
        for comment in ast::DocCommentIter::from_syntax_node(syntax) {
            if let Some((text, _)) = comment.doc_comment() {
                receive(text.trim_start());
            }
        }
    }

    /// Maps a resolved HIR definition to an original local coordinate or explicit foreign state.
    #[must_use]
    pub fn definition_origin<Definition: HasSource>(
        &self,
        definition: Definition,
        kind: SemanticKind,
    ) -> SourceOrigin {
        let Some(source) = self.semantics.source(definition) else {
            return SourceOrigin::Foreign(kind);
        };
        let range = self.semantics.original_range(source.value.syntax());
        if range.file_id != self.source_file {
            return SourceOrigin::Foreign(kind);
        }
        ByteSpan::from_text_range(range.range)
            .and_then(|span| bytes_at(self.source, span).ok().map(|_| span))
            .map_or(SourceOrigin::Foreign(kind), SourceOrigin::Local)
    }

    /// Visits syntax diagnostics with exact coordinates and without storing analyzer prose.
    pub fn visit_syntax_diagnostics(&self, mut receive: impl FnMut(ByteSpan)) {
        for error in self.source_file.parse(self.database).errors() {
            if let Some(span) = ByteSpan::from_text_range(error.range()) {
                receive(span);
            }
        }
    }

    /// Projects one syntax node onto this authority's original source bytes,
    /// mapping through macro-expansion provenance to the real-file token
    /// range it grew from. `Ok(None)` means the node projects into another
    /// file, so no byte span of this source can honestly represent it.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError`] when a same-file projection produced a
    /// range this source buffer cannot address.
    pub fn projected_span(
        &self,
        syntax: &ra_ap_syntax::SyntaxNode,
    ) -> Result<Option<ByteSpan>, RustAuthorityError> {
        let range = self.semantics.original_range(syntax);
        if range.file_id != self.source_file {
            return Ok(None);
        }
        let span =
            ByteSpan::from_text_range(range.range).ok_or(RustAuthorityError::InvalidSpan {
                span: ByteSpan { start: 1, end: 0 },
                source_bytes: self.source.len(),
            })?;
        bytes_at(self.source, span).map(|_| span).map(Some)
    }

    /// True when one syntax node lives inside a macro expansion rather than
    /// the parsed original source tree; its raw text ranges address the
    /// expansion buffer, never the caller source.
    #[must_use]
    pub fn is_macro_expansion(&self, syntax: &ra_ap_syntax::SyntaxNode) -> bool {
        self.semantics.hir_file_for(syntax).is_macro()
    }

    /// Streams every written field-access expression with the field
    /// rust-analyzer resolved it to, when it resolved one.
    pub fn field_accesses(&self) -> impl Iterator<Item = RustFieldAccess> + '_ {
        self.root.syntax().descendants().filter_map(|syntax| {
            let syntax = ast::FieldExpr::cast(syntax)?;
            let target = self.resolve_field_target(&syntax);
            Some(RustFieldAccess { syntax, target })
        })
    }

    /// Resolves one field access to its named HIR field. Tuple-index
    /// accesses resolve to no named declaration, so they stay unresolved.
    #[must_use]
    pub fn resolve_field_target(&self, access: &ast::FieldExpr) -> Option<Field> {
        self.semantics
            .resolve_field(access)
            .and_then(|resolved| resolved.left())
    }

    /// Enumerates every HIR-provable declaration of this crate's module tree
    /// — including declarations that exist only through macro expansion and
    /// tuple-struct fields the written syntax tree does not cast — paired
    /// with their projected item span and projected name span. Declarations
    /// whose HIR origin projects outside this source, or whose name has no
    /// provable same-source spelling, carry `Foreign` or absent coordinates
    /// and stay unemitted rather than being guessed.
    pub fn module_declarations(&self) -> Vec<ModuleDeclaration> {
        let Some(root) = self.semantics.hir_file_to_module_def(self.source_file) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut modules = vec![root];
        let mut cursor = 0;
        while cursor < modules.len() {
            let module = modules[cursor];
            cursor += 1;
            for definition in module.declarations(self.database) {
                match definition {
                    ModuleDef::Function(definition) => {
                        self.record_module_declaration(
                            &mut out,
                            RustDefinition::Function(definition),
                        );
                    }
                    ModuleDef::Adt(adt) => match adt {
                        Adt::Struct(definition) => {
                            self.record_module_declaration(
                                &mut out,
                                RustDefinition::Record(Adt::Struct(definition)),
                            );
                            self.record_fields(&mut out, definition.fields(self.database));
                        }
                        Adt::Union(definition) => {
                            self.record_module_declaration(
                                &mut out,
                                RustDefinition::Record(Adt::Union(definition)),
                            );
                            self.record_fields(&mut out, definition.fields(self.database));
                        }
                        Adt::Enum(definition) => {
                            self.record_module_declaration(
                                &mut out,
                                RustDefinition::Enum(Adt::Enum(definition)),
                            );
                            for variant in definition.variants(self.database) {
                                self.record_module_declaration(
                                    &mut out,
                                    RustDefinition::Variant(variant),
                                );
                            }
                        }
                    },
                    ModuleDef::Const(definition) => self
                        .record_module_declaration(&mut out, RustDefinition::Constant(definition)),
                    ModuleDef::Static(definition) => {
                        self.record_module_declaration(
                            &mut out,
                            RustDefinition::Static(definition),
                        );
                    }
                    ModuleDef::Trait(definition) => {
                        self.record_module_declaration(&mut out, RustDefinition::Trait(definition));
                    }
                    ModuleDef::TypeAlias(definition) => self
                        .record_module_declaration(&mut out, RustDefinition::TypeAlias(definition)),
                    // Modules are covered by the written syntax walk or live
                    // in another file; variants are enumerated with their
                    // enum; builtins and macros carry no lane declaration.
                    ModuleDef::Module(_)
                    | ModuleDef::EnumVariant(_)
                    | ModuleDef::BuiltinType(_)
                    | ModuleDef::Macro(_) => {}
                }
            }
            for implementation in module.impl_defs(self.database) {
                self.record_module_declaration(
                    &mut out,
                    RustDefinition::Implementation(implementation),
                );
                for item in implementation.items(self.database) {
                    let definition = match item {
                        AssocItem::Function(definition) => RustDefinition::Function(definition),
                        AssocItem::Const(definition) => RustDefinition::Constant(definition),
                        AssocItem::TypeAlias(definition) => RustDefinition::TypeAlias(definition),
                    };
                    self.record_module_declaration(&mut out, definition);
                }
            }
            modules.extend(module.children(self.database));
        }
        out
    }

    /// Records one walked declaration with its projected item and name
    /// coordinates, keeping declarations whose origin projects outside this
    /// source off the list.
    fn record_module_declaration(
        &self,
        out: &mut Vec<ModuleDeclaration>,
        definition: RustDefinition,
    ) {
        let kind = definition.kind();
        let Some((name, item_node)) = self.definition_source_nodes(&definition) else {
            return;
        };
        let item = match self.projected_span(&item_node) {
            Ok(Some(span)) => SourceOrigin::Local(span),
            Ok(None) | Err(_) => SourceOrigin::Foreign(kind),
        };
        if !item.is_local() {
            return;
        }
        let name = name.and_then(|name| {
            let projected = self.projected_span(&name).ok().flatten()?;
            let projected_bytes = self.source_at(projected).ok()?;
            // `original_range` may conservatively map an expansion token to
            // the complete invocation. Such a range proves an origin but not
            // the declaration's exact name. Admit the coordinate only when
            // its caller-source bytes equal the authority syntax spelling.
            let authority_spelling = name.text().to_string();
            (projected_bytes == authority_spelling.as_bytes()).then_some(projected)
        });
        out.push(ModuleDeclaration {
            definition,
            item,
            name,
            syntax: item_node,
        });
    }

    /// Records one named field declaration; tuple fields carry no name node
    /// and rely on their canonical positional name at the projection site.
    fn record_fields(&self, out: &mut Vec<ModuleDeclaration>, fields: Vec<Field>) {
        for field in fields {
            self.record_module_declaration(out, RustDefinition::Field(field));
        }
    }

    /// Borrows the projected name node and whole-item node of one walked
    /// declaration, when rust-analyzer retains its source.
    fn definition_source_nodes(
        &self,
        definition: &RustDefinition,
    ) -> Option<(Option<ra_ap_syntax::SyntaxNode>, ra_ap_syntax::SyntaxNode)> {
        let (name, item) = match definition {
            RustDefinition::Function(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            RustDefinition::Record(adt) | RustDefinition::Enum(adt) => match adt {
                Adt::Struct(definition) => {
                    let source = self.semantics.source(*definition)?;
                    (
                        source.value.name().map(|name| name.syntax().clone()),
                        source.value.syntax().clone(),
                    )
                }
                Adt::Union(definition) => {
                    let source = self.semantics.source(*definition)?;
                    (
                        source.value.name().map(|name| name.syntax().clone()),
                        source.value.syntax().clone(),
                    )
                }
                Adt::Enum(definition) => {
                    let source = self.semantics.source(*definition)?;
                    (
                        source.value.name().map(|name| name.syntax().clone()),
                        source.value.syntax().clone(),
                    )
                }
            },
            RustDefinition::Variant(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            RustDefinition::Field(definition) => {
                let source = self.semantics.source(*definition)?;
                let name = match &source.value {
                    FieldSource::Named(field) => field.name().map(|name| name.syntax().clone()),
                    FieldSource::Pos(_) => None,
                };
                (name, source.value.syntax().clone())
            }
            RustDefinition::Trait(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            RustDefinition::Implementation(definition) => {
                let source = self.semantics.source(*definition)?;
                let name = implementation_name(&source.value).map(|name| name.syntax().clone());
                (name, source.value.syntax().clone())
            }
            RustDefinition::TypeAlias(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            RustDefinition::Constant(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            RustDefinition::Static(definition) => {
                let source = self.semantics.source(*definition)?;
                (
                    source.value.name().map(|name| name.syntax().clone()),
                    source.value.syntax().clone(),
                )
            }
            // Macro definitions stay out of the declaration lane; walked
            // modules are covered by the written syntax walk or live in
            // another file.
            RustDefinition::Macro(_) | RustDefinition::Module(_) => return None,
        };
        Some((name, item))
    }

    /// Converts one source declaration into a borrowed HIR definition.
    fn declaration(&self, syntax: ra_ap_syntax::SyntaxNode) -> Option<RustDeclaration> {
        let definition = if let Some(item) = ast::RecordField::cast(syntax.clone()) {
            RustDefinition::Field(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Variant::cast(syntax.clone()) {
            RustDefinition::Variant(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Macro::cast(syntax.clone()) {
            RustDefinition::Macro(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Module::cast(syntax.clone()) {
            RustDefinition::Module(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Trait::cast(syntax.clone()) {
            RustDefinition::Trait(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Impl::cast(syntax.clone()) {
            RustDefinition::Implementation(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Fn::cast(syntax.clone()) {
            RustDefinition::Function(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Struct::cast(syntax.clone()) {
            RustDefinition::Record(self.semantics.to_def(&item)?.into())
        } else if let Some(item) = ast::Union::cast(syntax.clone()) {
            RustDefinition::Record(self.semantics.to_def(&item)?.into())
        } else if let Some(item) = ast::Enum::cast(syntax.clone()) {
            RustDefinition::Enum(self.semantics.to_def(&item)?.into())
        } else if let Some(item) = ast::TypeAlias::cast(syntax.clone()) {
            RustDefinition::TypeAlias(self.semantics.to_def(&item)?)
        } else if let Some(item) = ast::Const::cast(syntax.clone()) {
            RustDefinition::Constant(self.semantics.to_def(&item)?)
        } else {
            let item = ast::Static::cast(syntax.clone())?;
            RustDefinition::Static(self.semantics.to_def(&item)?)
        };
        Some(RustDeclaration {
            kind: definition.kind(),
            definition,
            syntax,
        })
    }
}
