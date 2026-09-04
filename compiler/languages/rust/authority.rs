//! Owns the bounded rust-analyzer authority transaction for one Cargo source root.
//! Borrows HIR definitions, types, substitutions, and source maps directly into a caller closure.
//! Never renders, copies, or serializes semantic facts before the shared IR lowerer consumes them.

use std::{
    collections::HashSet,
    fs,
    ops::Deref,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use compiler_vocabulary::RustEdition;
use ra_ap_base_db::{EditionedFileId, all_crates};
use ra_ap_hir::{
    Adt, AssocItem, Const, EnumVariant, Field, FieldSource, Function, HasSource, Impl, Macro,
    Module, ModuleDef, PathResolution, Semantics, Static, Trait, TypeAlias, TypeInfo,
};
use ra_ap_project_model::{CargoConfig, CargoFeatures, RustLibSource};
use ra_ap_syntax::{
    AstNode,
    ast::{self, HasName},
};
use ra_ap_vfs::{AbsPathBuf, VfsPath};

use crate::{LoadError, RustToolchain};

/// Caller-owned Cargo root selected for one semantic authority transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustProject {
    /// Absolute Cargo package root.
    pub root: PathBuf,
    /// Absolute crate root source selected by the caller.
    pub source_path: PathBuf,
    /// Exact native toolchain whose sysroot establishes semantic context.
    pub toolchain: RustToolchain,
    /// Closed Rust edition expected by the compile recipe.
    pub edition: RustEdition,
}

impl RustProject {
    pub(crate) fn validate_root(root: impl AsRef<Path>) -> Result<PathBuf, RustAuthorityError> {
        let root =
            root.as_ref()
                .canonicalize()
                .map_err(|source| RustAuthorityError::ProjectRoot {
                    path: root.as_ref().to_path_buf(),
                    source,
                })?;
        let manifest = root.join("Cargo.toml");
        if !manifest.is_file() {
            return Err(RustAuthorityError::MissingManifest { path: manifest });
        }
        Ok(root)
    }
    /// Validates one caller-selected Cargo root and seals its language profile.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError`] when `root` cannot identify a Cargo package.
    pub fn open(
        root: impl AsRef<Path>,
        toolchain: &RustToolchain,
        edition: RustEdition,
    ) -> Result<Self, RustAuthorityError> {
        let source_path = root.as_ref().join("src/lib.rs");
        Self::open_with_source(root, source_path, toolchain, edition)
    }

    /// Validates a caller-selected Cargo root and exact crate-root source.
    ///
    /// The driver uses this form so the source authority cannot be guessed
    /// from a package layout or filename.
    ///
    /// # Errors
    ///
    /// Returns a typed authority failure when either caller-selected path is
    /// unavailable or the Cargo root lacks a package manifest.
    pub fn open_with_source(
        root: impl AsRef<Path>,
        source_path: impl AsRef<Path>,
        toolchain: &RustToolchain,
        edition: RustEdition,
    ) -> Result<Self, RustAuthorityError> {
        let root =
            root.as_ref()
                .canonicalize()
                .map_err(|source| RustAuthorityError::ProjectRoot {
                    path: root.as_ref().to_path_buf(),
                    source,
                })?;
        let manifest = root.join("Cargo.toml");
        if !manifest.is_file() {
            return Err(RustAuthorityError::MissingManifest { path: manifest });
        }
        let source_path = source_path.as_ref().canonicalize().map_err(|source| {
            RustAuthorityError::ProjectSource {
                path: source_path.as_ref().to_path_buf(),
                source,
            }
        })?;
        if !source_path.is_file() {
            return Err(RustAuthorityError::SourceNotFile { path: source_path });
        }
        Ok(Self {
            root,
            source_path,
            toolchain: toolchain.clone(),
            edition,
        })
    }

