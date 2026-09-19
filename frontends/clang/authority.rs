//! Executable libclang semantic authority and canonical native projection.
//!
//! This module owns the real C/C++ authority path.  It loads libclang, applies
//! the project's compilation database and host include search path, extracts
//! owned semantic data, and only then projects that data into the bounded
//! `backend-compile` native record boundary.  Tree-sitter remains available
//! through [`crate::syntax_frontend`] as a syntax baseline; it is never used
//! to manufacture a semantic claim here.

use crate::{
    compile_commands::CompileCommands,
    extract::{extract_file_checked, extract_unsaved_checked},
    oracle::{ClangOracle, OracleDiagnostic, OracleNamespace, OracleRecord, OracleType, Reference},
    system_includes,
};
use backend_compile::{AuthorityError, NativeRecord, NativeRecordKind};
use clang::{Clang, Index};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

const MAX_PROJECT_SOURCES: usize = 100_000;
const MAX_NATIVE_RECORDS: usize = backend_compile::MAX_NATIVE_RECORDS;
const PAYLOAD_PREFIX: &str = "clang-semantic-v2";

/// A checked C/C++ semantic authority rooted at one immutable project path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClangProject {
    root: PathBuf,
}

impl ClangProject {
    /// Opens a project root without following a symlink at the root.
    ///
    /// # Errors
    /// Returns [`ClangAuthorityError`] when `root` is not an absolute regular
    /// directory or cannot be canonicalized.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, ClangAuthorityError> {
        let requested = root.as_ref();
        if !requested.is_absolute() {
            return Err(ClangAuthorityError::RelativePath {
                path: requested.to_path_buf(),
            });
        }
        let metadata =
            fs::symlink_metadata(requested).map_err(|source| ClangAuthorityError::RootIo {
                path: requested.to_path_buf(),
                source,
            })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(ClangAuthorityError::RootNotDirectory {
                path: requested.to_path_buf(),
            });
        }
        let root = requested
            .canonicalize()
            .map_err(|source| ClangAuthorityError::RootIo {
                path: requested.to_path_buf(),
                source,
            })?;
        Ok(Self { root })
    }

    /// Returns the canonical project root retained by this authority.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Runs libclang over the project's compilation database and source files.
    ///
    /// Every returned value is owned and remains valid after libclang is
    /// released.  A parse failure is preserved as a typed error instead of
    /// being converted into an empty semantic result.
    ///
    /// # Errors
    /// Returns [`ClangAuthorityError`] when libclang is unavailable, project
    /// discovery exceeds its bound, or a selected translation unit fails.
    pub fn analyze(&self) -> Result<ClangOracle, ClangAuthorityError> {
        let clang = Clang::new().map_err(|error| ClangAuthorityError::Libclang {
            message: error.to_string(),
        })?;
        let index = Index::new(&clang, false, false);
        let database = CompileCommands::load(&self.root);
        let system = system_includes::args();
        let sources = source_files(&self.root)?;
        let mut oracle = ClangOracle::default();
        for source in sources {
            let database_args = database
                .as_ref()
                .and_then(|commands| commands.args_for(&source));
            let defaults = default_arguments(&source);
            let selected = database_args.as_deref().unwrap_or(&defaults);
            let arguments = system
                .iter()
                .chain(selected.iter())
                .map(String::as_str)
                .collect::<Vec<_>>();
            let partial = extract_file_checked(&index, &source, &arguments).map_err(|message| {
                ClangAuthorityError::Parse {
                    path: source.clone(),
                    message,
                }
            })?;
            merge_oracle(&mut oracle, partial);
        }
        Ok(oracle)
    }
}

/// Runs libclang over one source file with an exact argument list.
///
/// # Errors
/// Returns [`ClangAuthorityError`] when libclang cannot load or parse the file.
pub fn analyze_file(
    path: impl AsRef<Path>,
    arguments: &[String],
) -> Result<ClangOracle, ClangAuthorityError> {
    let path = checked_file(path.as_ref())?;
    let clang = Clang::new().map_err(|error| ClangAuthorityError::Libclang {
        message: error.to_string(),
    })?;
    let index = Index::new(&clang, false, false);
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    extract_file_checked(&index, &path, &arguments)
        .map_err(|message| ClangAuthorityError::Parse { path, message })
}

