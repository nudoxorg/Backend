//! One bounded, deterministic filesystem discovery policy.
//!
//! Source ingestion and package/archive graph construction must agree about
//! which tree they are looking at. Keeping that decision in one small crate
//! prevents a frontend-specific walker, an archive staging walker, and a
//! package graph walker from growing subtly different ignore rules. The
//! policy delegates Git pattern semantics to the same matcher used by Git
//! aware search tools, while keeping product-owned generated directories hard
//! ignored regardless of a negated user pattern.

use ignore::{DirEntry, Walk, WalkBuilder, overrides::OverrideBuilder};
use std::{
    ffi::OsStr,
    fmt,
    path::{Path, PathBuf},
};

/// Directory names treated as generated or tool-owned state by default.
pub const DEFAULT_IGNORED_DIRECTORIES: &[&str] = &[
    ".git",
    ".backend",
    "target",
    "bin",
    "obj",
    "out",
    ".gradle",
    "node_modules",
    "dist",
    "build",
    ".angular",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".cache",
    ".vite",
    ".parcel-cache",
    ".webpack",
    ".rollup.cache",
    ".nx",
    "coverage",
    "storybook-static",
    ".nyc_output",
    "Library",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    ".ruff_cache",
    ".idea",
    ".vscode",
    ".vs",
    ".yarn",
    ".pnpm-store",
    "bower_components",
    "jspm_packages",
    "vendor",
];

/// Compatibility alias for callers that used the original name.
pub const HARD_IGNORED_DIRECTORIES: &[&str] = DEFAULT_IGNORED_DIRECTORIES;

/// Returns whether a path component is one of Nudox's hard generated roots.
#[must_use]
pub fn is_hard_ignored_directory(name: &OsStr) -> bool {
    DEFAULT_IGNORED_DIRECTORIES
        .iter()
        .any(|candidate| OsStr::new(candidate) == name)
}

/// Returns whether any directory component in a relative archive/source path
/// is a hard generated root.
#[must_use]
pub fn is_hard_ignored_path(path: &Path) -> bool {
    path.parent().is_some_and(|parent| {
        parent
            .components()
            .any(|component| is_hard_ignored_directory(component.as_os_str()))
    })
}

/// The kind of one admitted filesystem entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// A directory that may contain more entries.
    Directory,
    /// A regular file.
    File,
    /// A symbolic link, always excluded from project admission.
    Symlink,
    /// A special filesystem node, always excluded from project admission.
    Other,
}

/// One owned, deterministic discovery result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveredEntry {
    path: PathBuf,
    kind: EntryKind,
}

impl DiscoveredEntry {
    fn from_dir_entry(entry: DirEntry) -> Result<Self, DiscoveryError> {
        let is_symlink = entry.path_is_symlink();
        let file_type = entry.file_type();
        let path = entry.into_path();
        let kind = if is_symlink {
            EntryKind::Symlink
        } else {
            file_type
                .map(|file_type| {
                    if file_type.is_dir() {
                        EntryKind::Directory
                    } else if file_type.is_file() {
                        EntryKind::File
                    } else {
                        EntryKind::Other
                    }
                })
                .ok_or_else(|| DiscoveryError::Io {
                    path: path.clone(),
                    detail: "filesystem entry has no file type".to_owned(),
                })?
        };
        Ok(Self { path, kind })
    }

    /// Returns the path supplied to the walker.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns this entry's filesystem kind.
    #[must_use]
    pub const fn kind(&self) -> EntryKind {
        self.kind
    }

    /// Returns whether this entry is a regular file.
    #[must_use]
    pub const fn is_file(&self) -> bool {
        matches!(self.kind, EntryKind::File)
    }

    /// Returns whether this entry is a directory.
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        matches!(self.kind, EntryKind::Directory)
    }

    /// Returns whether this entry is a symbolic link.
    #[must_use]
    pub const fn is_symlink(&self) -> bool {
        matches!(self.kind, EntryKind::Symlink)
    }
}

