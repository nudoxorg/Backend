//! Immutable projection from admitted backend rows into desktop-friendly data.

use backend_library::{Fragment, Row, RowId, ViewRoot, encode_id};
use gpui::SharedString;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::package_metadata::PackageMetadata;

#[derive(Clone)]
pub(super) enum DocBlock {
    Prose(SharedString),
    Code(SharedString),
    Link(SharedString),
    Break,
}

#[derive(Clone)]
pub(super) struct DocRow {
    pub(super) key: SharedString,
    pub(super) id: RowId,
    pub(super) coordinate: SharedString,
    pub(super) title: SharedString,
    pub(super) kind: SharedString,
    pub(super) language: SharedString,
    pub(super) signature: SharedString,
    pub(super) blocks: Arc<[DocBlock]>,
    pub(super) parent: Option<String>,
    pub(super) source: Option<SourceLink>,
}

#[derive(Clone)]
pub(super) struct SourceLink {
    pub(super) path: PathBuf,
    pub(super) display_path: SharedString,
    pub(super) line: u32,
}

#[derive(Clone)]
pub(super) struct SourcePreview {
    pub(super) path: SharedString,
    pub(super) lines: Arc<[SharedString]>,
    pub(super) line: u32,
}

impl DocRow {
    fn from_row(row: &Row, project: &Path) -> Self {
        let coordinate = row.label.clone();
        let title = coordinate
            .rsplit("::")
            .next()
            .unwrap_or(&coordinate)
            .rsplit(':')
            .next()
            .unwrap_or(&coordinate)
            .to_owned();
        let language = language_for(&coordinate).to_owned();
        let signature = row.signature.clone().unwrap_or_default();
        let kind = row
            .kind
            .map_or_else(|| "Symbol".to_owned(), |kind| display_kind(kind.name()));
        let blocks = row
            .document
            .iter()
            .map(|fragment| match fragment {
                Fragment::Text(text) => DocBlock::Prose(text.clone().into()),
                Fragment::Code(code) => DocBlock::Code(code.clone().into()),
                Fragment::Link { label, .. } => DocBlock::Link(label.clone().into()),
                Fragment::Break => DocBlock::Break,
            })
            .collect::<Vec<_>>()
            .into();
        let source_coordinate = row.source.captured().map_or_else(String::new, |location| {
            format!("{}:{}", location.path(), location.start_line())
        });
        let key: SharedString = format!(
            "{}\0{}\0{}\0{}\0{}",
            row.id.stable_key(),
            coordinate,
            kind,
            signature,
            source_coordinate
        )
        .into();
        Self {
            key,
            id: row.id,
            coordinate: coordinate.into(),
            title: title.into(),
            kind: kind.into(),
            language: language.into(),
            signature: signature.into(),
            blocks,
            parent: row.parent.map(|key| encode_id(key.as_bytes())),
            source: row.source.captured().and_then(|location| {
                let indexed_path = Path::new(location.path());
                let display_path = indexed_path
                    .strip_prefix(project)
                    .unwrap_or(indexed_path)
                    .to_string_lossy()
                    .into_owned();
                let path = if indexed_path.is_absolute() {
                    indexed_path.to_path_buf()
                } else {
                    project.join(indexed_path)
                };
                path.is_file().then_some(SourceLink {
                    path,
                    display_path: display_path.into(),
                    line: location.start_line(),
                })
            }),
        }
    }
}

pub(super) struct Catalog {
    pub(super) rows: Arc<[DocRow]>,
    pub(super) by_key: BTreeMap<SharedString, usize>,
    by_id: BTreeMap<RowId, usize>,
    pub(super) order: Vec<usize>,
    pub(super) languages: BTreeMap<SharedString, usize>,
    pub(super) files: BTreeSet<String>,
    pub(super) source_lines: usize,
    pub(super) source_bytes: u64,
    pub(super) package_name: SharedString,
    pub(super) package: PackageMetadata,
}

