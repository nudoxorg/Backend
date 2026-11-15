//! Module resolution for an indexed project, run by the compiler's own resolver.
//!
//! Consumers which need to know which indexed file a specifier names (impact
//! analysis, graph views) must agree with the compiler, so this module does not
//! resolve anything itself. It serves the indexed file set and the project's
//! JSON configuration to the same configuration loader and module resolver the
//! native program uses (`tsconfig.json` or `jsconfig.json`, `extends` chains,
//! `baseUrl`, `paths`, `moduleResolution`), and classifies the outcome.
//!
//! The view is the indexed file set plus whatever JSON the caller's reader can
//! return. Nothing outside the project root is reachable, so a configuration
//! which extends a file outside the project, or inside `node_modules`, fails
//! with a named reason instead of resolving partially.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use tsz::config::ResolvedCompilerOptions;
use tsz::diagnostics::data::diagnostic_codes;
use tsz::module_resolver::{ImportKind, ModuleResolver, ResolutionFailure};
use tsz::{ResolutionDirEntry, ResolutionFileKind, ResolutionFileSystem};

/// Virtual root under which the project is presented to the resolver.
const VIRTUAL_ROOT: &str = "/project";
const CONFIG_NAMES: [&str; 2] = ["tsconfig.json", "jsconfig.json"];

/// Reads one project-relative file. `Ok(None)` means the file does not exist.
pub type ProjectReader<'a> = dyn Fn(&str) -> io::Result<Option<Vec<u8>>> + 'a;

/// Whether `specifier` is written the way projects spell a path alias (`@/x`,
/// `~/x`, `#x`), so a failure to resolve it is a missing mapping rather than a
/// package outside the project.
#[must_use]
pub fn looks_like_path_alias(specifier: &str) -> bool {
    specifier.starts_with("@/") || specifier.starts_with("~/") || specifier.starts_with('#')
}

/// What a module specifier names, as the compiler's resolver sees it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProjectModule {
    /// An indexed source file, as a project-relative path.
    File(String),
    /// A package outside the project (a bare specifier no local mapping covers).
    Package,
    /// A specifier which should name project code but does not resolve.
    Unresolved(String),
}

/// The configuration file which governs a directory.
struct Governing {
    path: String,
    /// Why the file could not be read, when it could not.
    unreadable: Option<String>,
}

struct ConfigSlot {
    /// Project-relative path of the governing configuration, when one exists.
    path: Option<String>,
    options: ResolvedCompilerOptions,
    /// Why the configuration could not be loaded, when it could not.
    error: Option<String>,
}

/// The indexed file set served to the compiler's resolver.
struct ProjectView<'a> {
    root: PathBuf,
    files: &'a BTreeSet<String>,
    directories: BTreeSet<String>,
    read: &'a ProjectReader<'a>,
    json: RefCell<BTreeMap<String, Option<Rc<Vec<u8>>>>>,
}

impl<'a> ProjectView<'a> {
    fn new(files: &'a BTreeSet<String>, read: &'a ProjectReader<'a>) -> Self {
        let mut directories = BTreeSet::new();
        for file in files {
            let mut end = file.len();
            while let Some(slash) = file[..end].rfind('/') {
                end = slash;
                if !directories.insert(file[..end].to_owned()) {
                    break;
                }
            }
        }
        Self {
            root: PathBuf::from(VIRTUAL_ROOT),
            files,
            directories,
            read,
            json: RefCell::default(),
        }
    }

    fn relative(&self, path: &Path) -> Option<String> {
        let tail = path.strip_prefix(&self.root).ok()?;
        let mut parts = Vec::new();
        for component in tail.components() {
            match component {
                Component::Normal(part) => parts.push(part.to_str()?),
                Component::CurDir => {}
                _ => return None,
            }
        }
        Some(parts.join("/"))
    }