/// A traversal or ignore-file diagnostic that prevents a trustworthy scan.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryError {
    /// The filesystem walker could not inspect a path or parse an ignore file.
    Io {
        /// Path at which traversal reported the failure.
        path: PathBuf,
        /// Bounded human-readable diagnostic from the walker.
        detail: String,
    },
    /// A caller supplied override pattern was malformed.
    Pattern {
        /// Pattern that could not be compiled.
        pattern: String,
        /// Bounded human-readable compiler diagnostic.
        detail: String,
    },
}

impl fmt::Display for DiscoveryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, detail } => {
                write!(formatter, "discover {}: {detail}", path.display())
            }
            Self::Pattern { pattern, detail } => {
                write!(formatter, "discover override `{pattern}`: {detail}")
            }
        }
    }
}

impl std::error::Error for DiscoveryError {}

/// A reusable product discovery policy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryPolicy {
    generated_defaults: bool,
    respect_gitignore: bool,
    case_insensitive: bool,
    includes: Vec<String>,
    excludes: Vec<String>,
}

impl Default for DiscoveryPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl DiscoveryPolicy {
    /// Creates the production policy for a local source tree.
    #[must_use]
    pub fn new() -> Self {
        Self {
            generated_defaults: true,
            respect_gitignore: true,
            case_insensitive: cfg!(windows),
            includes: Vec::new(),
            excludes: Vec::new(),
        }
    }

    /// Enables or disables generated/dependency directory defaults.
    #[must_use]
    pub fn generated_defaults(mut self, enabled: bool) -> Self {
        self.generated_defaults = enabled;
        self
    }

    /// Enables or disables repository and global Git ignore files.
    #[must_use]
    pub fn respect_gitignore(mut self, enabled: bool) -> Self {
        self.respect_gitignore = enabled;
        self
    }

    /// Selects host case behavior for repository rules and overrides.
    #[must_use]
    pub fn case_insensitive(mut self, enabled: bool) -> Self {
        self.case_insensitive = enabled;
        self
    }

    /// Reopens a path matched by a repository ignore or generated default.
    #[must_use]
    pub fn include(mut self, pattern: impl Into<String>) -> Self {
        self.includes.push(pattern.into());
        self
    }

    /// Excludes a path after all repository and include rules have run.
    #[must_use]
    pub fn exclude(mut self, pattern: impl Into<String>) -> Self {
        self.excludes.push(pattern.into());
        self
    }

    fn overrides(&self, root: &Path) -> Result<ignore::overrides::Override, DiscoveryError> {
        let mut builder = OverrideBuilder::new(root);
        if self.case_insensitive {
            builder
                .case_insensitive(true)
                .map_err(|error| DiscoveryError::Pattern {
                    pattern: "<case-insensitive>".to_owned(),
                    detail: error.to_string(),
                })?;
        }
        // Generated directories are pruned by `filter_entry`, before the
        // walker can descend into them. Keeping those defaults out of the
        // override matcher matters for explicit includes: an ignore override
        // on `.next/**` would also block the `.next` ancestor of a requested
        // `.next/server/app.ts` path.
        if !self.includes.is_empty() {
            // An override whitelist turns every unmatched file into an
            // implicit ignore. Start with a broad whitelist so an explicit
            // include reopens only the requested generated path while normal
            // source files remain selected.
            builder.add("**").map_err(|error| DiscoveryError::Pattern {
                pattern: "**".to_owned(),
                detail: error.to_string(),
            })?;
        }
        for pattern in &self.includes {
            let normalized = normalize_pattern(pattern);
            builder
                .add(&normalized)
                .map_err(|error| DiscoveryError::Pattern {
                    pattern: normalized,
                    detail: error.to_string(),
                })?;
        }
        for pattern in &self.excludes {
            let pattern = normalize_pattern(pattern);
            let normalized = if pattern.starts_with('!') {
                pattern
            } else {
                format!("!{pattern}")
            };
            builder
                .add(&normalized)
                .map_err(|error| DiscoveryError::Pattern {
                    pattern: normalized,
                    detail: error.to_string(),
                })?;
        }
        builder.build().map_err(|error| DiscoveryError::Pattern {
            pattern: "<policy>".to_owned(),
            detail: error.to_string(),
        })
    }