impl Catalog {
    pub(super) fn from_root(root: &ViewRoot, project: &Path) -> Self {
        let rows: Arc<[DocRow]> = root
            .rows()
            .iter()
            .map(|row| DocRow::from_row(row, project))
            .collect::<Vec<_>>()
            .into();
        let by_key = rows
            .iter()
            .enumerate()
            .map(|(at, row)| (row.key.clone(), at))
            .collect();
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(at, row)| (row.id, at))
            .collect();
        let packages: BTreeMap<String, SharedString> = root
            .rows()
            .iter()
            .filter_map(|row| match row.id {
                RowId::Package(key) => Some((encode_id(key.as_bytes()), row.label.clone().into())),
                _ => None,
            })
            .collect();
        let package = PackageMetadata::load(project);
        let package_name = if package.name.is_empty() {
            packages
                .values()
                .next()
                .map_or_else(|| "workspace".into(), |path| package_name(path))
        } else {
            package.name.clone().into()
        };
        let mut languages = BTreeMap::new();
        let mut files = BTreeSet::new();
        for row in rows.iter().filter(|row| matches!(row.id, RowId::Symbol(_))) {
            *languages.entry(row.language.clone()).or_insert(0) += 1;
            if let Some(source) = &row.source {
                files.insert(source.display_path.to_string());
            }
        }
        let (source_lines, source_bytes) = files.iter().fold((0usize, 0u64), |totals, path| {
            let path = project.join(path);
            let bytes = std::fs::read(&path).unwrap_or_default();
            (
                totals.0.saturating_add(
                    bytes.iter().filter(|byte| **byte == b'\n').count()
                        + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n")),
                ),
                totals.1.saturating_add(bytes.len() as u64),
            )
        });
        let mut order = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| matches!(row.id, RowId::Symbol(_)).then_some(index))
            .collect::<Vec<_>>();
        order.sort_unstable_by(|left, right| {
            kind_rank(&rows[*left].kind)
                .cmp(&kind_rank(&rows[*right].kind))
                .then_with(|| rows[*left].coordinate.cmp(&rows[*right].coordinate))
                .then_with(|| rows[*left].signature.cmp(&rows[*right].signature))
                .then_with(|| {
                    rows[*left]
                        .source
                        .as_ref()
                        .map(|source| (&source.display_path, source.line))
                        .cmp(
                            &rows[*right]
                                .source
                                .as_ref()
                                .map(|source| (&source.display_path, source.line)),
                        )
                })
                .then_with(|| rows[*left].key.cmp(&rows[*right].key))
        });
        order.dedup_by(|left, right| {
            let left = &rows[*left];
            let right = &rows[*right];
            left.coordinate == right.coordinate
                && left.kind == right.kind
                && left.signature == right.signature
                && left
                    .source
                    .as_ref()
                    .map(|source| (&source.display_path, source.line))
                    == right
                        .source
                        .as_ref()
                        .map(|source| (&source.display_path, source.line))
        });
        Self {
            rows,
            by_key,
            by_id,
            order,
            languages,
            files,
            source_lines,
            source_bytes,
            package_name,
            package,
        }
    }

    pub(super) fn get(&self, key: &str) -> Option<&DocRow> {
        self.by_key.get(key).and_then(|index| self.rows.get(*index))
    }

    /// Projects library-ranked result identities into the immutable native catalog.
    pub(super) fn indices_for_ids(&self, ids: &[RowId]) -> Vec<usize> {
        ids.iter()
            .filter(|id| matches!(id, RowId::Symbol(_)))
            .filter_map(|id| self.by_id.get(id).copied())
            .collect()
    }

    pub(super) fn symbol_count(&self) -> usize {
        self.order.len()
    }
}

fn package_name(path: &str) -> SharedString {
    path.trim_end_matches('/')
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("workspace")
        .to_owned()
        .into()
}

pub(super) fn language_for(coordinate: &str) -> &'static str {
    let path = coordinate
        .split("::")
        .nth(1)
        .unwrap_or(coordinate)
        .split(':')
        .next()
        .unwrap_or(coordinate);
    match path.rsplit('.').next().unwrap_or_default() {
        "rs" => "Rust",
        "py" => "Python",
        "ts" | "tsx" | "js" | "jsx" | "mts" | "cts" => "TypeScript",
        "go" => "Go",
        "java" => "Java",
        "cs" => "C#",
        "c" | "h" | "cc" | "cpp" | "cxx" | "hpp" => "C / C++",
        _ => "Other",
    }
}

fn display_kind(kind: &str) -> String {
    let mut chars = kind.chars();
    chars.next().map_or_else(
        || "Symbol".to_owned(),
        |first| first.to_ascii_uppercase().to_string() + chars.as_str(),
    )
}

fn kind_rank(kind: &str) -> usize {
    match kind {
        "Module" => 0,
        "Struct" => 1,
        "Class" => 2,
        "Enum" => 3,
        "Trait" | "Interface" => 4,
        "Function" | "Method" => 5,
        "Constant" => 6,
        _ => 7,
    }
}