    fn absolute(&self, relative: &str) -> PathBuf {
        if relative.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative)
        }
    }

    /// JSON served from the reader. Only `.json` files are read, once.
    fn json(&self, relative: &str) -> io::Result<Option<Rc<Vec<u8>>>> {
        if !relative.ends_with(".json") {
            return Ok(None);
        }
        if let Some(cached) = self.json.borrow().get(relative) {
            return Ok(cached.clone());
        }
        let bytes = (self.read)(relative)?.map(Rc::new);
        self.json
            .borrow_mut()
            .insert(relative.to_owned(), bytes.clone());
        Ok(bytes)
    }

    fn is_file_relative(&self, relative: &str) -> io::Result<bool> {
        Ok(self.files.contains(relative) || self.json(relative)?.is_some())
    }

    fn is_dir_relative(&self, relative: &str) -> bool {
        relative.is_empty() || self.directories.contains(relative)
    }
}

impl ResolutionFileSystem for ProjectView<'_> {
    fn resolution_boundary(&self) -> Option<&Path> {
        Some(&self.root)
    }

    fn read_file(&self, path: &Path) -> io::Result<Option<Vec<u8>>> {
        let Some(relative) = self.relative(path) else {
            return Ok(None);
        };
        if self.files.contains(&relative) {
            return (self.read)(&relative);
        }
        Ok(self.json(&relative)?.map(|bytes| bytes.as_ref().clone()))
    }

    fn is_file(&self, path: &Path) -> io::Result<bool> {
        match self.relative(path) {
            Some(relative) => self.is_file_relative(&relative),
            None => Ok(false),
        }
    }

    fn is_dir(&self, path: &Path) -> io::Result<bool> {
        Ok(self
            .relative(path)
            .is_some_and(|relative| self.is_dir_relative(&relative)))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Option<Vec<ResolutionDirEntry>>> {
        let Some(relative) = self.relative(path) else {
            return Ok(None);
        };
        if !self.is_dir_relative(&relative) {
            return Ok(None);
        }
        let prefix = if relative.is_empty() {
            String::new()
        } else {
            format!("{relative}/")
        };
        let mut entries: BTreeMap<String, ResolutionFileKind> = BTreeMap::new();
        for file in self
            .files
            .range(prefix.clone()..)
            .take_while(|file| file.starts_with(&prefix))
        {
            let rest = &file[prefix.len()..];
            match rest.split_once('/') {
                Some((directory, _)) => {
                    entries.insert(directory.to_owned(), ResolutionFileKind::Directory);
                }
                None => {
                    entries.insert(rest.to_owned(), ResolutionFileKind::File);
                }
            }
        }
        Ok(Some(
            entries
                .into_iter()
                .map(|(name, kind)| ResolutionDirEntry {
                    name: name.into(),
                    kind,
                })
                .collect(),
        ))
    }

    fn canonicalize(&self, path: &Path) -> io::Result<Option<PathBuf>> {
        let Some(relative) = self.relative(path) else {
            return Ok(None);
        };
        Ok(
            (self.is_file_relative(&relative)? || self.is_dir_relative(&relative))
                .then(|| self.absolute(&relative)),
        )
    }

    fn symlink_metadata(&self, path: &Path) -> io::Result<Option<ResolutionFileKind>> {
        let Some(relative) = self.relative(path) else {
            return Ok(None);
        };
        if self.is_file_relative(&relative)? {
            Ok(Some(ResolutionFileKind::File))
        } else if self.is_dir_relative(&relative) {
            Ok(Some(ResolutionFileKind::Directory))
        } else {
            Ok(None)
        }
    }
}

/// Resolves specifiers written in an indexed project with the compiler's resolver.
pub struct ProjectModuleResolver<'a> {
    view: ProjectView<'a>,
    /// Governing configuration by directory.
    by_directory: RefCell<BTreeMap<String, Rc<ConfigSlot>>>,
    /// Loaded configurations by the path which governs them, so every
    /// directory under one configuration shares a single load.
    by_config: RefCell<BTreeMap<Option<String>, Rc<ConfigSlot>>>,
}

