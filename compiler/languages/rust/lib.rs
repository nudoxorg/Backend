//! Extract typed Rust declaration, signature, and docs facts in-process via rust-analyzer.
//! The crate parses Rust sources without spawning LSP or JSON protocol processes.
//! It emits standalone fact records shaped for compiler-ir canonical data admission.

use std::{
    collections::VecDeque,
    fs,
    ops::Range,
    panic::{AssertUnwindSafe, catch_unwind},
    path::{Path, PathBuf},
    process::Command,
};

use ra_ap_project_model::{CargoConfig, RustLibSource};
use ra_ap_syntax::ast::{HasGenericParams, HasName, HasTypeBounds};
use ra_ap_syntax::{AstNode, Edition, SourceFile, SyntaxKind, ast};
use ra_ap_vfs::AbsPathBuf;

/// Failure to establish the native sysroot required for HIR analysis.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    /// The named compiler did not yield a usable sysroot.
    #[error("sysroot unavailable for `{tool}` while attempting `{attempted_source}`")]
    SysrootUnavailable {
        /// Toolchain executable selected by the caller.
        tool: PathBuf,
        /// Deterministic discovery operation that was attempted.
        attempted_source: String,
    },
    /// The selected sysroot path is not a directory.
    #[error("discovered sysroot is not a directory: `{0}`")]
    InvalidSysroot(PathBuf),
    /// The manifest path could not be inspected.
    #[error("manifest inspection failed for `{path}`: {source}")]
    ManifestInspection {
        /// Manifest or parent path inspected.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// rust-analyzer could not construct the requested workspace.
    #[error("rust-analyzer workspace load failed: {source}")]
    Workspace {
        /// Original rust-analyzer failure.
        #[source]
        source: anyhow::Error,
    },
}

/// A caller-selected compiler and its validated sysroot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RustToolchain {
    /// Compiler executable used for deterministic discovery.
    tool: PathBuf,
    /// Sysroot returned by that compiler.
    sysroot: PathBuf,
}

impl RustToolchain {
    /// Resolve and validate the sysroot from the toolchain named by the caller.
    pub fn discover(tool: impl Into<PathBuf>) -> Result<Self, LoadError> {
        let tool = tool.into();
        let attempted_source = "<tool> --print sysroot".to_owned();
        let output = Command::new(&tool)
            .arg("--print")
            .arg("sysroot")
            .output()
            .map_err(|_| LoadError::SysrootUnavailable {
                tool: tool.clone(),
                attempted_source: attempted_source.clone(),
            })?;
        if !output.status.success() {
            return Err(LoadError::SysrootUnavailable {
                tool,
                attempted_source,
            });
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let sysroot = Path::new(text.trim()).to_path_buf();
        if !sysroot.is_dir() {
            return Err(LoadError::InvalidSysroot(sysroot));
        }
        Ok(Self { tool, sysroot })
    }

    /// The compiler executable selected for this analysis.
    pub fn tool(&self) -> &Path {
        &self.tool
    }

    /// The validated sysroot selected for this analysis.
    pub fn sysroot(&self) -> &Path {
        &self.sysroot
    }
}

/// A link found in one declaration's raw documentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocLink {
    /// The text displayed for the link, when the link has explicit display text.
    label: Option<String>,
    /// The exact target written by the author.
    target: String,
    /// Byte span of the complete link syntax in the documentation text.
    span: Range<usize>,
}

impl DocLink {
    /// Explicit display text, or none for an intra-doc code link.
    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    /// The target exactly as written.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// Byte span of the complete link syntax.
    pub fn span(&self) -> Range<usize> {
        self.span.clone()
    }
}

