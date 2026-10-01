//! Reading the lines your workspace uses a declaration on (W-Open I3), off
//! the UI thread: the read worker calls [`read`] with a page's references and
//! puts the lines on the page, so the page lands whole.
//!
//! Until `SymbolPage` carries them (`model/pages/symbol.rs` is W-Sym6's; the
//! field and its two call sites are in `.local/lanes/wave6/instant/I3.md`), the
//! symbol body reads them itself, on first draw, and shows a placeholder line
//! meanwhile. With the page carrying them there is no placeholder and no
//! second cache.

use crate::model::pages::{ReferenceSite, Resolution, UseLine};
use backend_library::SemanticConfidence;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The biggest file read for a line: a generated file is not your code.
const LARGEST_FILE: u64 = 4 * 1024 * 1024;

/// Where a file's text comes from (a test gives it).
pub(crate) trait Files {
    /// The text of `path`, or `None` when it cannot be read as text.
    fn text(&self, path: &Path) -> Option<Arc<str>>;
}

/// Your files, as they are on disk.
pub(crate) struct OnDisk;

impl Files for OnDisk {
    fn text(&self, path: &Path) -> Option<Arc<str>> {
        if std::fs::metadata(path).ok()?.len() > LARGEST_FILE {
            return None;
        }
        String::from_utf8(std::fs::read(path).ok()?)
            .ok()
            .map(Arc::from)
    }
}

/// The lines of the local uses among `references`, in the order the index
/// gave them. Each file is read once. A use of a registry package (not yours
/// to open), one with no span, and one whose file or line cannot be read, is
/// left out.
pub(crate) fn read(references: &[ReferenceSite], files: &dyn Files) -> Arc<[UseLine]> {
    let mut texts: HashMap<PathBuf, Option<Arc<str>>> = HashMap::new();
    let mut lines = Vec::new();
    for reference in references {
        let Some(span) = reference.span.known() else {
            continue;
        };
        let Some(package) = reference.site.coordinate.package() else {
            continue;
        };
        if !package.is_local() {
            continue;
        }
        let path = Path::new(package.as_str()).join(span.file.as_ref());
        let text = texts
            .entry(path.clone())
            .or_insert_with(|| files.text(&path))
            .clone();
        let Some(text) = text else { continue };
        let Some(found) = line_at(&text, span.bytes.start as usize, span.bytes.end as usize) else {
            continue;
        };
        lines.push(UseLine {
            package: Arc::from(package.display_name()),
            file: Arc::clone(&span.file),
            path: Arc::from(path.to_string_lossy().as_ref()),
            line: found.number,
            text: Arc::from(found.shown),
            mark: found.mark,
            relation: reference.relation,
            resolution: match reference.confidence {
                SemanticConfidence::Compiler
                | SemanticConfidence::Imported
                | SemanticConfidence::Indexed => Resolution::Resolved,
                SemanticConfidence::Syntactic | SemanticConfidence::Heuristic => Resolution::ByName,
            },
        });
    }
    Arc::from(lines)
}

/// The line a span is on.
struct Found {
    /// One-based.
    number: u32,
    /// Trimmed and cut at [`UseLine::MAX_TEXT`] characters.
    shown: String,
    /// The span in `shown`'s bytes, when it lies wholly inside what is shown.
    mark: Option<std::ops::Range<u32>>,
}

/// The line a byte span starts on (`None` past the end, inside a character,
/// or on a blank line).
fn line_at(text: &str, at: usize, end: usize) -> Option<Found> {
    if at > text.len() || !text.is_char_boundary(at) {
        return None;
    }
    let start = text[..at].rfind('\n').map_or(0, |i| i + 1);
    let stop = text[at..].find('\n').map_or(text.len(), |i| at + i);
    let number =
        u32::try_from(text[..start].bytes().filter(|byte| *byte == b'\n').count() + 1).ok()?;
    let line = &text[start..stop];
    let indent = line.len() - line.trim_start().len();
    let shown: String = line.trim().chars().take(UseLine::MAX_TEXT).collect();
    let mark = at
        .checked_sub(start + indent)
        .zip(end.min(stop).checked_sub(start + indent))
        .filter(|(from, to)| {
            from < to
                && *to <= shown.len()
                && shown.is_char_boundary(*from)
                && shown.is_char_boundary(*to)
        })
        .and_then(|(from, to)| Some(u32::try_from(from).ok()?..u32::try_from(to).ok()?));
    (!shown.is_empty()).then_some(Found {
        number,
        shown,
        mark,
    })
}

#[cfg(test)]
#[path = "workspace_lines_tests.rs"]
mod tests;
