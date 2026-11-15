//! The first unresolved reference of a refused C# compilation, without compiler text.
//!
//! Roslyn refuses a project whose types, namespaces or assemblies it cannot bind
//! rather than publishing a partial answer. These facts say which reference was
//! the first one missing, in a closed shape: a fixed code set, a restricted symbol
//! alphabet and project-relative paths. No compiler message and no absolute path
//! ever reaches them.

use serde::{Deserialize, Deserializer, Serialize};

const MAX_SYMBOL_BYTES: usize = 96;
const MAX_PATH_BYTES: usize = 256;

/// Roslyn numeric codes that mean a type, namespace or assembly is unavailable:
/// CS0006, CS0012, CS0234, CS0246, CS0400 and CS0518.
const UNRESOLVED_CODES: [u16; 6] = [6, 12, 234, 246, 400, 518];

/// Why the configured `dotnet` file cannot run the compiler stage.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CSharpHostFormatFailure {
    /// The file is an interpreter script that launches the real host (a package manager's
    /// wrapper), not an executable the compiler stage can hold in private custody.
    LauncherScript,
}

impl CSharpHostFormatFailure {
    /// One-line public detail.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::LauncherScript => {
                "C# compiler stage refused: the configured dotnet is a launcher script, not an executable the compiler can hold in private custody"
            }
        }
    }

    /// What the owner of the configuration can do about it.
    #[must_use]
    pub const fn advice(self) -> &'static str {
        match self {
            Self::LauncherScript => {
                "point NUDOX_DOTNET at the real dotnet executable that the launcher runs (not the launcher), then retry"
            }
        }
    }

    /// Stable tag for the failure.
    #[must_use]
    pub const fn kind_tag(self) -> &'static str {
        match self {
            Self::LauncherScript => "csharp_host_launcher_script",
        }
    }
}

/// What the restore result of the project owning an unresolved reference says.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CSharpRestoreState {
    /// The project's restore result was read.
    Present,
    /// The project declares packages but has no usable restore result.
    Missing,
    /// No restore observation applies (no owning project, or none was read).
    Unknown,
}

/// The earliest unresolved reference over every error of a refused compilation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CSharpUnresolvedReferenceFacts {
    code: u16,
    symbol: Box<str>,
    path: Box<str>,
    line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<Box<str>>,
    restore: CSharpRestoreState,
    count: u32,
    files: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    code: u16,
    symbol: String,
    path: String,
    line: u32,
    #[serde(default)]
    project: Option<String>,
    restore: CSharpRestoreState,
    count: u32,
    files: u32,
}

/// Candidate fields of an unresolved reference, before admission.
#[derive(Clone, Copy, Debug)]
pub struct CSharpUnresolvedReferenceInput<'a> {
    /// Numeric part of the Roslyn code, for example `246` for `CS0246`.
    pub code: u16,
    /// Missing type, namespace or assembly name.
    pub symbol: &'a str,
    /// Project-relative file containing the reference.
    pub path: &'a str,
    /// One-based line of the reference.
    pub line: u32,
    /// Project-relative `.csproj` that owns the source, when there is one.
    pub project: Option<&'a str>,
    /// Restore state of that project.
    pub restore: CSharpRestoreState,
    /// Total unresolved-reference errors in the compilation.
    pub count: u32,
    /// Number of files containing them.
    pub files: u32,
}

impl CSharpUnresolvedReferenceFacts {
    /// Admits one reference exactly; `None` when any field is outside its closed shape.
    #[must_use]
    pub fn new(input: CSharpUnresolvedReferenceInput<'_>) -> Option<Self> {
        let CSharpUnresolvedReferenceInput {
            code,
            symbol,
            path,
            line,
            project,
            restore,
            count,
            files,
        } = input;
        let symbol_ok = !symbol.is_empty()
            && symbol.len() <= MAX_SYMBOL_BYTES
            && symbol.chars().all(|character| {
                character.is_ascii_alphanumeric()
                    || matches!(character, '_' | '.' | '<' | '>' | ',' | '`' | '+' | '?')
            });
        let project_ok = project.is_none_or(|project| {
            project.to_ascii_lowercase().ends_with(".csproj") && relative_path_ok(project)
        });
        (UNRESOLVED_CODES.contains(&code)
            && symbol_ok
            && project_ok
            && line > 0
            && files > 0
            && count >= files
            && relative_path_ok(path))
        .then(|| Self {
            code,
            symbol: symbol.into(),
            path: path.into(),
            line,
            project: project.map(Into::into),
            restore,
            count,
            files,
        })
    }