/// Runs libclang over exact in-memory source bytes under a virtual path.
///
/// # Errors
/// Returns [`ClangAuthorityError`] when the filename is not absolute, the
/// bytes are not UTF-8 (libclang's unsaved source API requires text), or the
/// translation unit cannot be parsed.
pub fn analyze_source(
    virtual_path: impl AsRef<Path>,
    source: &[u8],
    arguments: &[String],
) -> Result<ClangOracle, ClangAuthorityError> {
    let path = virtual_path.as_ref();
    if !path.is_absolute() {
        return Err(ClangAuthorityError::RelativePath {
            path: path.to_path_buf(),
        });
    }
    let source = std::str::from_utf8(source).map_err(|_| ClangAuthorityError::SourceEncoding {
        path: path.to_path_buf(),
    })?;
    let clang = Clang::new().map_err(|error| ClangAuthorityError::Libclang {
        message: error.to_string(),
    })?;
    let index = Index::new(&clang, false, false);
    let arguments = arguments.iter().map(String::as_str).collect::<Vec<_>>();
    extract_unsaved_checked(&index, path, source, &arguments).map_err(|message| {
        ClangAuthorityError::Parse {
            path: path.to_path_buf(),
            message,
        }
    })
}

/// Projects an owned libclang oracle into language-admitted native records.
///
/// The values use a versioned, escaped cell grammar.  Every record still goes
/// through [`crate::ClangSemanticAdapter`] when the common extraction boundary
/// is used, while direct users can retain the typed [`ClangOracle`] alongside
/// these records for richer projections.
///
/// # Errors
/// Returns [`AuthorityError`] when the projected record set exceeds the native
/// protocol bound or a record key cannot be admitted.
pub fn native_records(oracle: &ClangOracle) -> Result<Vec<NativeRecord>, AuthorityError> {
    let mut records = Vec::new();
    for namespace in &oracle.namespaces {
        push_declaration(
            &mut records,
            namespace_key(namespace),
            "namespace",
            namespace.name.as_str(),
        )?;
    }
    for record in &oracle.records {
        push_declaration(
            &mut records,
            declaration_key("record", &record.usr),
            record_kind(record),
            record.name.as_str(),
        )?;
        for (index, ty) in record.super_types.iter().enumerate() {
            push_type(&mut records, type_key(&record.usr, "base", index), ty)?;
        }
        for (index, field) in record.fields.iter().enumerate() {
            push_declaration(
                &mut records,
                declaration_key("field", &field.usr),
                "field",
                field.name.as_str(),
            )?;
            push_type(
                &mut records,
                type_key(&field.usr, "field", index),
                &field.ty,
            )?;
        }
    }
    for function in &oracle.functions {
        push_declaration(
            &mut records,
            declaration_key("function", &function.usr),
            "function",
            function.name.as_str(),
        )?;
        push_type(
            &mut records,
            type_key(&function.usr, "return", 0),
            &function.ret,
        )?;
        for (index, parameter) in function.params.iter().enumerate() {
            push_declaration(
                &mut records,
                declaration_key("parameter", &format!("{}:{index}", function.usr)),
                "parameter",
                parameter.name.as_str(),
            )?;
            push_type(
                &mut records,
                type_key(&function.usr, "parameter", index),
                &parameter.ty,
            )?;
        }
    }
    for enumeration in &oracle.enums {
        push_declaration(
            &mut records,
            declaration_key("enum", &enumeration.usr),
            "enum",
            enumeration.name.as_str(),
        )?;
    }
    for variant in &oracle.variants {
        push_declaration(
            &mut records,
            declaration_key("variant", &variant.usr),
            "enumerator",
            variant.name.as_str(),
        )?;
    }
    for alias in &oracle.aliases {
        push_declaration(
            &mut records,
            declaration_key("alias", &alias.usr),
            "type-alias",
            alias.name.as_str(),
        )?;
        push_type(
            &mut records,
            type_key(&alias.usr, "alias", 0),
            &alias.target,
        )?;
    }
    for variable in &oracle.vars {
        push_declaration(
            &mut records,
            declaration_key("variable", &variable.usr),
            "variable",
            variable.name.as_str(),
        )?;
        push_type(
            &mut records,
            type_key(&variable.usr, "variable", 0),
            &variable.ty,
        )?;
    }
    for (index, reference) in oracle.references.iter().enumerate() {
        push_reference(&mut records, reference, index)?;
    }
    for (index, diagnostic) in oracle.diagnostics.iter().enumerate() {
        push_diagnostic(&mut records, diagnostic, index)?;
    }
    // A source row is an explicit positive dependency witness for each file
    // observed by this oracle.  It lets downstream reconciliation distinguish
    // an empty but parsed file from a lane that never ran.
    let mut files = HashSet::new();
    for path in oracle_files(oracle) {
        if files.insert(path.clone()) {
            push_record(
                &mut records,
                NativeRecordKind::Dependency,
                format!("clang/source/{}", escape_key(&path)),
                b"present".to_vec(),
            )?;
        }
    }
    records.sort_by(|left, right| (left.kind(), left.key()).cmp(&(right.kind(), right.key())));
    if records.len() > MAX_NATIVE_RECORDS {
        return Err(AuthorityError::Extraction(
            "Clang semantic record set exceeds native protocol capacity".to_owned(),
        ));
    }
    Ok(records)
}