    fn include_overrides(
        &self,
        root: &Path,
    ) -> Result<Option<ignore::overrides::Override>, DiscoveryError> {
        if self.includes.is_empty() {
            return Ok(None);
        }
        let mut builder = OverrideBuilder::new(root);
        if self.case_insensitive {
            builder
                .case_insensitive(true)
                .map_err(|error| DiscoveryError::Pattern {
                    pattern: "<case-insensitive>".to_owned(),
                    detail: error.to_string(),
                })?;
        }
        for pattern in &self.includes {
            let normalized = normalize_pattern(pattern);
            builder
                .add(&normalized)
                .map_err(|error| DiscoveryError::Pattern {
                    pattern: normalized,
                    detail: error.to_string(),
                })?;
        }
        builder
            .build()
            .map(Some)
            .map_err(|error| DiscoveryError::Pattern {
                pattern: "<includes>".to_owned(),
                detail: error.to_string(),
            })
    }

    fn generated_component(&self, component: &OsStr) -> bool {
        self.generated_defaults
            && DEFAULT_IGNORED_DIRECTORIES.iter().any(|candidate| {
                if self.case_insensitive {
                    candidate.eq_ignore_ascii_case(&component.to_string_lossy())
                } else {
                    OsStr::new(candidate) == component
                }
            })
    }

    fn include_reopens(&self, root: &Path, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        let relative = slash_path(relative);
        self.includes.iter().any(|pattern| {
            let pattern = normalize_pattern(pattern);
            let pattern = pattern.trim_start_matches('!').trim_start_matches('/');
            if pattern == "**" || pattern.is_empty() {
                return true;
            }
            let prefix = pattern
                .find(['*', '?', '['])
                .map_or(pattern, |index| &pattern[..index])
                .trim_end_matches('/')
                .to_owned();
            let (prefix, relative) = if self.case_insensitive {
                (prefix.to_ascii_lowercase(), relative.to_ascii_lowercase())
            } else {
                (prefix, relative.clone())
            };
            prefix == relative
                || prefix.starts_with(&format!("{relative}/"))
                || relative.starts_with(&format!("{prefix}/"))
        })
    }

    fn include_reopens_directory_contents(&self, root: &Path, path: &Path) -> bool {
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        let relative = slash_path(relative);
        self.includes.iter().any(|pattern| {
            let pattern = normalize_pattern(pattern)
                .trim_start_matches('!')
                .trim_start_matches('/')
                .trim_end_matches('/')
                .to_owned();
            // A literal directory include is intentionally recursive. Glob
            // patterns are left to the ignore override matcher below so a
            // prefix such as `dist/foo*` cannot accidentally admit siblings.
            !pattern.is_empty()
                && !pattern.contains(['*', '?', '['])
                && (relative == pattern || relative.starts_with(&format!("{pattern}/")))
        })
    }

    fn admits_path(
        &self,
        root: &Path,
        path: &Path,
        include_overrides: Option<&ignore::overrides::Override>,
    ) -> bool {
        let Ok(relative) = path.strip_prefix(root) else {
            return false;
        };
        let generated = relative
            .components()
            .any(|component| self.generated_component(component.as_os_str()));
        if !generated {
            return true;
        }
        if path.is_dir() {
            return self.include_reopens(root, path);
        }
        include_overrides.is_some_and(|overrides| overrides.matched(path, false).is_whitelist())
            || self.include_reopens_directory_contents(root, path)
    }
}