    /// Runs one non-escaping semantic transaction over the selected Cargo root.
    ///
    /// The closure is universally quantified over the analyzer lifetime, so no HIR value can
    /// escape the loaded database. A lowerer must emit directly into its caller-owned compact IR
    /// arena while this closure runs.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError`] when Cargo loading, profile validation, source loading, or
    /// source-coordinate validation fails; preserves a closure-returned authority failure exactly.
    pub fn analyze<Output>(
        &self,
        control: RustAnalysisControl<'_>,
        lower: impl for<'analysis> FnOnce(
            RustAuthority<'analysis>,
        ) -> Result<Output, RustAuthorityError>,
    ) -> Result<Output, RustAuthorityError> {
        self.analyze_with_features(control, RustFeatureControl::default(), lower)
    }

    /// Runs analysis with explicit Cargo feature unification controls.
    pub fn analyze_with_features<Output>(
        &self,
        control: RustAnalysisControl<'_>,
        features: RustFeatureControl<'_>,
        lower: impl for<'analysis> FnOnce(
            RustAuthority<'analysis>,
        ) -> Result<Output, RustAuthorityError>,
    ) -> Result<Output, RustAuthorityError> {
        control.check()?;
        let source_path = &self.source_path;
        let source_bytes = fs::metadata(source_path)
            .map_err(|source| RustAuthorityError::SourceRead {
                path: source_path.clone(),
                source,
            })?
            .len();
        if source_bytes > u64::from(*control.maximum_source_bytes) {
            return Err(RustAuthorityError::SourceBudget {
                actual: source_bytes,
                maximum: control.maximum_source_bytes,
            });
        }
        let config = CargoConfig {
            sysroot: Some(RustLibSource::Path(AbsPathBuf::assert_utf8(
                self.toolchain.sysroot.clone(),
            ))),
            no_deps: false,
            metadata_extra_args: vec!["--offline".to_owned()],
            features: features.cargo_features(),
            ..CargoConfig::default()
        };
        let load = ra_ap_load_cargo::LoadCargoConfig {
            load_out_dirs_from_check: false,
            with_proc_macro_server: ra_ap_load_cargo::ProcMacroServerChoice::None,
            prefill_caches: false,
            num_worker_threads: 1,
            proc_macro_processes: 0,
        };
        let (database, vfs, _proc_macros) =
            ra_ap_load_cargo::load_workspace_at(&self.root, &config, &load, &|_| {}).map_err(
                |source| RustAuthorityError::Workspace {
                    root: self.root.clone(),
                    source,
                },
            )?;
        control.check()?;
        let source = fs::read(source_path).map_err(|source| RustAuthorityError::SourceRead {
            path: source_path.clone(),
            source,
        })?;
        let vfs_path = VfsPath::from(AbsPathBuf::assert_utf8(source_path.clone()));
        let file_id = vfs.file_id(&vfs_path).map(|(id, _excluded)| id).ok_or(
            RustAuthorityError::SourceNotLoaded {
                path: source_path.clone(),
            },
        )?;
        // `all_crates` is topologically ordered, so shared roots resolve to the first
        // crate in the loader's deterministic crate-graph order.
        let observed_edition = all_crates(&database)
            .iter()
            .find_map(|krate| {
                let root_file_id = krate.root_file_id(&database);
                (root_file_id.file_id(&database) == file_id)
                    .then_some(root_file_id.edition(&database))
            })
            .ok_or_else(|| RustAuthorityError::SourceNotLoaded {
                path: source_path.clone(),
            })?;
        let source_file = EditionedFileId::new(&database, file_id, observed_edition);
        let observed = rust_edition(observed_edition);
        if observed != self.edition {
            return Err(RustAuthorityError::EditionMismatch {
                requested: self.edition,
                observed,
            });
        }
        control.check()?;
        ra_ap_hir_ty::next_solver::interner::attach_db(&database, || {
            let semantics = Semantics::new(&database);
            let root = semantics.parse(source_file);
            lower(RustAuthority {
                database: &database,
                semantics,
                root,
                source: &source,
                source_file,
                edition: self.edition,
            })
        })
    }
}

