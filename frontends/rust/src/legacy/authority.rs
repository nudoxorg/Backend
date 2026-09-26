//! Owns the bounded rust-analyzer authority transaction for one Cargo source root.
//! Borrows HIR definitions, types, substitutions, and source maps directly into a caller closure.
//! Never renders, copies, or serializes semantic facts before the shared IR lowerer consumes them.

use std::{
    collections::HashSet,
    fs,
    ops::Deref,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

use backend_semantic::vocabulary::RustEdition;
use ra_ap_base_db::{EditionedFileId, all_crates};
use ra_ap_hir::{
    Adt, AssocItem, Const, EnumVariant, Field, FieldSource, Function, HasSource, Impl, Macro,
    Module, ModuleDef, PathResolution, Semantics, Static, Trait, TypeAlias, TypeInfo,
};
use ra_ap_project_model::{CargoConfig, CargoFeatures, RustLibSource};
use ra_ap_syntax::{
    AstNode,
    ast::{self, HasName, HasVisibility},
};
use ra_ap_vfs::{AbsPathBuf, VfsPath};

use crate::legacy::{LoadError, RustToolchain};

mod query;

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
    /// Monotonic deadline checked before every controllable expensive phase.
    pub deadline: Instant,
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
        } else if Instant::now() >= self.deadline {
            Err(RustAuthorityError::DeadlineExceeded)
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

    /// Borrows this definition's HIR generic parameters.
    ///
    /// Order follows rust-analyzer: lifetimes first, then type and const
    /// parameters. Callers that emit positionally must re-pair with the
    /// written parameter list rather than trusting this order.
    ///
    /// Every definition rust-analyzer models as a public [`ra_ap_hir::GenericDef`]
    /// is projected directly. Fields, enum variants, modules, and macros have no
    /// such handle and yield an empty vector rather than an approximated one.
    #[must_use]
    pub fn generic_params<'analysis>(
        &self,
        database: &'analysis ra_ap_ide_db::RootDatabase,
    ) -> Vec<ra_ap_hir::GenericParam> {
        let generic = match self {
            Self::Field(_) | Self::Variant(_) | Self::Macro(_) | Self::Module(_) => {
                return Vec::new();
            }
            Self::Trait(definition) => ra_ap_hir::GenericDef::from(*definition),
            Self::Implementation(definition) => ra_ap_hir::GenericDef::from(*definition),
            Self::Function(definition) => ra_ap_hir::GenericDef::from(*definition),
            Self::Record(definition) | Self::Enum(definition) => {
                ra_ap_hir::GenericDef::from(*definition)
            }
            Self::TypeAlias(definition) => ra_ap_hir::GenericDef::from(*definition),
            Self::Constant(definition) => ra_ap_hir::GenericDef::from(*definition),
            Self::Static(definition) => ra_ap_hir::GenericDef::from(*definition),
        };
        generic.params(database)
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

/// One written expression and its unrendered inferred type, when inference succeeded.
pub struct RustInferredExpression<'analysis> {
    /// The original expression syntax.
    pub expression: ast::Expr,
    /// The analyzer's semantic result, absent for unresolved expressions.
    pub inferred: Option<TypeInfo<'analysis>>,
}

/// One written local spelling in a resolvable `use` binding.
pub struct RustReexport {
    /// The use item, retained for visibility and documentation projection.
    pub item: ast::Use,
    /// Exact written local name span.
    pub name: ra_ap_syntax::SyntaxNode,
    /// Whether rust-analyzer resolved the imported path.
    pub resolved: bool,
}

fn collect_reexports<'analysis>(
    authority: &RustAuthority<'analysis>,
    item: &ast::Use,
    tree: &ast::UseTree,
    result: &mut Vec<RustReexport>,
) {
    if tree.star_token().is_some() {
        return;
    }
    if let Some(list) = tree.use_tree_list() {
        for child in list.use_trees() {
            collect_reexports(authority, item, &child, result);
        }
        return;
    }
    let Some(path) = tree.path() else {
        return;
    };
    let name = tree
        .rename()
        .and_then(|rename| rename.name())
        .map(|name| name.syntax().clone())
        .or_else(|| {
            path.segments()
                .last()
                .and_then(|segment| segment.name_ref())
                .map(|name| name.syntax().clone())
        });
    let Some(name) = name else {
        return;
    };
    result.push(RustReexport {
        item: item.clone(),
        name,
        resolved: authority.resolve_path(&path).is_some(),
    });
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
    /// The caller's monotonic work deadline elapsed before the next authority phase.
    #[error("Rust semantic authority exceeded its deadline")]
    DeadlineExceeded,
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
        cause: backend_semantic::vocabulary::LoweringUnsupported,
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
