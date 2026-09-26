//! Filesystem walk and generated-directory admission for one discovery policy.

use super::{
    DEFAULT_IGNORED_DIRECTORIES, Discovery, DiscoveryError, DiscoveryPolicy, normalize_pattern,
    slash_path,
};
use ignore::{WalkBuilder, overrides::OverrideBuilder};
use std::{ffi::OsStr, path::Path};

impl DiscoveryPolicy {
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