/// Bounded original-source byte budget admitted before Cargo workspace loading.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SourceByteLimit(pub u32);

impl Deref for SourceByteLimit {
    type Target = u32;

    /// Borrows the caller-selected maximum without erasing its source-budget meaning.
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<u32> for SourceByteLimit {
    /// Binds one checked-u32 transport limit to Rust source admission.
    fn from(bytes: u32) -> Self {
        Self(bytes)
    }
}

/// Lock-free cancellation and bounded-work facts for one authority transaction.
#[derive(Clone, Copy, Debug)]
pub struct RustAnalysisControl<'cancel> {
    /// Caller-owned cancellation flag, checked at every controllable phase boundary.
    pub cancelled: &'cancel AtomicBool,
    /// Maximum root-source size admitted before Cargo workspace loading.
    pub maximum_source_bytes: SourceByteLimit,
}

/// Caller-selected Cargo feature policy, borrowing the requested spellings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RustFeatureControl<'features> {
    /// Enable every declared feature.
    pub all_features: bool,
    /// Suppress the package's default feature set.
    pub no_default_features: bool,
    /// Exact feature names requested by the caller.
    pub features: &'features [&'features str],
}

impl<'features> RustFeatureControl<'features> {
    /// The unchanged Cargo default.
    #[must_use]
    pub const fn default() -> Self {
        Self {
            all_features: false,
            no_default_features: false,
            features: &[],
        }
    }
}

impl<'features> RustFeatureControl<'features> {
    fn cargo_features(self) -> CargoFeatures {
        if self.all_features {
            CargoFeatures::All
        } else {
            CargoFeatures::Selected {
                features: self
                    .features
                    .iter()
                    .map(|feature| (*feature).to_owned())
                    .collect(),
                no_default_features: self.no_default_features,
            }
        }
    }
}

impl RustAnalysisControl<'_> {
    /// Stops a transaction before it begins another controllable expensive phase.
    ///
    /// # Errors
    ///
    /// Returns [`RustAuthorityError::Cancelled`] when the caller has released the work permit.
    fn check(self) -> Result<(), RustAuthorityError> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(RustAuthorityError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Non-escaping rust-analyzer view whose lifetimes prove all semantic values remain borrowed.
pub struct RustAuthority<'analysis> {
    /// HIR database holding project-model, source-map, resolution, and inference state.
    pub database: &'analysis ra_ap_ide_db::RootDatabase,
    /// Source-to-HIR resolver scoped to `database`.
    pub semantics: Semantics<'analysis, ra_ap_ide_db::RootDatabase>,
    /// Parsed root module bound to `source_file`.
    pub root: ast::SourceFile,
    /// Exact caller-owned original source bytes.
    pub source: &'analysis [u8],
    /// Analyzer file identity of `source` under its Cargo-derived edition.
    pub source_file: EditionedFileId,
    /// Compile-recipe edition cross-checked before this authority was created.
    pub edition: RustEdition,
}

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
        let name = name
            .and_then(|name| self.projected_span(&name).ok())
            .flatten();
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

/// Source-coordinate fact carried as bytes, never characters or UTF-16 columns.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ByteSpan {
    /// First included original source byte.
    pub start: u32,
    /// First excluded original source byte.
    pub end: u32,
}

impl ByteSpan {
    /// Converts rust-analyzer's compact text range without widening or changing coordinate units.
    fn from_text_range(range: ra_ap_syntax::TextRange) -> Option<Self> {
        let start = u32::from(range.start());
        let end = u32::from(range.end());
        (start <= end).then_some(Self { start, end })
    }
}