impl DiscoveryPolicy {
    /// Creates a deterministic project/archive discovery iterator.
    ///
    /// `.gitignore`, nested ignore files, negations, anchored patterns,
    /// escaped pattern characters, `.git/info/exclude`, and configured global
    /// Git excludes are interpreted by `ignore`. `require_git` is disabled
    /// because a forge export or an uninitialised local project can still
    /// contain a useful `.gitignore`. No `git` subprocess is spawned. Symlinks
    /// are never followed, and hard generated directories are filtered before
    /// the matcher can descend into them. Explicit include patterns may
    /// reopen a generated default when the caller asks for that path.
    #[must_use]
    pub fn walk(self, root: impl AsRef<Path>) -> Discovery {
        let root = root.as_ref().to_owned();
        let mut builder = WalkBuilder::new(&root);
        let policy = self.clone();
        let overrides = self.overrides(&root);
        let include_overrides = self.include_overrides(&root);
        let initial_error = match (&overrides, &include_overrides) {
            (Err(error), _) | (_, Err(error)) => Some(error.clone()),
            (Ok(_), Ok(_)) => None,
        };
        if let Ok(overrides) = overrides {
            builder.overrides(overrides);
        }
        let include_overrides = include_overrides.ok().flatten();
        let root_for_filter = root.clone();
        builder
            .hidden(false)
            .parents(self.respect_gitignore)
            .ignore(self.respect_gitignore)
            .git_ignore(self.respect_gitignore)
            .git_global(self.respect_gitignore)
            .git_exclude(self.respect_gitignore)
            .require_git(false)
            .ignore_case_insensitive(self.case_insensitive)
            .follow_links(false)
            .threads(1)
            .sort_by_file_path(|left, right| left.cmp(right))
            .current_dir(root.clone());
        builder.filter_entry(move |entry| {
            entry.path() == root_for_filter
                || (!entry.path_is_symlink()
                    && policy.admits_path(
                        &root_for_filter,
                        entry.path(),
                        include_overrides.as_ref(),
                    ))
        });
        Discovery {
            root,
            inner: builder.build(),
            initial_error,
        }
    }
}

/// An owned iterator over the selected tree.
pub struct Discovery {
    root: PathBuf,
    inner: Walk,
    initial_error: Option<DiscoveryError>,
}

impl fmt::Debug for Discovery {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Discovery")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl Iterator for Discovery {
    type Item = Result<DiscoveredEntry, DiscoveryError>;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(error) = self.initial_error.take() {
            return Some(Err(error));
        }
        loop {
            let result = self.inner.next()?;
            match result {
                Ok(entry) if entry.path() == self.root => {
                    if let Some(error) = entry.error() {
                        return Some(Err(DiscoveryError::Io {
                            path: self.root.clone(),
                            detail: error.to_string(),
                        }));
                    }
                    continue;
                }
                Ok(entry) => {
                    return Some(if let Some(error) = entry.error() {
                        Err(DiscoveryError::Io {
                            path: entry.path().to_owned(),
                            detail: error.to_string(),
                        })
                    } else {
                        DiscoveredEntry::from_dir_entry(entry)
                    });
                }
                Err(error) => {
                    return Some(Err(DiscoveryError::Io {
                        path: self.root.clone(),
                        detail: error.to_string(),
                    }));
                }
            }
        }
    }
}