    /// Numeric part of the Roslyn code, for example `246` for `CS0246`.
    #[must_use]
    pub const fn code(&self) -> u16 {
        self.code
    }

    /// Missing type, namespace or assembly name.
    #[must_use]
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Project-relative file containing the reference.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// One-based line of the reference.
    #[must_use]
    pub const fn line(&self) -> u32 {
        self.line
    }

    /// Project-relative project file that owns the source, when there is one.
    #[must_use]
    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    /// Restore state of that project.
    #[must_use]
    pub const fn restore(&self) -> CSharpRestoreState {
        self.restore
    }

    /// Total unresolved-reference errors in the compilation.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// Number of files containing them.
    #[must_use]
    pub const fn files(&self) -> u32 {
        self.files
    }

    /// Stable tag for the Roslyn code, for example `CS0246`.
    #[must_use]
    pub fn code_text(&self) -> String {
        format!("CS{:04}", self.code)
    }

    /// One-line public detail naming the reference, its place and its restore state.
    #[must_use]
    pub fn detail(&self) -> String {
        let place = match (self.project(), self.restore) {
            (Some(project), CSharpRestoreState::Present) => {
                format!("project {project}, which was restored")
            }
            (Some(project), CSharpRestoreState::Missing) => {
                format!("project {project}, which has no usable restore result")
            }
            (Some(project), CSharpRestoreState::Unknown) => {
                format!("project {project}, whose restore state is unknown")
            }
            (None, _) => "no project file".to_owned(),
        };
        let others = self.count.saturating_sub(1);
        let tail = if others == 0 {
            String::new()
        } else {
            format!(
                "; {others} more unresolved reference{} in {} file{}",
                if others == 1 { "" } else { "s" },
                self.files,
                if self.files == 1 { "" } else { "s" },
            )
        };
        format!(
            "C# compilation refused: first unresolved reference {} {} at {}:{} in {place}{tail}",
            self.code_text(),
            self.symbol,
            self.path,
            self.line,
        )
    }

    /// What the owner of the project can do about it.
    #[must_use]
    pub const fn advice(&self) -> &'static str {
        match self.restore {
            CSharpRestoreState::Missing => {
                "run dotnet restore for that project on this machine, then retry"
            }
            CSharpRestoreState::Present => {
                "the restore result does not supply it: add the package or project reference that declares it, or restore again on this machine, then retry"
            }
            CSharpRestoreState::Unknown => {
                "make sure the project or package that declares it is part of the project and restored, then retry"
            }
        }
    }
}

impl<'de> Deserialize<'de> for CSharpUnresolvedReferenceFacts {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = Wire::deserialize(deserializer)?;
        Self::new(CSharpUnresolvedReferenceInput {
            code: wire.code,
            symbol: &wire.symbol,
            path: &wire.path,
            line: wire.line,
            project: wire.project.as_deref(),
            restore: wire.restore,
            count: wire.count,
            files: wire.files,
        })
        .ok_or_else(|| serde::de::Error::custom("invalid C# unresolved-reference facts"))
    }
}