/// Rust declaration category proved by HIR rather than inferred from token spelling.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticKind {
    /// A module declaration.
    Module,
    /// A function or associated method.
    Function,
    /// A named or tuple field.
    Field,
    /// A struct or union.
    Record,
    /// An enum.
    Enum,
    /// A trait.
    Trait,
    /// An inherent or trait implementation.
    Implementation,
    /// A type alias.
    TypeAlias,
    /// A constant.
    Constant,
    /// A static declaration.
    Static,
    /// A macro definition or invocation.
    Macro,
    /// A named enum variant.
    Variant,
    /// A lexical value binding.
    LocalBinding,
    /// A type or const generic parameter.
    GenericParameter,
    /// A compiler-known item with no source-owned declaration coordinate.
    Builtin,
}

/// Local or foreign status of a resolved HIR definition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceOrigin {
    /// Definition maps to an exact span in the authority source buffer.
    Local(ByteSpan),
    /// Definition belongs to another source file, dependency, or non-source compiler item.
    Foreign(SemanticKind),
}

impl SourceOrigin {
    /// True when the definition resolved into this authority's exact source buffer.
    #[must_use]
    pub const fn is_local(self) -> bool {
        matches!(self, Self::Local(_))
    }
}

/// One source declaration paired with its actual rust-analyzer HIR definition.
pub struct RustDeclaration {
    /// HIR-proven category.
    pub kind: SemanticKind,
    /// Exact typed HIR definition consumed by the IR lowerer.
    pub definition: RustDefinition,
    /// Original source syntax preserving documentation, attributes, and byte coordinates.
    pub syntax: ra_ap_syntax::SyntaxNode,
}

/// Rust-analyzer definitions retained without rendering generic or type information.
#[allow(
    clippy::large_enum_variant,
    reason = "The variant selects a monomorphized HIR handle and never reaches stored IR."
)]
pub enum RustDefinition {
    /// Named or tuple field declaration.
    Field(Field),
    /// Enum-case declaration.
    Variant(EnumVariant),
    /// Declarative or source-defined macro declaration.
    Macro(Macro),
    /// Source module declaration.
    Module(Module),
    /// Trait declaration.
    Trait(Trait),
    /// Inherent or trait implementation.
    Implementation(Impl),
    /// Free or associated function.
    Function(Function),
    /// Struct or union definition.
    Record(Adt),
    /// Enum definition.
    Enum(Adt),
    /// Type alias definition.
    TypeAlias(TypeAlias),
    /// Constant definition.
    Constant(Const),
    /// Static definition.
    Static(Static),
}

impl RustDefinition {
    /// Returns the closed semantic category selected by this HIR definition.
    #[must_use]
    pub const fn kind(&self) -> SemanticKind {
        match self {
            Self::Field(_) => SemanticKind::Field,
            Self::Variant(_) => SemanticKind::Variant,
            Self::Macro(_) => SemanticKind::Macro,
            Self::Module(_) => SemanticKind::Module,
            Self::Trait(_) => SemanticKind::Trait,
            Self::Implementation(_) => SemanticKind::Implementation,
            Self::Function(_) => SemanticKind::Function,
            Self::Record(_) => SemanticKind::Record,
            Self::Enum(_) => SemanticKind::Enum,
            Self::TypeAlias(_) => SemanticKind::TypeAlias,
            Self::Constant(_) => SemanticKind::Constant,
            Self::Static(_) => SemanticKind::Static,
        }
    }

    /// Borrows the HIR type owned by this definition without rendering or allocating it.
    #[must_use]
    pub fn semantic_type<'analysis>(
        &self,
        database: &'analysis ra_ap_ide_db::RootDatabase,
    ) -> Option<ra_ap_hir::Type<'analysis>> {
        match self {
            Self::Field(definition) => Some(definition.ty(database)),
            Self::Variant(definition) => Some(definition.constructor_ty(database)),
            Self::Macro(_) | Self::Module(_) | Self::Trait(_) => None,
            Self::Implementation(definition) => Some(definition.self_ty(database)),
            Self::Function(definition) => Some(definition.fn_ptr_type(database)),
            Self::Record(definition) | Self::Enum(definition) => Some(definition.ty(database)),
            Self::TypeAlias(definition) => Some(definition.ty(database)),
            Self::Constant(definition) => Some(definition.ty(database)),
            Self::Static(definition) => Some(definition.ty(database)),
        }
    }
}