/// Typed failure from the executable Clang authority.
#[derive(Debug, thiserror::Error)]
pub enum ClangAuthorityError {
    /// A caller supplied a relative path.
    #[error("Clang authority path is relative: {path}", path = path.display())]
    RelativePath {
        /// Caller-selected path.
        path: PathBuf,
    },
    /// The project root could not be inspected.
    #[error("cannot inspect Clang project root {path}: {source}", path = path.display())]
    RootIo {
        /// Path involved in the filesystem operation.
        path: PathBuf,
        /// Original filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The project root was not a real directory.
    #[error("Clang project root is not a directory: {path}", path = path.display())]
    RootNotDirectory {
        /// Caller-selected root path.
        path: PathBuf,
    },
    /// The selected translation unit source is not UTF-8.
    #[error("Clang source is not UTF-8: {path}", path = path.display())]
    SourceEncoding {
        /// Virtual source path associated with the invalid bytes.
        path: PathBuf,
    },
    /// libclang could not be loaded from the configured runtime.
    #[error("libclang authority is unavailable: {message}")]
    Libclang {
        /// Native loader or runtime error text.
        message: String,
    },
    /// libclang rejected a translation unit.
    #[error("libclang could not parse {path}: {message}", path = path.display())]
    Parse {
        /// Source path passed to libclang.
        path: PathBuf,
        /// Native parser error text.
        message: String,
    },
    /// The project contains too many source files for one authority run.
    #[error("Clang project has more than {maximum} source files")]
    SourceCount {
        /// Maximum admitted source count.
        maximum: usize,
    },
    /// A source file disappeared or changed type during discovery.
    #[error("Clang source path is not a regular file: {path}", path = path.display())]
    InvalidSource {
        /// Source path that failed the regular-file check.
        path: PathBuf,
    },
}

fn checked_file(path: &Path) -> Result<PathBuf, ClangAuthorityError> {
    if !path.is_absolute() {
        return Err(ClangAuthorityError::RelativePath {
            path: path.to_path_buf(),
        });
    }
    let metadata = fs::symlink_metadata(path).map_err(|source| ClangAuthorityError::RootIo {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ClangAuthorityError::InvalidSource {
            path: path.to_path_buf(),
        });
    }
    path.canonicalize()
        .map_err(|source| ClangAuthorityError::RootIo {
            path: path.to_path_buf(),
            source,
        })
}

fn source_files(root: &Path) -> Result<Vec<PathBuf>, ClangAuthorityError> {
    let mut pending = vec![root.to_path_buf()];
    let mut output = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|source| ClangAuthorityError::RootIo {
            path: directory.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| ClangAuthorityError::RootIo {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| ClangAuthorityError::RootIo {
                    path: path.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() && is_source(&path) {
                output.push(path);
                if output.len() > MAX_PROJECT_SOURCES {
                    return Err(ClangAuthorityError::SourceCount {
                        maximum: MAX_PROJECT_SOURCES,
                    });
                }
            }
        }
    }
    output.sort();
    Ok(output)
}

fn is_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("c" | "cc" | "cpp" | "cxx" | "C" | "c++" | "m" | "mm")
    )
}