/// Extracts Markdown links and intra-doc links without normalising their targets.
///
/// This deliberately scans bytes rather than Unicode scalar values: the public
/// span contract is a byte range, and non-ASCII labels must not shift it.
pub fn extract_doc_link_targets(doc: &str) -> Vec<DocLink> {
    let bytes = doc.as_bytes();
    let mut links = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] != b'[' {
            cursor += 1;
            continue;
        }
        let Some(close_label) = bytes[cursor + 1..].iter().position(|byte| *byte == b']') else {
            cursor += 1;
            continue;
        };
        let label_end = cursor + 1 + close_label;
        let label = &doc[cursor + 1..label_end];
        if label.as_bytes().contains(&b'[') {
            cursor += 1;
            continue;
        }
        if label.starts_with('`') && label.ends_with('`') {
            links.push(DocLink {
                label: None,
                target: label[1..label.len() - 1].to_owned(),
                span: cursor..label_end + 1,
            });
            cursor = label_end + 1;
            continue;
        }
        if bytes.get(label_end + 1) == Some(&b'(') {
            let target_start = label_end + 2;
            let Some(close_target) = bytes[target_start..].iter().position(|byte| *byte == b')')
            else {
                cursor += 1;
                continue;
            };
            let target_end = target_start + close_target;
            links.push(DocLink {
                label: Some(label.to_owned()),
                target: doc[target_start..target_end].to_owned(),
                span: cursor..target_end + 1,
            });
            cursor = target_end + 1;
            continue;
        }
        if bytes.get(label_end + 1) == Some(&b'`') {
            let target_start = label_end + 2;
            let Some(close_target) = bytes[target_start..].iter().position(|byte| *byte == b'`')
            else {
                cursor += 1;
                continue;
            };
            let target_end = target_start + close_target;
            links.push(DocLink {
                label: None,
                target: doc[cursor + 1..label_end].to_owned(),
                span: cursor..target_end + 1,
            });
            cursor = target_end + 1;
            continue;
        }
        cursor = label_end + 1;
    }
    links
}

/// A deliberately small, closed declaration vocabulary for the frontend.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum DeclarationKind {
    /// A Rust module.
    Module = 1,
    /// A Rust function.
    Function = 2,
    /// A Rust struct.
    Struct = 3,
    /// A Rust union.
    Union = 4,
    /// A Rust enum.
    Enum = 5,
    /// A Rust trait.
    Trait = 6,
    /// An inherent or trait implementation.
    Impl = 7,
    /// A type alias.
    TypeAlias = 8,
    /// A constant.
    Const = 9,
    /// A static item.
    Static = 10,
    /// A re-export.
    Reexport = 11,
    /// A declaration whose lowering panicked and was isolated.
    PerItemPanic = 12,
}

/// A declaration-independent fact retained by the frontend.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationFact {
    /// Fully qualified source path within the loaded crate.
    path: String,
    /// Closed declaration category.
    kind: DeclarationKind,
    /// Raw declaration documentation, if present.
    documentation: Option<String>,
    /// Links extracted from `documentation`.
    doc_links: Vec<DocLink>,
    /// Item's source file, byte span, and typed details.
    source: PathBuf,
    span: Range<usize>,
    details: DeclarationDetails,
}

impl DeclarationFact {
    /// The declaration's fully qualified source path.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// The declaration's closed kind.
    pub const fn kind(&self) -> DeclarationKind {
        self.kind
    }

    /// Raw documentation, when the declaration has a doc comment.
    pub fn documentation(&self) -> Option<&str> {
        self.documentation.as_deref()
    }

    /// Links extracted from the raw documentation.
    pub fn doc_links(&self) -> &[DocLink] {
        &self.doc_links
    }

    /// Source file containing this declaration.
    pub fn source(&self) -> &Path {
        &self.source
    }

    /// Exact UTF-8 byte span in `source`.
    pub fn span(&self) -> Range<usize> {
        self.span.clone()
    }

    /// Typed details specific to this declaration kind.
    pub fn details(&self) -> &DeclarationDetails {
        &self.details
    }
}