/// One method call with exact AST call-site, inferred result type, and static dispatch target.
pub struct RustMethodCall<'analysis> {
    /// Original call syntax including generic arguments and macro provenance.
    pub syntax: ast::MethodCallExpr,
    /// Original and adjusted inferred result types, retained unrendered.
    pub inferred: Option<TypeInfo<'analysis>>,
    /// Concrete function selected by static method dispatch, when rust-analyzer can resolve one.
    pub target: Option<Function>,
    /// Original-source span for a call discovered through macro expansion.
    pub projected_span: Option<ByteSpan>,
}

/// One written field-access expression with its resolved named HIR field.
pub struct RustFieldAccess {
    /// Original access syntax (`base.field`).
    pub syntax: ast::FieldExpr,
    /// Named field selected by the analyzer, when the base type provably
    /// owns one; tuple-index accesses stay unresolved.
    pub target: Option<Field>,
}

/// One HIR-walked declaration with its projected original-source coordinates.
pub struct ModuleDeclaration {
    /// Exact typed HIR definition consumed by the IR lowerer.
    pub definition: RustDefinition,
    /// Projected whole-item origin: `Local` only when this source owns it.
    pub item: SourceOrigin,
    /// Projected name span, when the name has a provable same-source
    /// spelling. Tuple fields carry none and use their canonical positional
    /// name at the projection site.
    pub name: Option<ByteSpan>,
    /// The declaration's own syntax node, whatever file it was parsed in.
    pub syntax: ra_ap_syntax::SyntaxNode,
}

/// Borrows the written self-type leaf name of one implementation, the lane's
/// implementation naming rule.
fn implementation_name(implementation: &ast::Impl) -> Option<ast::NameRef> {
    let self_ty = implementation.self_ty()?;
    let ast::Type::PathType(path_type) = self_ty else {
        return None;
    };
    let segment = path_type.path()?.segments().last()?;
    segment.name_ref()
}