fn default_arguments(path: &Path) -> Vec<String> {
    if matches!(
        path.extension().and_then(|extension| extension.to_str()),
        Some("cc" | "cpp" | "cxx" | "C" | "c++" | "mm")
    ) {
        vec!["-std=c++17".to_owned(), "-x".to_owned(), "c++".to_owned()]
    } else {
        vec!["-std=c11".to_owned()]
    }
}

fn merge_oracle(destination: &mut ClangOracle, mut source: ClangOracle) {
    destination.namespaces.append(&mut source.namespaces);
    destination.records.append(&mut source.records);
    destination.fields.append(&mut source.fields);
    destination.functions.append(&mut source.functions);
    destination.enums.append(&mut source.enums);
    destination.variants.append(&mut source.variants);
    destination.aliases.append(&mut source.aliases);
    destination.vars.append(&mut source.vars);
    destination.references.append(&mut source.references);
    destination.diagnostics.append(&mut source.diagnostics);
    dedup(&mut destination.namespaces, |value| value.usr.as_str());
    dedup(&mut destination.records, |value| value.usr.as_str());
    dedup(&mut destination.fields, |value| value.usr.as_str());
    dedup(&mut destination.functions, |value| value.usr.as_str());
    dedup(&mut destination.enums, |value| value.usr.as_str());
    dedup(&mut destination.variants, |value| value.usr.as_str());
    dedup(&mut destination.aliases, |value| value.usr.as_str());
    dedup(&mut destination.vars, |value| value.usr.as_str());
    destination.references.sort_by_key(|value| {
        (
            value.owner.clone(),
            value.target.clone(),
            value.byte_start,
            value.byte_end,
        )
    });
    destination.references.dedup_by(|left, right| {
        left.owner == right.owner
            && left.target == right.target
            && left.byte_start == right.byte_start
            && left.byte_end == right.byte_end
    });
}

fn dedup<T>(values: &mut Vec<T>, key: impl Fn(&T) -> &str) {
    let mut seen = HashSet::new();
    values.retain(|value| seen.insert(key(value).to_owned()));
}

fn declaration_key(kind: &str, usr: &str) -> String {
    format!("clang/{kind}/{usr}")
}

fn namespace_key(value: &OracleNamespace) -> String {
    declaration_key("namespace", &value.usr)
}

fn record_kind(_value: &OracleRecord) -> &'static str {
    "record"
}

fn type_key(owner: &str, lane: &str, index: usize) -> String {
    format!("clang/type/{owner}/{lane}/{index}")
}

fn push_declaration(
    records: &mut Vec<NativeRecord>,
    key: String,
    kind: &str,
    name: &str,
) -> Result<(), AuthorityError> {
    let value = payload(&[
        ("kind", kind),
        ("definition", "definition"),
        ("storage", "none"),
        ("virtuality", "non-virtual"),
        ("name", name),
    ]);
    push_record(records, NativeRecordKind::Declaration, key, value)
}

fn push_type(
    records: &mut Vec<NativeRecord>,
    key: String,
    value: &OracleType,
) -> Result<(), AuthorityError> {
    let rendered = render_type(value);
    let value = payload(&[
        ("kind", type_kind(value)),
        ("const", "false"),
        ("volatile", "false"),
        ("restrict", "false"),
        ("display", rendered.as_str()),
    ]);
    push_record(records, NativeRecordKind::Type, key, value)
}

fn push_reference(
    records: &mut Vec<NativeRecord>,
    reference: &Reference,
    index: usize,
) -> Result<(), AuthorityError> {
    let key = format!(
        "clang/edge/{}/{}-{index}",
        reference.owner, reference.target
    );
    push_record(
        records,
        NativeRecordKind::Edge,
        key,
        payload(&[("relation", "reference-local")]),
    )
}

fn push_diagnostic(
    records: &mut Vec<NativeRecord>,
    diagnostic: &OracleDiagnostic,
    index: usize,
) -> Result<(), AuthorityError> {
    push_record(
        records,
        NativeRecordKind::Diagnostic,
        format!("clang/diagnostic/{index}"),
        payload(&[
            ("severity", diagnostic.severity.as_str()),
            ("message", diagnostic.message.as_str()),
        ]),
    )
}