fn relative_path_ok(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= MAX_PATH_BYTES
        && !path.starts_with('/')
        && !path.contains(['\\', '"'])
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_script_failure_says_what_to_configure() {
        let failure = CSharpHostFormatFailure::LauncherScript;
        assert!(failure.detail().contains("launcher script"));
        assert!(failure.advice().contains("NUDOX_DOTNET"));
        assert_eq!(failure.kind_tag(), "csharp_host_launcher_script");
        assert_eq!(
            serde_json::to_string(&failure).unwrap(),
            "\"launcher_script\""
        );
        assert!(serde_json::from_str::<CSharpHostFormatFailure>("\"elf\"").is_err());
    }

    fn facts(restore: CSharpRestoreState) -> CSharpUnresolvedReferenceFacts {
        CSharpUnresolvedReferenceFacts::new(CSharpUnresolvedReferenceInput {
            code: 246,
            symbol: "DbContext",
            path: "src/Api/Data/AppDbContext.cs",
            line: 15,
            project: Some("src/Api/Api.csproj"),
            restore,
            count: 23,
            files: 5,
        })
        .expect("admitted")
    }

    #[test]
    fn detail_names_code_symbol_place_project_and_restore() {
        let detail = facts(CSharpRestoreState::Present).detail();
        assert_eq!(
            detail,
            "C# compilation refused: first unresolved reference CS0246 DbContext at \
             src/Api/Data/AppDbContext.cs:15 in project src/Api/Api.csproj, which was restored; \
             22 more unresolved references in 5 files"
        );
        assert!(
            facts(CSharpRestoreState::Missing)
                .detail()
                .contains("which has no usable restore result")
        );
        assert!(
            facts(CSharpRestoreState::Unknown)
                .detail()
                .contains("whose restore state is unknown")
        );
        let alone = CSharpUnresolvedReferenceFacts::new(CSharpUnresolvedReferenceInput {
            code: 234,
            symbol: "Microsoft.AspNetCore",
            path: "Tests.cs",
            line: 2,
            project: None,
            restore: CSharpRestoreState::Unknown,
            count: 1,
            files: 1,
        })
        .expect("admitted");
        assert_eq!(
            alone.detail(),
            "C# compilation refused: first unresolved reference CS0234 Microsoft.AspNetCore at \
             Tests.cs:2 in no project file"
        );
    }

    #[test]
    fn advice_depends_on_the_restore_state() {
        assert!(
            facts(CSharpRestoreState::Missing)
                .advice()
                .contains("dotnet restore")
        );
        assert!(
            facts(CSharpRestoreState::Present)
                .advice()
                .contains("does not supply it")
        );
    }

    #[test]
    fn hostile_facts_are_refused_at_construction_and_on_the_wire() {
        let ok = |symbol: &str, path: &str, project: Option<&str>, line, count, files| {
            CSharpUnresolvedReferenceFacts::new(CSharpUnresolvedReferenceInput {
                code: 246,
                symbol,
                path,
                line,
                project,
                restore: CSharpRestoreState::Present,
                count,
                files,
            })
        };
        assert!(ok("DbContext", "a/b.cs", Some("a/a.csproj"), 1, 1, 1).is_some());
        assert!(ok("Db Context", "a/b.cs", None, 1, 1, 1).is_none());
        assert!(ok("D\"b", "a/b.cs", None, 1, 1, 1).is_none());
        assert!(ok(&"x".repeat(97), "a/b.cs", None, 1, 1, 1).is_none());
        assert!(ok("T", "../b.cs", None, 1, 1, 1).is_none());
        assert!(ok("T", "/abs/b.cs", None, 1, 1, 1).is_none());
        assert!(ok("T", "a\\b.cs", None, 1, 1, 1).is_none());
        assert!(ok("T", "a/b.cs", Some("a/a.cs"), 1, 1, 1).is_none());
        assert!(ok("T", "a/b.cs", None, 0, 1, 1).is_none());
        assert!(ok("T", "a/b.cs", None, 1, 1, 2).is_none());
        assert!(
            CSharpUnresolvedReferenceFacts::new(CSharpUnresolvedReferenceInput {
                code: 103,
                symbol: "T",
                path: "a/b.cs",
                line: 1,
                project: None,
                restore: CSharpRestoreState::Unknown,
                count: 1,
                files: 1,
            })
            .is_none()
        );

        let good = serde_json::to_string(&facts(CSharpRestoreState::Present)).expect("encode");
        let reopened: CSharpUnresolvedReferenceFacts =
            serde_json::from_str(&good).expect("round trip");
        assert_eq!(reopened, facts(CSharpRestoreState::Present));
        for bad in [
            good.replace("\"code\":246", "\"code\":103"),
            good.replace("DbContext", "Db Context"),
            good.replace("\"line\":15", "\"line\":0"),
            good.replace("}", ",\"extra\":1}"),
            good.replace("present", "maybe"),
        ] {
            assert!(
                serde_json::from_str::<CSharpUnresolvedReferenceFacts>(&bad).is_err(),
                "{bad}"
            );
        }
    }
}