/// Failure to establish or query one rust-analyzer authority transaction.
#[derive(Debug, thiserror::Error)]
pub enum RustAuthorityError {
    /// The caller cancelled before the authority entered its next controllable phase.
    #[error("Rust semantic authority was cancelled")]
    Cancelled,
    /// Native toolchain discovery rejected the caller-selected compiler.
    #[error("Rust toolchain authority failed: {0}")]
    Toolchain(#[from] LoadError),
    /// The requested project root could not be canonicalized.
    #[error("cannot open Rust project root {path}: {source}")]
    ProjectRoot {
        /// Caller-selected project root.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The caller-selected crate root source could not be canonicalized.
    #[error("cannot open Rust crate root {path}: {source}")]
    ProjectSource {
        /// Caller-selected crate root source path.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The root is not a Cargo package manifest location.
    #[error("Rust project is missing Cargo manifest {path}")]
    MissingManifest {
        /// Expected manifest path.
        path: PathBuf,
    },
    /// The caller-selected crate root is not a regular source file.
    #[error("Rust crate root is not a regular file: {path}")]
    SourceNotFile {
        /// Caller-selected unusable crate root path.
        path: PathBuf,
    },
    /// The selected root source exceeds the caller-owned admission budget.
    #[error("Rust source is {actual} bytes, exceeding the {maximum:?}-byte authority budget")]
    SourceBudget {
        /// Observed root-source length before workspace loading.
        actual: u64,
        /// Caller-selected maximum source bytes.
        maximum: SourceByteLimit,
    },
    /// Cargo selected an edition that conflicts with the caller's sealed profile.
    #[error("Rust Cargo edition {observed:?} conflicts with requested profile {requested:?}")]
    EditionMismatch {
        /// Profile bound into the compile request.
        requested: RustEdition,
        /// Edition derived by rust-analyzer from Cargo metadata.
        observed: RustEdition,
    },
    /// rust-analyzer could not load the Cargo project graph.
    #[error("rust-analyzer could not load {root}: {source}")]
    Workspace {
        /// Caller-selected project root.
        root: PathBuf,
        /// Exact project-model failure.
        #[source]
        source: anyhow::Error,
    },
    /// The declared crate root could not be read after project loading.
    #[error("cannot read Rust crate root {path}: {source}")]
    SourceRead {
        /// Exact requested source path.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The loader did not admit the requested crate root into its VFS.
    #[error("rust-analyzer did not load source path {path}")]
    SourceNotLoaded {
        /// Exact requested source path.
        path: PathBuf,
    },
    /// The compiler request source does not equal the selected Cargo crate root.
    #[error("Rust request source has {expected} bytes, but Cargo crate root has {observed} bytes")]
    SourceBinding {
        /// Byte count retained by the compiler request source identity.
        expected: usize,
        /// Byte count loaded by rust-analyzer from the selected Cargo root.
        observed: usize,
    },
    /// A rust-analyzer byte range exceeded the supplied source buffer.
    #[error("rust-analyzer emitted source range {span:?} outside {source_bytes} bytes")]
    InvalidSpan {
        /// Returned byte coordinate.
        span: ByteSpan,
        /// Exact loaded source length.
        source_bytes: usize,
    },
    /// A platform address cannot represent rust-analyzer's source coordinate.
    #[error("rust-analyzer coordinate {span:?} cannot fit this platform: {source}")]
    Coordinate {
        /// Returned byte coordinate.
        span: ByteSpan,
        /// Exact failed width conversion.
        #[source]
        source: std::num::TryFromIntError,
    },
    /// A required HIR fact was absent after successful Cargo graph loading.
    #[error("rust-analyzer did not produce required semantic fact {fact:?}")]
    MissingSemanticFact {
        /// Exact fact category required by the caller's lowering contract.
        fact: SemanticKind,
    },
    /// The shared canonical fact lane rejected a complete borrowed HIR declaration.
    #[error("Rust semantic admission rejected a declaration: {cause}")]
    Admission {
        /// Exact canonical admission terminal.
        #[source]
        cause: compiler_vocabulary::LoweringUnsupported,
    },
    /// Inference returned an error type where a resolved semantic type was required.
    #[error("rust-analyzer produced an unresolved inferred type")]
    UnresolvedInferredType,
}

/// Converts one local syntax range into validated byte coordinates.
fn checked_span(
    syntax: &ra_ap_syntax::SyntaxNode,
    source: &[u8],
) -> Result<ByteSpan, RustAuthorityError> {
    let span =
        ByteSpan::from_text_range(syntax.text_range()).ok_or(RustAuthorityError::InvalidSpan {
            span: ByteSpan { start: 1, end: 0 },
            source_bytes: source.len(),
        })?;
    bytes_at(source, span).map(|_| span)
}

/// Borrows a source span after checked coordinate conversion and range validation.
fn bytes_at(source: &[u8], span: ByteSpan) -> Result<&[u8], RustAuthorityError> {
    let start = usize::try_from(span.start)
        .map_err(|source| RustAuthorityError::Coordinate { span, source })?;
    let end = usize::try_from(span.end)
        .map_err(|source| RustAuthorityError::Coordinate { span, source })?;
    source
        .get(start..end)
        .ok_or(RustAuthorityError::InvalidSpan {
            span,
            source_bytes: source.len(),
        })
}

/// Converts rust-analyzer's Cargo-derived edition into the sealed compiler profile.
const fn rust_edition(edition: ra_ap_syntax::Edition) -> RustEdition {
    match edition {
        ra_ap_syntax::Edition::Edition2015 => RustEdition::Rust2015,
        ra_ap_syntax::Edition::Edition2018 => RustEdition::Rust2018,
        ra_ap_syntax::Edition::Edition2021 => RustEdition::Rust2021,
        ra_ap_syntax::Edition::Edition2024 => RustEdition::Rust2024,
    }
}