impl<'a> ProjectModuleResolver<'a> {
    /// `files` are the project-relative paths of every indexed source;
    /// `read` serves project JSON (configuration, `extends` targets, manifests).
    #[must_use]
    pub fn new(files: &'a BTreeSet<String>, read: &'a ProjectReader<'a>) -> Self {
        Self {
            view: ProjectView::new(files, read),
            by_directory: RefCell::default(),
            by_config: RefCell::default(),
        }
    }

    /// Resolve `specifier` as written in `importer` (a project-relative path).
    #[must_use]
    pub fn resolve(&self, importer: &str, specifier: &str) -> ProjectModule {
        let directory = importer
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory);
        let slot = self.config_for(directory);
        let mut resolver = ModuleResolver::with_file_system(&slot.options, &self.view);
        let outcome = resolver.resolve_with_kind_checked(
            specifier,
            &self.view.absolute(importer),
            tsz::span::Span::new(0, 0),
            ImportKind::EsmImport,
        );
        match outcome {
            Ok(Ok(resolved)) => self.classify_resolved(&resolved.resolved_path),
            // The file was found; the project merely does not enable JSX. That
            // is a diagnostic for the checker, not a missing module, and a
            // project with `.tsx` sources and no `jsx` option still has them.
            Ok(Err(ResolutionFailure::JsxNotEnabled { resolved_path, .. })) => {
                self.classify_resolved(&resolved_path)
            }
            Ok(Err(_)) => self.classify_failure(&slot, specifier),
            Err(error) => ProjectModule::Unresolved(format!(
                "the resolver could not read the project: {error}"
            )),
        }
    }

    fn classify_resolved(&self, resolved_path: &Path) -> ProjectModule {
        match self.view.relative(resolved_path) {
            Some(relative) if self.view.files.contains(&relative) => ProjectModule::File(relative),
            Some(relative) => ProjectModule::Unresolved(format!(
                "resolves to {relative}, which is not an indexed source file"
            )),
            None => ProjectModule::Package,
        }
    }

    /// The project JSON served to the resolver so far, as `(path, bytes)`:
    /// every configuration, `extends` target and manifest its answers rest on.
    /// Files which do not exist are not listed. Call this after resolving.
    #[must_use]
    pub fn files_read(&self) -> Vec<(String, Vec<u8>)> {
        self.view
            .json
            .borrow()
            .iter()
            .filter_map(|(path, bytes)| Some((path.clone(), bytes.as_ref()?.as_ref().clone())))
            .collect()
    }

    /// Configurations which could not be loaded, as `(path, reason)`.
    #[must_use]
    pub fn configuration_errors(&self) -> Vec<(String, String)> {
        let mut seen = BTreeSet::new();
        self.by_directory
            .borrow()
            .values()
            .filter_map(|slot| Some((slot.path.clone()?, slot.error.clone()?)))
            .filter(|entry| seen.insert(entry.clone()))
            .collect()
    }

    fn classify_failure(&self, slot: &ConfigSlot, specifier: &str) -> ProjectModule {
        if let Some(error) = &slot.error {
            let config = slot.path.as_deref().unwrap_or("tsconfig.json");
            return ProjectModule::Unresolved(format!("{config} could not be loaded: {error}"));
        }
        if specifier.starts_with("./")
            || specifier.starts_with("../")
            || specifier == "."
            || specifier == ".."
        {
            return ProjectModule::Unresolved(
                "no indexed file matches this relative path".to_owned(),
            );
        }
        if let Some(pattern) = Self::matching_pattern(&slot.options, specifier) {
            let config = slot.path.as_deref().unwrap_or("tsconfig.json");
            return ProjectModule::Unresolved(format!(
                "matches paths pattern \"{pattern}\" in {config}, but none of its targets is an indexed file"
            ));
        }
        if self.base_url_covers(&slot.options, specifier) {
            return ProjectModule::Unresolved(
                "starts at a baseUrl entry, but no indexed file matches".to_owned(),
            );
        }
        if looks_like_path_alias(specifier) {
            return ProjectModule::Unresolved(match &slot.path {
                Some(config) => format!(
                    "looks like a path alias, but {config} declares no paths entry that matches it"
                ),
                None => {
                    "looks like a path alias, but the project has no tsconfig.json or jsconfig.json"
                        .to_owned()
                }
            });
        }
        ProjectModule::Package
    }

    fn matching_pattern(options: &ResolvedCompilerOptions, specifier: &str) -> Option<String> {
        options.paths.as_ref()?.iter().find_map(|mapping| {
            let matches = match mapping.pattern.split_once('*') {
                Some((prefix, suffix)) => {
                    specifier.len() >= prefix.len() + suffix.len()
                        && specifier.starts_with(prefix)
                        && specifier.ends_with(suffix)
                }
                None => mapping.pattern == specifier,
            };
            matches.then(|| mapping.pattern.clone())
        })
    }

    fn base_url_covers(&self, options: &ResolvedCompilerOptions, specifier: &str) -> bool {
        let Some(base) = options
            .base_url
            .as_ref()
            .and_then(|base| self.view.relative(base))
        else {
            return false;
        };
        let first = specifier.split('/').next().unwrap_or(specifier);
        if first.is_empty() {
            return false;
        }
        let joined = if base.is_empty() {
            first.to_owned()
        } else {
            format!("{base}/{first}")
        };
        // A file named by the first segment plus an extension: only paths with
        // that prefix can match, and the sorted set yields exactly those.
        self.view.is_dir_relative(&joined)
            || self
                .view
                .files
                .range(joined.clone()..)
                .take_while(|file| file.starts_with(&joined))
                .any(|file| {
                    let rest = &file[joined.len()..];
                    rest.starts_with('.') && !rest[1..].contains('/')
                })
    }

    /// The governing configuration of `directory`: the nearest `tsconfig.json`
    /// or `jsconfig.json` at or above it, inside the project. A configuration
    /// is loaded once, however many directories it governs.
    fn config_for(&self, directory: &str) -> Rc<ConfigSlot> {
        if let Some(slot) = self.by_directory.borrow().get(directory) {
            return Rc::clone(slot);
        }
        let governing = self.governing_config(directory);
        let cached = self
            .by_config
            .borrow()
            .get(&governing.as_ref().map(|found| found.path.clone()))
            .cloned();
        let slot = cached.unwrap_or_else(|| {
            let slot = Rc::new(match governing {
                Some(Governing {
                    path,
                    unreadable: Some(error),
                }) => ConfigSlot {
                    path: Some(path),
                    options: ResolvedCompilerOptions::default(),
                    error: Some(error),
                },
                Some(Governing {
                    path,
                    unreadable: None,
                }) => self.load_config(path),
                None => ConfigSlot {
                    path: None,
                    options: ResolvedCompilerOptions::default(),
                    error: None,
                },
            });
            self.by_config
                .borrow_mut()
                .insert(slot.path.clone(), Rc::clone(&slot));
            slot
        });
        self.by_directory
            .borrow_mut()
            .insert(directory.to_owned(), Rc::clone(&slot));
        slot
    }

    /// The nearest configuration file at or above `directory`, if any.
    fn governing_config(&self, directory: &str) -> Option<Governing> {
        let mut current = directory;
        loop {
            let found = CONFIG_NAMES.iter().find_map(|name| {
                let path = if current.is_empty() {
                    (*name).to_owned()
                } else {
                    format!("{current}/{name}")
                };
                match self.view.json(&path) {
                    Ok(Some(_)) => Some(Governing {
                        path,
                        unreadable: None,
                    }),
                    Ok(None) => None,
                    Err(error) => Some(Governing {
                        path,
                        unreadable: Some(error.to_string()),
                    }),
                }
            });
            if found.is_some() {
                return found;
            }
            match current.rsplit_once('/') {
                Some((parent, _)) => current = parent,
                None if current.is_empty() => return None,
                None => current = "",
            }
        }
    }

    fn load_config(&self, path: String) -> ConfigSlot {
        let failed = |path: String, error: String| ConfigSlot {
            path: Some(path),
            options: ResolvedCompilerOptions::default(),
            error: Some(error),
        };
        let parsed = match tsz::config::load_tsconfig_with_diagnostics_and_file_system(
            &self.view.absolute(&path),
            &self.view,
        ) {
            Ok(parsed) => parsed,
            Err(error) => return failed(path, error.to_string()),
        };
        // The loader goes on past an `extends` it cannot read, as `tsc` does, and
        // only records a diagnostic. The options left would be a guess.
        if let Some(missing) = parsed.diagnostics.iter().find(|diagnostic| {
            matches!(
                diagnostic.code,
                diagnostic_codes::FILE_NOT_FOUND | diagnostic_codes::CANNOT_READ_FILE_2
            )
        }) {
            return failed(path, missing.message_text.clone());
        }
        let mut options = match tsz::config::resolve_compiler_options_embedded(
            parsed.config.compiler_options.as_ref(),
        ) {
            Ok(options) => options,
            Err(error) => return failed(path, error.to_string()),
        };
        // Same anchoring as the native program: `paths` without `baseUrl` is
        // relative to the configuration which declared it.
        if options.base_url.is_none()
            && options
                .paths
                .as_ref()
                .is_some_and(|paths| !paths.is_empty())
            && let Some(anchor) = parsed.paths_base_path
        {
            options.paths_base_path = Some(anchor);
        }
        ConfigSlot {
            path: Some(path),
            options,
            error: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn project(
        files: &[&str],
        json: &[(&str, &str)],
    ) -> (BTreeSet<String>, HashMap<String, String>) {
        (
            files.iter().map(|file| (*file).to_owned()).collect(),
            json.iter()
                .map(|(path, text)| ((*path).to_owned(), (*text).to_owned()))
                .collect(),
        )
    }

    fn resolve(
        files: &BTreeSet<String>,
        json: &HashMap<String, String>,
        importer: &str,
        specifier: &str,
    ) -> ProjectModule {
        let read = |path: &str| Ok(json.get(path).map(|text| text.clone().into_bytes()));
        ProjectModuleResolver::new(files, &read).resolve(importer, specifier)
    }

    #[test]
    fn paths_alias_without_base_url_resolves_to_indexed_file() {
        let (files, json) = project(
            &["src/components/Button.tsx", "src/app/page.tsx"],
            &[(
                "tsconfig.json",
                r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
            )],
        );
        assert_eq!(
            resolve(&files, &json, "src/app/page.tsx", "@/components/Button"),
            ProjectModule::File("src/components/Button.tsx".to_owned())
        );
        assert_eq!(
            resolve(&files, &json, "src/app/page.tsx", "../components/Button"),
            ProjectModule::File("src/components/Button.tsx".to_owned())
        );
    }

    #[test]
    fn tsx_sources_resolve_when_there_is_no_configuration_to_enable_jsx() {
        let (files, json) = project(
            &["src/ui/Button.tsx", "src/ui/index.tsx", "src/page.tsx"],
            &[],
        );
        assert_eq!(
            resolve(&files, &json, "src/page.tsx", "./ui/Button"),
            ProjectModule::File("src/ui/Button.tsx".to_owned())
        );
        assert_eq!(
            resolve(&files, &json, "src/page.tsx", "./ui"),
            ProjectModule::File("src/ui/index.tsx".to_owned())
        );
    }

    #[test]
    fn extends_chain_and_base_url_resolve_like_the_compiler() {
        let (files, json) = project(
            &[
                "web/src/lib/util.ts",
                "web/src/app/a.ts",
                "web/src/ui/index.ts",
            ],
            &[
                (
                    "tsconfig.base.json",
                    r#"{"compilerOptions":{"baseUrl":".","paths":{"@lib/*":["web/src/lib/*"]}}}"#,
                ),
                (
                    "web/tsconfig.json",
                    r#"{"extends":"../tsconfig.base.json"}"#,
                ),
            ],
        );
        assert_eq!(
            resolve(&files, &json, "web/src/app/a.ts", "@lib/util"),
            ProjectModule::File("web/src/lib/util.ts".to_owned())
        );
        // baseUrl-relative, directory index.
        assert_eq!(
            resolve(&files, &json, "web/src/app/a.ts", "web/src/ui"),
            ProjectModule::File("web/src/ui/index.ts".to_owned())
        );
    }

    #[test]
    fn unresolvable_alias_is_named_and_packages_are_not_flagged() {
        let (files, json) = project(
            &["src/app/page.tsx"],
            &[(
                "tsconfig.json",
                r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
            )],
        );
        match resolve(&files, &json, "src/app/page.tsx", "@/components/Missing") {
            ProjectModule::Unresolved(reason) => assert!(reason.contains("\"@/*\""), "{reason}"),
            other => panic!("expected unresolved alias, got {other:?}"),
        }
        assert_eq!(
            resolve(&files, &json, "src/app/page.tsx", "react"),
            ProjectModule::Package
        );
        match resolve(&files, &json, "src/app/page.tsx", "./nope") {
            ProjectModule::Unresolved(_) => {}
            other => panic!("expected unresolved relative, got {other:?}"),
        }
    }

    #[test]
    fn alias_without_a_configuration_is_unresolved_not_a_package() {
        let (files, json) = project(&["src/a.ts", "src/b.ts"], &[]);
        match resolve(&files, &json, "src/a.ts", "@/b") {
            ProjectModule::Unresolved(reason) => {
                assert!(reason.contains("no tsconfig.json"), "{reason}")
            }
            other => panic!("expected unresolved alias, got {other:?}"),
        }
    }

    #[test]
    fn unloadable_extends_is_reported_instead_of_guessed() {
        let (files, json) = project(
            &["src/a.ts", "src/b.ts"],
            &[(
                "tsconfig.json",
                r#"{"extends":"../outside/tsconfig.json","compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
            )],
        );
        let read = |path: &str| Ok(json.get(path).map(|text| text.clone().into_bytes()));
        let resolver = ProjectModuleResolver::new(&files, &read);
        match resolver.resolve("src/a.ts", "@/b") {
            ProjectModule::Unresolved(reason) => assert!(
                reason.starts_with("tsconfig.json could not be loaded: "),
                "{reason}"
            ),
            other => panic!("expected the configuration failure, got {other:?}"),
        }
        let errors = resolver.configuration_errors();
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert_eq!(errors[0].0, "tsconfig.json");
        assert!(errors[0].1.contains("outside/tsconfig.json"), "{errors:?}");
    }

    #[test]
    fn directories_under_one_configuration_share_its_single_load() {
        let (files, json) = project(
            &["src/a/x.ts", "src/b/c/y.ts", "other/z.ts"],
            &[(
                "tsconfig.json",
                r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
            )],
        );
        let read = |path: &str| Ok(json.get(path).map(|text| text.clone().into_bytes()));
        let resolver = ProjectModuleResolver::new(&files, &read);
        let first = resolver.config_for("src/a");
        for directory in ["src/b/c", "src/b", "other", ""] {
            assert!(
                Rc::ptr_eq(&first, &resolver.config_for(directory)),
                "{directory}"
            );
        }
        assert_eq!(resolver.by_config.borrow().len(), 1);
        assert_eq!(resolver.by_directory.borrow().len(), 5);
    }

    #[test]
    fn a_base_url_entry_that_matches_nothing_is_named_and_a_near_prefix_is_not_one() {
        let (files, json) = project(
            &["src/lib.ts", "src/library/x.ts", "src/app/a.ts"],
            &[("tsconfig.json", r#"{"compilerOptions":{"baseUrl":"src"}}"#)],
        );
        // `src/lib.ts` exists, so the first segment names a baseUrl entry.
        match resolve(&files, &json, "src/app/a.ts", "lib/missing") {
            ProjectModule::Unresolved(reason) => assert!(reason.contains("baseUrl"), "{reason}"),
            other => panic!("expected a baseUrl entry, got {other:?}"),
        }
        // A directory of that name does too.
        match resolve(&files, &json, "src/app/a.ts", "library/missing") {
            ProjectModule::Unresolved(reason) => assert!(reason.contains("baseUrl"), "{reason}"),
            other => panic!("expected a baseUrl entry, got {other:?}"),
        }
        // `src/library/x.ts` shares the prefix `src/lib` without being `src/lib.*`.
        let (files, json) = project(
            &["src/library/x.ts", "src/app/a.ts"],
            &[("tsconfig.json", r#"{"compilerOptions":{"baseUrl":"src"}}"#)],
        );
        assert_eq!(
            resolve(&files, &json, "src/app/a.ts", "lib/y"),
            ProjectModule::Package
        );
        assert_eq!(
            resolve(&files, &json, "src/app/a.ts", "react"),
            ProjectModule::Package
        );
    }

    #[test]
    fn path_alias_spellings_are_unresolved_not_packages() {
        for specifier in ["@/x", "~/x", "#x"] {
            assert!(looks_like_path_alias(specifier), "{specifier}");
        }
        for specifier in ["@scope/pkg", "react", "~user", "x#y"] {
            assert!(!looks_like_path_alias(specifier), "{specifier}");
        }
        let (files, json) = project(
            &["src/a.ts"],
            &[("tsconfig.json", r#"{"compilerOptions":{}}"#)],
        );
        for specifier in ["@/x", "~/x", "#x"] {
            match resolve(&files, &json, "src/a.ts", specifier) {
                ProjectModule::Unresolved(reason) => {
                    assert!(
                        reason.contains("looks like a path alias")
                            && reason.contains("tsconfig.json"),
                        "{specifier}: {reason}"
                    );
                }
                other => panic!("{specifier}: expected unresolved alias, got {other:?}"),
            }
        }
        assert_eq!(
            resolve(&files, &json, "src/a.ts", "@scope/pkg"),
            ProjectModule::Package
        );
    }

    #[test]
    fn the_configuration_chain_is_reported_as_read() {
        let (files, json) = project(
            &["web/src/lib/util.ts", "web/src/app/a.ts"],
            &[
                (
                    "tsconfig.base.json",
                    r#"{"compilerOptions":{"baseUrl":".","paths":{"@lib/*":["web/src/lib/*"]}}}"#,
                ),
                (
                    "web/tsconfig.json",
                    r#"{"extends":"../tsconfig.base.json"}"#,
                ),
            ],
        );
        let read = |path: &str| Ok(json.get(path).map(|text| text.clone().into_bytes()));
        let resolver = ProjectModuleResolver::new(&files, &read);
        assert!(
            resolver.files_read().is_empty(),
            "nothing is read before the first resolution"
        );
        let _ = resolver.resolve("web/src/app/a.ts", "@lib/util");
        let paths: Vec<String> = resolver
            .files_read()
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        assert_eq!(paths, ["tsconfig.base.json", "web/tsconfig.json"]);
        let bytes = resolver
            .files_read()
            .into_iter()
            .find(|(path, _)| path == "web/tsconfig.json")
            .map(|(_, bytes)| bytes);
        assert_eq!(
            bytes.as_deref(),
            json.get("web/tsconfig.json").map(String::as_bytes)
        );
    }

    #[test]
    fn nearest_configuration_governs_each_directory() {
        let (files, json) = project(
            &[
                "apps/a/src/x.ts",
                "apps/a/src/y.ts",
                "apps/b/src/x.ts",
                "apps/b/lib/y.ts",
            ],
            &[
                (
                    "apps/a/tsconfig.json",
                    r#"{"compilerOptions":{"paths":{"@/*":["./src/*"]}}}"#,
                ),
                (
                    "apps/b/tsconfig.json",
                    r#"{"compilerOptions":{"paths":{"@/*":["./lib/*"]}}}"#,
                ),
            ],
        );
        assert_eq!(
            resolve(&files, &json, "apps/a/src/x.ts", "@/y"),
            ProjectModule::File("apps/a/src/y.ts".to_owned())
        );
        assert_eq!(
            resolve(&files, &json, "apps/b/src/x.ts", "@/y"),
            ProjectModule::File("apps/b/lib/y.ts".to_owned())
        );
    }
}