/// Shape-specific information retained by a declaration fact.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeclarationDetails {
    /// Module declaration.
    Module,
    /// Function signature and parameters.
    Function(FunctionDetails),
    /// Struct form and fields.
    Struct(StructDetails),
    /// Union fields.
    Union {
        /// Fields declared by the union.
        fields: Box<[FieldFact]>,
    },
    /// Enum variants.
    Enum {
        /// Variants declared by the enum.
        variants: Box<[VariantFact]>,
    },
    /// Trait flags and supertraits.
    Trait {
        /// Whether the trait is unsafe.
        unsafe_trait: bool,
        /// Whether the trait is auto.
        auto_trait: bool,
        /// Supertrait source spellings.
        supers: Box<[String]>,
    },
    /// Implementation target.
    Impl {
        /// Implemented trait, when this is a trait implementation.
        trait_ref: Option<String>,
        /// Implemented self type.
        self_type: String,
        /// Whether no trait reference was written.
        inherent: bool,
    },
    /// Alias target.
    Alias {
        /// Alias target source spelling.
        target: Option<String>,
    },
    /// Constant type and source value.
    Const {
        /// Declared type source spelling.
        ty: Option<String>,
        /// Initializer source spelling.
        value: Option<String>,
    },
    /// Static mutability.
    Static {
        /// Whether `mut` was written.
        mutable: bool,
    },
    /// Re-export source text.
    Reexport {
        /// Re-export source text.
        target: String,
    },
    /// Panic payload retained from isolated lowering.
    PerItemPanic {
        /// Declaration kind selected before the isolated lowering attempt.
        original_kind: DeclarationKind,
        /// Panic message.
        message: String,
    },
}

/// A field in a struct or union.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FieldFact {
    name: Option<String>,
    ty: String,
}

impl FieldFact {
    /// Field name, absent for tuple fields.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
    /// Field type as written.
    pub fn ty(&self) -> &str {
        &self.ty
    }
}

/// An enum variant and its explicitly written discriminant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VariantFact {
    name: String,
    discriminant: Option<String>,
}

impl VariantFact {
    /// Variant name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Explicit discriminant source text, if present.
    pub fn discriminant(&self) -> Option<&str> {
        self.discriminant.as_deref()
    }
}

/// Function signature facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FunctionDetails {
    async_fn: bool,
    unsafe_fn: bool,
    const_fn: bool,
    receiver: Option<String>,
    generics: Option<String>,
    where_predicates: Box<[String]>,
    abi: Option<String>,
    parameters: Box<[ParameterFact]>,
}

impl FunctionDetails {
    /// Whether the function is async.
    pub const fn is_async(&self) -> bool {
        self.async_fn
    }
    /// Whether the function is unsafe.
    pub const fn is_unsafe(&self) -> bool {
        self.unsafe_fn
    }
    /// Whether the function is const.
    pub const fn is_const(&self) -> bool {
        self.const_fn
    }
    /// Receiver source text for a method.
    pub fn receiver(&self) -> Option<&str> {
        self.receiver.as_deref()
    }
    /// Generic parameter source text.
    pub fn generics(&self) -> Option<&str> {
        self.generics.as_deref()
    }
    /// Where predicates as written.
    pub fn where_predicates(&self) -> &[String] {
        &self.where_predicates
    }
    /// Non-default ABI, if written.
    pub fn abi(&self) -> Option<&str> {
        self.abi.as_deref()
    }
    /// Parameters in source order.
    pub fn parameters(&self) -> &[ParameterFact] {
        &self.parameters
    }
}

/// A function parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParameterFact {
    name: String,
    ty: String,
}

impl ParameterFact {
    /// Parameter pattern/name source text.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// Parameter type source text.
    pub fn ty(&self) -> &str {
        &self.ty
    }
}

/// Load a Cargo workspace with rust-analyzer and lower its source declarations.
///
/// Sources are parsed as Rust 2021. Callers using another edition must not use
/// this extractor: edition-specific syntax can be silently lowered with the
/// wrong meaning rather than rejected by this frontend.
pub fn extract_declarations(
    root: impl AsRef<Path>,
    toolchain: &RustToolchain,
) -> Result<Box<[DeclarationFact]>, LoadError> {
    extract_declarations_inner(root, toolchain, None)
}

