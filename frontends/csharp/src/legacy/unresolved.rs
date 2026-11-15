//! The earliest reference Roslyn could not resolve, as named by the helper.
//!
//! The helper prints one `unresolved-reference` JSON line, computed over every
//! compiler error and ordered by package-relative path, line and column. This
//! module admits that line into a closed, bounded value: the compiler's text is
//! never carried further than the fixed code set, a restricted symbol alphabet
//! and package-relative paths.

use serde::Deserialize;

const MARKER: &str = "oracle: unresolved-reference: ";
const MAX_SYMBOL_BYTES: usize = 96;
const MAX_PATH_BYTES: usize = 256;

/// Roslyn errors that mean a type, namespace or assembly is unavailable.
const CODES: [u16; 6] = [6, 12, 234, 246, 400, 518];

/// Restore state of the project that needs the unresolved reference.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CSharpRestoreState {
    /// The project's restore result was read.
    Present,
    /// The project declares packages but has no usable restore result.
    Missing,
    /// No restore observation applies (not under a project, or none read).
    Unknown,
}

/// The first unresolved reference of a refused C# compilation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CSharpUnresolvedReference {
    /// Numeric part of the Roslyn code, for example `246` for `CS0246`.
    pub code: u16,
    /// Missing type, namespace or assembly name (restricted alphabet).
    pub symbol: String,
    /// Package-relative source file containing the reference.
    pub path: String,
    /// One-based line of the reference.
    pub line: u32,
    /// Package-relative project file that owns the source, when there is one.
    pub project: Option<String>,
    /// Whether that project's packages were restored.
    pub restore: CSharpRestoreState,
    /// Total unresolved-reference errors in the compilation.
    pub count: u32,
    /// Number of files containing them.
    pub files: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Line {
    code: String,
    symbol: String,
    path: String,
    line: u32,
    #[serde(default)]
    project: Option<String>,
    restore: String,
    count: u32,
    files: u32,
}

impl CSharpUnresolvedReference {
    /// Extracts the last well-formed summary from the helper's stderr.
    #[must_use]
    pub fn from_stderr(stderr: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(stderr);
        text.lines()
            .rev()
            .filter_map(|line| line.strip_prefix(MARKER))
            .find_map(Self::parse)
    }

    fn parse(json: &str) -> Option<Self> {
        let line: Line = serde_json::from_str(json).ok()?;
        let digits = line.code.strip_prefix("CS")?;
        if digits.len() != 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let code: u16 = digits.parse().ok()?;
        let restore = match line.restore.as_str() {
            "present" => CSharpRestoreState::Present,
            "missing" => CSharpRestoreState::Missing,
            "unknown" => CSharpRestoreState::Unknown,
            _ => return None,
        };
        let symbol_ok = !line.symbol.is_empty()
            && line.symbol.len() <= MAX_SYMBOL_BYTES
            && line.symbol.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '_' | '.' | '<' | '>' | ',' | '`' | '+' | '?')
            });
        let project_ok = line.project.as_deref().is_none_or(|project| {
            project.to_ascii_lowercase().ends_with(".csproj") && relative_path_ok(project)
        });
        (CODES.contains(&code)
            && symbol_ok
            && project_ok
            && line.line > 0
            && line.files > 0
            && line.count >= line.files
            && relative_path_ok(&line.path))
        .then_some(Self {
            code,
            symbol: line.symbol,
            path: line.path,
            line: line.line,
            project: line.project,
            restore,
            count: line.count,
            files: line.files,
        })
    }
}

fn relative_path_ok(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    const LINE: &str = r#"{"code":"CS0246","symbol":"DbContext","path":"Api/Data/AppDbContext.cs","line":15,"project":"Api/Api.csproj","restore":"present","count":23,"files":5}"#;

    #[test]
    fn the_last_well_formed_summary_is_admitted() {
        let stderr = format!(
            "oracle: noise\noracle: unresolved-reference: not json\n{MARKER}{LINE}\noracle: wrote x\n"
        );
        let found = CSharpUnresolvedReference::from_stderr(stderr.as_bytes()).expect("summary");
        assert_eq!(found.code, 246);
        assert_eq!(found.symbol, "DbContext");
        assert_eq!(found.path, "Api/Data/AppDbContext.cs");
        assert_eq!(found.project.as_deref(), Some("Api/Api.csproj"));
        assert_eq!(found.restore, CSharpRestoreState::Present);
        assert_eq!((found.line, found.count, found.files), (15, 23, 5));
    }

    #[test]
    fn hostile_summaries_are_refused() {
        for bad in [
            LINE.replace("CS0246", "CS0103"),
            LINE.replace("CS0246", "CS246"),
            LINE.replace("DbContext", "Db Context"),
            LINE.replace("DbContext", &"x".repeat(97)),
            LINE.replace("Api/Data/AppDbContext.cs", "../AppDbContext.cs"),
            LINE.replace("Api/Data/AppDbContext.cs", "/abs/AppDbContext.cs"),
            LINE.replace("Api/Api.csproj", "Api/Api.cs"),
            LINE.replace("present", "maybe"),
            LINE.replace("\"line\":15", "\"line\":0"),
            LINE.replace("\"files\":5", "\"files\":50"),
            LINE.replace("}", ",\"extra\":1}"),
        ] {
            let stderr = format!("{MARKER}{bad}\n");
            assert!(
                CSharpUnresolvedReference::from_stderr(stderr.as_bytes()).is_none(),
                "{bad}"
            );
        }
    }
}