fn normalize_pattern(pattern: &str) -> String {
    pattern.replace('\\', "/")
}

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let suffix = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let path = std::env::temp_dir().join(format!("nudox-discovery-{label}-{suffix}"));
            fs::create_dir_all(&path).expect("scratch");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn hard_defaults_are_unreincludeable() {
        let scratch = Scratch::new("hard");
        fs::create_dir_all(scratch.0.join(".next")).expect("next");
        fs::write(scratch.0.join(".next/app.js"), b"generated").expect("generated");
        fs::write(scratch.0.join(".gitignore"), b"!.next/app.js\n").expect("ignore");
        fs::write(scratch.0.join("z.rs"), b"z").expect("z");
        fs::write(scratch.0.join("a.rs"), b"a").expect("a");
        let paths = DiscoveryPolicy::default()
            .walk(&scratch.0)
            .map(|entry| {
                entry
                    .expect("discovery")
                    .path()
                    .strip_prefix(&scratch.0)
                    .expect("relative")
                    .to_owned()
            })
            .filter(|path| !path.as_os_str().is_empty())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                PathBuf::from(".gitignore"),
                PathBuf::from("a.rs"),
                PathBuf::from("z.rs")
            ]
        );
    }

    #[test]
    fn explicit_include_reopens_a_generated_directory() {
        let scratch = Scratch::new("override");
        fs::create_dir_all(scratch.0.join(".next/server")).expect("next");
        fs::write(scratch.0.join(".next/server/app.ts"), b"app").expect("app");
        fs::write(scratch.0.join(".next/server/drop.ts"), b"drop").expect("drop");
        fs::create_dir_all(scratch.0.join("src")).expect("src");
        fs::write(scratch.0.join("src/live.ts"), b"live").expect("live");
        let paths = DiscoveryPolicy::default()
            .include(".next/server/app.ts")
            .walk(&scratch.0)
            .map(|entry| {
                entry
                    .expect("discovery")
                    .path()
                    .strip_prefix(&scratch.0)
                    .expect("relative")
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|path| path.ends_with(".ts"))
            .collect::<Vec<_>>();
        assert_eq!(paths, [".next/server/app.ts", "src/live.ts"]);
    }

    #[test]
    fn an_explicit_generated_directory_root_is_scanned() {
        let scratch = Scratch::new("root");
        let root = scratch.0.join("dist");
        fs::create_dir_all(&root).expect("dist");
        fs::write(root.join("bundle.js"), b"bundle").expect("bundle");
        let paths = DiscoveryPolicy::default()
            .walk(&root)
            .map(|entry| {
                entry
                    .expect("discovery")
                    .path()
                    .strip_prefix(&root)
                    .expect("relative")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(paths, [PathBuf::from("bundle.js")]);
    }

    #[test]
    fn explicit_literal_directory_include_reopens_all_descendants() {
        let scratch = Scratch::new("directory-override");
        fs::create_dir_all(scratch.0.join("dist/nested")).expect("dist");
        fs::write(scratch.0.join("dist/bundle.js"), b"bundle").expect("bundle");
        fs::write(scratch.0.join("dist/nested/chunk.js"), b"chunk").expect("chunk");
        fs::write(scratch.0.join("dist/drop.css"), b"drop").expect("drop");
        let paths = DiscoveryPolicy::default()
            .include("dist")
            .walk(&scratch.0)
            .filter_map(|entry| {
                let path = entry.ok()?.path().strip_prefix(&scratch.0).ok()?.to_owned();
                path.extension()
                    .is_some_and(|extension| extension == "js")
                    .then_some(path)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [
                PathBuf::from("dist/bundle.js"),
                PathBuf::from("dist/nested/chunk.js")
            ]
        );
    }

    #[test]
    fn case_insensitive_policy_matches_windows_style_names() {
        let scratch = Scratch::new("case");
        fs::create_dir_all(scratch.0.join("Build")).expect("build");
        fs::write(scratch.0.join("Build/app.ts"), b"app").expect("app");
        let paths = DiscoveryPolicy::default()
            .case_insensitive(true)
            .walk(&scratch.0)
            .map(|entry| {
                entry
                    .expect("discovery")
                    .path()
                    .strip_prefix(&scratch.0)
                    .expect("relative")
                    .to_owned()
            })
            .filter(|path| path.to_string_lossy().ends_with(".ts"))
            .collect::<Vec<_>>();
        assert!(
            paths.is_empty(),
            "Build must follow generated defaults: {paths:?}"
        );
        let reopened = DiscoveryPolicy::default()
            .case_insensitive(true)
            .include(r"build\app.ts")
            .walk(&scratch.0)
            .map(|entry| {
                entry
                    .expect("discovery")
                    .path()
                    .strip_prefix(&scratch.0)
                    .expect("relative")
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|path| path.ends_with(".ts"))
            .collect::<Vec<_>>();
        assert_eq!(reopened, ["Build/app.ts"]);
    }

    #[test]
    fn nested_negations_and_anchored_patterns_match_git_semantics() {
        let scratch = Scratch::new("patterns");
        fs::create_dir_all(scratch.0.join("src/generated")).expect("nested");
        fs::create_dir_all(scratch.0.join("generated")).expect("root generated");
        fs::write(
            scratch.0.join(".gitignore"),
            b"/generated/\nsrc/generated/\n!src/generated/\n!src/generated/keep.rs\n",
        )
        .expect("ignore");
        fs::write(scratch.0.join("generated/root.rs"), b"root").expect("root");
        fs::write(scratch.0.join("src/generated/drop.rs"), b"drop").expect("drop");
        fs::write(scratch.0.join("src/generated/keep.rs"), b"keep").expect("keep");
        fs::write(scratch.0.join("src/live.rs"), b"live").expect("live");
        let mut paths = DiscoveryPolicy::default()
            .walk(&scratch.0)
            .map(|entry| {
                entry
                    .expect("discovery")
                    .path()
                    .strip_prefix(&scratch.0)
                    .expect("relative")
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|path| path.ends_with(".rs"))
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            paths,
            [
                "src/generated/drop.rs",
                "src/generated/keep.rs",
                "src/live.rs"
            ]
        );
    }

    #[test]
    fn root_and_nested_gitignore_files_are_both_applied() {
        let scratch = Scratch::new("root-and-nested-ignore");
        fs::create_dir_all(scratch.0.join("src/nested")).expect("nested");
        fs::write(
            scratch.0.join(".gitignore"),
            b"root-drop.rs\nsrc/nested/drop.rs\n",
        )
        .expect("root ignore");
        fs::write(scratch.0.join("src/nested/.gitignore"), b"nested-drop.rs\n")
            .expect("nested ignore");
        fs::write(scratch.0.join("root-drop.rs"), b"drop").expect("root drop");
        fs::write(scratch.0.join("root-keep.rs"), b"keep").expect("root keep");
        fs::write(scratch.0.join("src/nested/drop.rs"), b"drop").expect("nested drop");
        fs::write(scratch.0.join("src/nested/nested-drop.rs"), b"drop")
            .expect("nested local drop");
        fs::write(scratch.0.join("src/nested/nested-keep.rs"), b"keep")
            .expect("nested keep");

        let mut paths = DiscoveryPolicy::default()
            .walk(&scratch.0)
            .map(|entry| {
                entry
                    .expect("discovery")
                    .path()
                    .strip_prefix(&scratch.0)
                    .expect("relative")
                    .to_string_lossy()
                    .into_owned()
            })
            .filter(|path| path.ends_with(".rs"))
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(
            paths,
            [
                "root-keep.rs",
                "src/nested/nested-keep.rs",
            ]
        );
    }

    #[test]
    fn local_git_exclude_and_escaped_patterns_are_applied_without_git() {
        let scratch = Scratch::new("exclude");
        fs::create_dir_all(scratch.0.join(".git/info")).expect("git metadata");
        fs::write(scratch.0.join(".git/info/exclude"), b"excluded.rs\n").expect("exclude");
        fs::write(scratch.0.join(".gitignore"), b"escaped\\[name\\].rs\n").expect("ignore");
        fs::write(scratch.0.join("excluded.rs"), b"excluded").expect("excluded");
        fs::write(scratch.0.join("escaped[name].rs"), b"escaped").expect("escaped");
        fs::write(scratch.0.join("kept.rs"), b"kept").expect("kept");

        let paths = DiscoveryPolicy::default()
            .walk(&scratch.0)
            .map(|entry| {
                entry
                    .expect("discovery")
                    .path()
                    .strip_prefix(&scratch.0)
                    .expect("relative")
                    .to_owned()
            })
            .filter(|path| !path.as_os_str().is_empty())
            .collect::<Vec<_>>();
        assert_eq!(
            paths,
            [PathBuf::from(".gitignore"), PathBuf::from("kept.rs")]
        );
    }

    #[test]
    fn symlinks_are_not_followed() {
        #[cfg(unix)]
        {
            let scratch = Scratch::new("symlink");
            let outside = scratch
                .0
                .with_file_name(format!("nudox-discovery-outside-{}", std::process::id()));
            fs::create_dir_all(&outside).expect("outside");
            fs::write(outside.join("escape.rs"), b"escape").expect("escape");
            std::os::unix::fs::symlink(&outside, scratch.0.join("linked")).expect("link");
            let entries = DiscoveryPolicy::default()
                .walk(&scratch.0)
                .collect::<Vec<_>>();
            assert!(entries.iter().all(|entry| {
                !entry
                    .as_ref()
                    .expect("entry")
                    .path()
                    .starts_with(scratch.0.join("linked"))
            }));
            let _ = fs::remove_dir_all(outside);
        }
    }

    #[test]
    fn missing_root_is_a_typed_discovery_error() {
        let scratch = Scratch::new("missing");
        let missing = scratch.0.join("does-not-exist");
        let results = DiscoveryPolicy::default()
            .walk(&missing)
            .collect::<Vec<_>>();
        assert_eq!(results.len(), 1, "missing roots must report one error");
        assert!(
            matches!(results.into_iter().next(), Some(Err(DiscoveryError::Io { path, .. })) if path == missing)
        );
    }
}