/// Fault-injection seam that forces one named item through panic isolation.
///
/// This hidden API exists solely to prove sibling survival and retained panic
/// facts. Isolation requires unwinding; a `panic = "abort"` profile terminates
/// the process before a [`DeclarationDetails::PerItemPanic`] fact can be made.
#[doc(hidden)]
pub fn extract_declarations_with_forced_panic(
    root: impl AsRef<Path>,
    toolchain: &RustToolchain,
    item: &str,
) -> Result<Box<[DeclarationFact]>, LoadError> {
    extract_declarations_inner(root, toolchain, Some(item))
}

fn extract_declarations_inner(
    root: impl AsRef<Path>,
    toolchain: &RustToolchain,
    panic_item: Option<&str>,
) -> Result<Box<[DeclarationFact]>, LoadError> {
    let root = root.as_ref();
    let config = CargoConfig {
        sysroot: Some(RustLibSource::Path(AbsPathBuf::assert_utf8(
            toolchain.sysroot().to_path_buf(),
        ))),
        no_deps: false,
        metadata_extra_args: vec!["--offline".to_owned()],
        ..CargoConfig::default()
    };
    let load_config = ra_ap_load_cargo::LoadCargoConfig {
        load_out_dirs_from_check: false,
        with_proc_macro_server: ra_ap_load_cargo::ProcMacroServerChoice::None,
        prefill_caches: false,
        num_worker_threads: 1,
        proc_macro_processes: 0,
    };
    let _workspace = ra_ap_load_cargo::load_workspace_at(root, &config, &load_config, &|_| {})
        .map_err(|source| LoadError::Workspace { source })?;

    let src = root.join("src");
    let mut queue = VecDeque::from([(src.join("lib.rs"), String::new())]);
    let mut facts = Vec::new();
    while let Some((file, module_path)) = queue.pop_front() {
        let text = fs::read_to_string(&file).map_err(|source| LoadError::ManifestInspection {
            path: file.clone(),
            source,
        })?;
        // The committed fixture manifest is edition 2021; do not silently parse it as the host's
        // current edition. The loader owns any vfs-notify resources it creates internally; this
        // frontend retains no notification handle.
        let parse = SourceFile::parse(&text, Edition::Edition2021);
        lower_items(
            parse.tree().syntax(),
            &file,
            &module_path,
            panic_item,
            &src,
            &mut queue,
            &mut facts,
        );
    }
    Ok(facts.into_boxed_slice())
}