fn push_record(
    records: &mut Vec<NativeRecord>,
    kind: NativeRecordKind,
    key: String,
    value: Vec<u8>,
) -> Result<(), AuthorityError> {
    if records.len() >= MAX_NATIVE_RECORDS {
        return Err(AuthorityError::Extraction(
            "Clang semantic record set exceeds native protocol capacity".to_owned(),
        ));
    }
    records.push(
        NativeRecord::new(kind, key, value)
            .map_err(|error| AuthorityError::Extraction(error.to_string()))?,
    );
    Ok(())
}

fn payload(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut output = String::from(PAYLOAD_PREFIX);
    for (name, value) in fields {
        output.push(';');
        output.push_str(name);
        output.push('=');
        output.push_str(&escape_cell(value));
    }
    output.into_bytes()
}

fn escape_cell(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'%' => output.push_str("%25"),
            b';' => output.push_str("%3B"),
            b'=' => output.push_str("%3D"),
            _ => output.push(byte as char),
        }
    }
    output
}

fn escape_key(value: &Path) -> String {
    escape_cell(&value.to_string_lossy())
}

fn type_kind(value: &OracleType) -> &'static str {
    match value {
        OracleType::Void => "builtin",
        OracleType::Bool | OracleType::Integer { .. } | OracleType::IntegerArch { .. } => "builtin",
        OracleType::Float { .. } | OracleType::FloatArch => "builtin",
        OracleType::ConstPointer(_) | OracleType::MutPointer(_) => "pointer",
        OracleType::LValueRef { .. } => "lvalue-reference",
        OracleType::RValueRef(_) => "rvalue-reference",
        OracleType::Array { .. } | OracleType::Slice(_) => "array",
        OracleType::FnPtr { .. } => "function",
        OracleType::Named { .. } => "named",
        OracleType::TypeVar(_) => "template-parameter",
        OracleType::Inferred => "unknown",
    }
}

fn render_type(value: &OracleType) -> String {
    match value {
        OracleType::Void => "void".to_owned(),
        OracleType::Bool => "bool".to_owned(),
        OracleType::Integer { signed, bits } => {
            format!("{}int{bits}", if *signed { "" } else { "u" })
        }
        OracleType::IntegerArch { signed } => format!("{}int", if *signed { "" } else { "u" }),
        OracleType::Float { bits } => format!("float{bits}"),
        OracleType::FloatArch => "float".to_owned(),
        OracleType::ConstPointer(inner) => format!("const*{}", render_type(inner)),
        OracleType::MutPointer(inner) => format!("*{}", render_type(inner)),
        OracleType::LValueRef { mutable, ty } => {
            format!("{}&{}", if *mutable { "mut " } else { "" }, render_type(ty))
        }
        OracleType::RValueRef(inner) => format!("{}&&", render_type(inner)),
        OracleType::Array { ty, len } => format!("{}[{len}]", render_type(ty)),
        OracleType::Slice(inner) => format!("{}[]", render_type(inner)),
        OracleType::FnPtr { ret, params } => format!(
            "fn({}) -> {}",
            params.iter().map(render_type).collect::<Vec<_>>().join(","),
            render_type(ret)
        ),
        OracleType::Named { name, args, .. } => {
            if args.is_empty() {
                name.clone()
            } else {
                format!(
                    "{}<{}>",
                    name,
                    args.iter().map(render_type).collect::<Vec<_>>().join(",")
                )
            }
        }
        OracleType::TypeVar(name) => name.clone(),
        OracleType::Inferred => "inferred".to_owned(),
    }
}

fn oracle_files(oracle: &ClangOracle) -> Vec<PathBuf> {
    oracle
        .namespaces
        .iter()
        .map(|value| value.source_file.clone())
        .chain(oracle.records.iter().map(|value| value.source_file.clone()))
        .chain(oracle.fields.iter().map(|value| value.source_file.clone()))
        .chain(
            oracle
                .functions
                .iter()
                .map(|value| value.source_file.clone()),
        )
        .chain(oracle.enums.iter().map(|value| value.source_file.clone()))
        .chain(
            oracle
                .variants
                .iter()
                .map(|value| value.source_file.clone()),
        )
        .chain(oracle.aliases.iter().map(|value| value.source_file.clone()))
        .chain(oracle.vars.iter().map(|value| value.source_file.clone()))
        .chain(
            oracle
                .references
                .iter()
                .map(|value| value.source_file.clone()),
        )
        .chain(
            oracle
                .diagnostics
                .iter()
                .map(|value| value.source_file.clone()),
        )
        .collect()
}