fn lower_items(
    root: &ra_ap_syntax::SyntaxNode,
    file: &Path,
    module_path: &str,
    panic_item: Option<&str>,
    module_root: &Path,
    queue: &mut VecDeque<(PathBuf, String)>,
    facts: &mut Vec<DeclarationFact>,
) {
    for node in root.children() {
        let Some(kind) = kind_for(node.kind()) else {
            continue;
        };
        let name = item_name(&node, kind);
        let full_path = if module_path.is_empty() {
            name.clone()
        } else {
            format!("{module_path}::{name}")
        };
        // catch_unwind isolates item lowering only in unwind profiles; the public seam documents
        // the panic=abort boundary.
        let lowered = catch_unwind(AssertUnwindSafe(|| {
            if panic_item == Some(name.as_str()) {
                std::panic::panic_any("forced item lowering panic");
            }
            details_for(&node, kind)
        }));
        let (kind, details) = match lowered {
            Ok(details) => (kind, details),
            Err(payload) => (
                DeclarationKind::PerItemPanic,
                DeclarationDetails::PerItemPanic {
                    original_kind: kind,
                    message: panic_message(payload),
                },
            ),
        };
        let start = usize::from(node.text_range().start());
        let end = usize::from(node.text_range().end());
        let documentation = doc_text(&node);
        let doc_links = documentation
            .as_deref()
            .map(extract_doc_link_targets)
            .unwrap_or_default();
        let reexport_names = if kind == DeclarationKind::Reexport {
            ast::Use::cast(node.clone())
                .and_then(|x| x.use_tree())
                .map(|x| use_leaf_names(&x))
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let paths = if reexport_names.is_empty() {
            vec![full_path.clone()]
        } else {
            reexport_names
                .into_iter()
                .map(|name| {
                    if module_path.is_empty() {
                        name
                    } else {
                        format!("{module_path}::{name}")
                    }
                })
                .collect()
        };
        for path in paths {
            facts.push(DeclarationFact {
                path,
                kind,
                documentation: documentation.clone(),
                doc_links: doc_links.clone(),
                source: file.to_path_buf(),
                span: start..end,
                details: details.clone(),
            });
        }
        if kind == DeclarationKind::Module
            && ast::Module::cast(node.clone()).is_some_and(|m| m.item_list().is_none())
        {
            queue.push_back((
                module_root
                    .join(module_path.replace("::", "/"))
                    .join(format!("{name}.rs")),
                full_path.clone(),
            ));
        }
        if matches!(
            kind,
            DeclarationKind::Impl | DeclarationKind::Trait | DeclarationKind::Module
        ) {
            let impl_path = (kind == DeclarationKind::Impl)
                .then(|| impl_self_path(&node))
                .flatten();
            let child_path = match kind {
                DeclarationKind::Module => full_path.as_str(),
                DeclarationKind::Impl => impl_path.as_deref().unwrap_or(full_path.as_str()),
                DeclarationKind::Trait => full_path.as_str(),
                _ => module_path,
            };
            lower_items(
                &node,
                file,
                child_path,
                panic_item,
                module_root,
                queue,
                facts,
            );
        }
    }
    for node in root.children() {
        if kind_for(node.kind()).is_none() {
            lower_items(
                &node,
                file,
                module_path,
                panic_item,
                module_root,
                queue,
                facts,
            );
        }
    }
}

fn kind_for(kind: SyntaxKind) -> Option<DeclarationKind> {
    Some(match kind {
        SyntaxKind::MODULE => DeclarationKind::Module,
        SyntaxKind::FN => DeclarationKind::Function,
        SyntaxKind::STRUCT => DeclarationKind::Struct,
        SyntaxKind::UNION => DeclarationKind::Union,
        SyntaxKind::ENUM => DeclarationKind::Enum,
        SyntaxKind::TRAIT => DeclarationKind::Trait,
        SyntaxKind::IMPL => DeclarationKind::Impl,
        SyntaxKind::TYPE_ALIAS => DeclarationKind::TypeAlias,
        SyntaxKind::CONST => DeclarationKind::Const,
        SyntaxKind::STATIC => DeclarationKind::Static,
        SyntaxKind::USE => DeclarationKind::Reexport,
        _ => return None,
    })
}

fn panic_message(payload: Box<dyn std::any::Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_owned();
    }
    "non-string panic payload".to_owned()
}

fn item_name(node: &ra_ap_syntax::SyntaxNode, kind: DeclarationKind) -> String {
    if kind == DeclarationKind::Reexport {
        return ast::Use::cast(node.clone())
            .and_then(|x| x.use_tree())
            .and_then(|x| use_leaf_names(&x).into_iter().next())
            .unwrap_or_else(|| "<reexport>".to_owned());
    }
    let name = match kind {
        DeclarationKind::Module => ast::Module::cast(node.clone()).and_then(|x| x.name()),
        DeclarationKind::Function => ast::Fn::cast(node.clone()).and_then(|x| x.name()),
        DeclarationKind::Struct => ast::Struct::cast(node.clone()).and_then(|x| x.name()),
        DeclarationKind::Union => ast::Union::cast(node.clone()).and_then(|x| x.name()),
        DeclarationKind::Enum => ast::Enum::cast(node.clone()).and_then(|x| x.name()),
        DeclarationKind::Trait => ast::Trait::cast(node.clone()).and_then(|x| x.name()),
        DeclarationKind::TypeAlias => ast::TypeAlias::cast(node.clone()).and_then(|x| x.name()),
        DeclarationKind::Const => ast::Const::cast(node.clone()).and_then(|x| x.name()),
        DeclarationKind::Static => ast::Static::cast(node.clone()).and_then(|x| x.name()),
        DeclarationKind::Impl => return impl_identity(node),
        DeclarationKind::PerItemPanic | DeclarationKind::Reexport => None,
    };
    name.map(|x| x.syntax().text().to_string())
        .unwrap_or_else(|| "<anonymous>".to_owned())
}

fn doc_text(node: &ra_ap_syntax::SyntaxNode) -> Option<String> {
    let docs: Vec<String> = node
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::COMMENT && token.text().starts_with("///"))
        .map(|token| {
            token
                .text()
                .trim_start_matches("///")
                .trim_start()
                .to_owned()
        })
        .collect();
    (!docs.is_empty()).then(|| docs.join("\n"))
}

fn details_for(node: &ra_ap_syntax::SyntaxNode, kind: DeclarationKind) -> DeclarationDetails {
    match kind {
        DeclarationKind::Function => DeclarationDetails::Function(function_details(node)),
        DeclarationKind::Struct => DeclarationDetails::Struct(struct_details(node)),
        DeclarationKind::Union => DeclarationDetails::Union {
            fields: ast::Union::cast(node.clone())
                .and_then(|x| x.record_field_list())
                .map(|x| record_fields(&x))
                .unwrap_or_default()
                .into_boxed_slice(),
        },
        DeclarationKind::Enum => DeclarationDetails::Enum {
            variants: ast::Enum::cast(node.clone())
                .and_then(|x| x.variant_list())
                .map(|x| {
                    x.variants()
                        .filter_map(|v| {
                            Some(VariantFact {
                                name: v.name()?.syntax().text().to_string(),
                                discriminant: discriminant(&v),
                            })
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
                .into_boxed_slice(),
        },
        DeclarationKind::Trait => trait_details(node),
        DeclarationKind::Impl => impl_details(node),
        DeclarationKind::TypeAlias => DeclarationDetails::Alias {
            target: ast::TypeAlias::cast(node.clone())
                .and_then(|x| x.ty())
                .map(|x| x.syntax().text().to_string()),
        },
        DeclarationKind::Const => DeclarationDetails::Const {
            ty: ast::Const::cast(node.clone())
                .and_then(|x| x.ty())
                .map(|x| x.syntax().text().to_string()),
            value: const_value(node),
        },
        DeclarationKind::Static => DeclarationDetails::Static {
            mutable: ast::Static::cast(node.clone()).is_some_and(|x| x.mut_token().is_some()),
        },
        DeclarationKind::Reexport => DeclarationDetails::Reexport {
            target: ast::Use::cast(node.clone())
                .and_then(|x| x.use_tree())
                .map(|x| x.syntax().text().to_string())
                .unwrap_or_default(),
        },
        DeclarationKind::Module => DeclarationDetails::Module,
        DeclarationKind::PerItemPanic => DeclarationDetails::PerItemPanic {
            original_kind: DeclarationKind::PerItemPanic,
            message: "lowering panic".to_owned(),
        },
    }
}

fn function_details(node: &ra_ap_syntax::SyntaxNode) -> FunctionDetails {
    let function = ast::Fn::cast(node.clone());
    let parameters = function
        .as_ref()
        .and_then(|x| x.param_list())
        .map(|list| {
            list.params()
                .filter_map(|p| {
                    Some(ParameterFact {
                        name: p
                            .pat()
                            .map(|x| x.syntax().text().to_string())
                            .unwrap_or_default(),
                        ty: p.ty()?.syntax().text().to_string(),
                    })
                })
                .chain(list.self_param().map(|p| ParameterFact {
                    name: p.syntax().text().to_string(),
                    ty: p.syntax().text().to_string(),
                }))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    FunctionDetails {
        async_fn: function.as_ref().is_some_and(|x| x.async_token().is_some()),
        unsafe_fn: function
            .as_ref()
            .is_some_and(|x| x.unsafe_token().is_some()),
        const_fn: function.as_ref().is_some_and(|x| x.const_token().is_some()),
        receiver: parameters
            .iter()
            .find(|p| p.name.contains("self"))
            .map(|p| p.name.clone()),
        generics: function
            .as_ref()
            .and_then(|x| x.generic_param_list())
            .map(|x| x.syntax().text().to_string()),
        where_predicates: function
            .as_ref()
            .and_then(|x| x.where_clause())
            .map(|x| {
                x.predicates()
                    .map(|p| p.syntax().text().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        abi: function
            .and_then(|x| x.abi())
            .and_then(|x| x.string_token())
            .map(|x| x.text().trim_matches('"').to_owned()),
        parameters: parameters.into_boxed_slice(),
    }
}

fn struct_details(node: &ra_ap_syntax::SyntaxNode) -> StructDetails {
    let structure = ast::Struct::cast(node.clone());
    let (form, fields) = match structure.and_then(|x| x.field_list()) {
        Some(ast::FieldList::RecordFieldList(list)) => (StructForm::Named, record_fields(&list)),
        Some(ast::FieldList::TupleFieldList(list)) => (StructForm::Tuple, tuple_fields(&list)),
        None => (StructForm::Unit, Vec::new()),
    };
    StructDetails {
        form,
        fields: fields.into_boxed_slice(),
    }
}

/// Struct declaration form.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StructForm {
    /// Braced fields.
    Named,
    /// Positional fields.
    Tuple,
    /// No fields.
    Unit,
}
/// Struct facts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructDetails {
    form: StructForm,
    fields: Box<[FieldFact]>,
}
impl StructDetails {
    /// Return the declaration form.
    pub const fn form(&self) -> StructForm {
        self.form
    }
    /// Return fields in source order.
    pub fn fields(&self) -> &[FieldFact] {
        &self.fields
    }
}

fn record_fields(list: &ast::RecordFieldList) -> Vec<FieldFact> {
    list.fields()
        .filter_map(|field| {
            Some(FieldFact {
                name: Some(field.name()?.syntax().text().to_string()),
                ty: field.ty()?.syntax().text().to_string(),
            })
        })
        .collect()
}
fn tuple_fields(list: &ast::TupleFieldList) -> Vec<FieldFact> {
    list.fields()
        .filter_map(|field| {
            Some(FieldFact {
                name: None,
                ty: field.ty()?.syntax().text().to_string(),
            })
        })
        .collect()
}
fn trait_details(node: &ra_ap_syntax::SyntaxNode) -> DeclarationDetails {
    let item = ast::Trait::cast(node.clone());
    DeclarationDetails::Trait {
        unsafe_trait: item.as_ref().is_some_and(|x| x.unsafe_token().is_some()),
        auto_trait: item.as_ref().is_some_and(|x| x.auto_token().is_some()),
        supers: item
            .and_then(|x| x.type_bound_list())
            .map(|x| {
                x.bounds()
                    .filter_map(|b| b.ty().map(|x| x.syntax().text().to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
            .into_boxed_slice(),
    }
}
fn impl_details(node: &ra_ap_syntax::SyntaxNode) -> DeclarationDetails {
    let types: Vec<String> = node
        .children()
        .filter_map(|x| ast::Type::cast(x).map(|x| x.syntax().text().to_string()))
        .collect();
    if types.len() >= 2 {
        DeclarationDetails::Impl {
            trait_ref: types.first().cloned(),
            self_type: types.get(1).cloned().unwrap_or_default(),
            inherent: false,
        }
    } else {
        DeclarationDetails::Impl {
            trait_ref: None,
            self_type: types.into_iter().next().unwrap_or_default(),
            inherent: true,
        }
    }
}
fn impl_self_path(node: &ra_ap_syntax::SyntaxNode) -> Option<String> {
    match impl_details(node) {
        DeclarationDetails::Impl { self_type, .. } if !self_type.is_empty() => Some(self_type),
        _ => None,
    }
}
fn impl_identity(node: &ra_ap_syntax::SyntaxNode) -> String {
    match impl_details(node) {
        DeclarationDetails::Impl {
            trait_ref: Some(trait_ref),
            self_type,
            ..
        } => format!("impl {trait_ref} for {self_type}"),
        DeclarationDetails::Impl { self_type, .. } => format!("impl {self_type}"),
        _ => "impl <anonymous>".to_owned(),
    }
}
fn direct_expression(node: &ra_ap_syntax::SyntaxNode) -> Option<String> {
    node.children()
        .find_map(ast::Expr::cast)
        .map(|x| x.syntax().text().to_string())
}

fn const_value(node: &ra_ap_syntax::SyntaxNode) -> Option<String> {
    if let Some(value) = direct_expression(node) {
        return Some(value);
    }
    let equals = node.children_with_tokens().find_map(|element| {
        element
            .into_token()
            .filter(|token| token.kind() == SyntaxKind::EQ)
    })?;
    let mut value = String::new();
    for element in node.descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        if token.text_range().start() <= equals.text_range().start() {
            continue;
        }
        if token.kind() == SyntaxKind::SEMICOLON {
            break;
        }
        if !token.kind().is_trivia() {
            value.push_str(token.text());
        }
    }
    (!value.is_empty()).then_some(value)
}

fn discriminant(variant: &ast::Variant) -> Option<String> {
    let equals = variant.eq_token()?;
    let mut after_equals = false;
    for element in variant.syntax().descendants_with_tokens() {
        let Some(token) = element.into_token() else {
            continue;
        };
        if token.text_range() == equals.text_range() {
            after_equals = true;
            continue;
        }
        if after_equals && !token.kind().is_trivia() {
            return Some(token.text().to_owned());
        }
    }
    None
}

fn use_leaf_names(tree: &ast::UseTree) -> Vec<String> {
    if let Some(list) = tree.use_tree_list() {
        return list.use_trees().flat_map(|x| use_leaf_names(&x)).collect();
    }
    if let Some(name) = tree.rename().and_then(|x| x.name()) {
        return vec![name.syntax().text().to_string()];
    }
    tree.path()
        .and_then(|path| path.segments().last())
        .and_then(|segment| segment.name_ref())
        .map(|name| vec![name.syntax().text().to_string()])
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_and_intra_doc_links_keep_exact_byte_spans() {
        let doc = "Préface [appel](crate::appel) and [`Target`]";
        let links = extract_doc_link_targets(doc);
        assert_eq!(links.len(), 2);
        assert_eq!(links[0].target, "crate::appel");
        assert_eq!(links[0].label.as_deref(), Some("appel"));
        assert_eq!(&doc[links[0].span.clone()], "[appel](crate::appel)");
        assert_eq!(links[1].target, "Target");
        assert_eq!(links[1].label, None);
        assert_eq!(&doc[links[1].span.clone()], "[`Target`]");
    }

    #[test]
    fn malformed_link_is_omitted_without_losing_following_links() {
        let links = extract_doc_link_targets("[broken(x [ok](target)");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "target");
    }

    #[test]
    fn unavailable_named_toolchain_is_typed_instead_of_falling_back() {
        let result = RustToolchain::discover("/definitely/not-a-rustc");
        assert!(matches!(
            result,
            Err(LoadError::SysrootUnavailable { tool, attempted_source })
                if tool == Path::new("/definitely/not-a-rustc")
                    && attempted_source == "<tool> --print sysroot"
        ));
    }
}
