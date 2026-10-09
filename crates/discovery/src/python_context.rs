//! Finite static Python source layout declarations, never packaging code execution.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use super::{DiscoveryError, DiscoveryPolicy, SourceSelectionScope};

const MAX_DOCUMENT_BYTES: usize = 1024 * 1024;
const MAX_DOCUMENTS: usize = 4096;
const MAX_DOCUMENT_TOTAL_BYTES: usize = 32 * 1024 * 1024;

/// Immutable source/package roots established by bounded static declarations.
/// Supported inputs are Pyrefly search paths, setuptools package-dir/find.where,
/// and Hatch wheel packages. Unsupported/dynamic declarations establish no root.
/// This is finite source-selection evidence, not a complete packaging resolver.
#[derive(Clone, Debug, Default)]
pub struct PythonSourceContext {
    source_roots: BTreeSet<PathBuf>,
    package_roots: BTreeSet<PathBuf>,
}

impl PythonSourceContext {
    /// Derives context from exact regular manifest/configuration members. Paths
    /// are project-relative; roots stay within each document's own directory.
    /// Parent traversal, absolute paths and dynamic declarations grant no root,
    /// so a nested document cannot reopen an ancestor or sibling output tree.
    /// The owner must retain and validate those original document bytes.
    pub fn from_documents<'a>(
        documents: impl IntoIterator<Item = (&'a Path, &'a [u8])>,
        case_insensitive: bool,
    ) -> Result<Self, DiscoveryError> {
        let mut context = Self::default();
        let mut count = 0usize;
        let mut total = 0usize;
        for (path, bytes) in documents {
            if path.is_absolute()
                || path
                    .components()
                    .any(|c| !matches!(c, Component::Normal(_)))
            {
                return Err(context_error(
                    path,
                    "unconfined Python source context document",
                ));
            }
            if !is_python_context_document(path, case_insensitive) {
                continue;
            }
            count = count.saturating_add(1);
            total = total.saturating_add(bytes.len());
            if count > MAX_DOCUMENTS
                || bytes.len() > MAX_DOCUMENT_BYTES
                || total > MAX_DOCUMENT_TOTAL_BYTES
            {
                return Err(context_error(
                    path,
                    "Python source context exceeds bounded document inputs",
                ));
            }
            let Ok(text) = std::str::from_utf8(bytes) else {
                continue;
            };
            let Ok(document) = text.parse::<toml::Value>() else {
                continue;
            };
            let base = path.parent().unwrap_or_else(|| Path::new(""));
            let pyproject = path
                .file_name()
                .is_some_and(|name| names_equal(name, "pyproject.toml", case_insensitive));
            let pyrefly = if pyproject {
                table(&document, &["tool", "pyrefly"])
            } else {
                Some(&document)
            };
            if let Some(paths) = pyrefly
                .and_then(|v| v.get("search-path"))
                .and_then(toml::Value::as_array)
            {
                for path in paths {
                    context.add_root(base, path.as_str(), false)?;
                }
            }
            if !pyproject {
                continue;
            }
            if let Some(paths) = table(&document, &["tool", "setuptools", "packages", "find"])
                .and_then(|v| v.get("where"))
                .and_then(toml::Value::as_array)
            {
                for path in paths {
                    context.add_root(base, path.as_str(), false)?;
                }
            }
            if let Some(paths) = table(&document, &["tool", "setuptools", "package-dir"])
                .and_then(toml::Value::as_table)
            {
                for (name, path) in paths {
                    context.add_root(base, path.as_str(), !name.is_empty())?;
                }
            }
            if let Some(paths) = table(&document, &["tool", "hatch", "build", "targets", "wheel"])
                .and_then(|v| v.get("packages"))
                .and_then(toml::Value::as_array)
            {
                for path in paths {
                    context.add_root(base, path.as_str(), true)?;
                }
            }
        }
        Ok(context)
    }

    fn add_root(
        &mut self,
        base: &Path,
        value: Option<&str>,
        package: bool,
    ) -> Result<(), DiscoveryError> {
        let Some(value) = value else { return Ok(()) };
        let path = Path::new(value);
        if value.is_empty()
            || value.len() > 4096
            || value.contains(['\\', ':', '\0'])
            || path.is_absolute()
            || path
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
        {
            return Ok(());
        }
        let path = base.join(
            path.components()
                .filter(|c| !matches!(c, Component::CurDir))
                .collect::<PathBuf>(),
        );
        if package {
            self.package_roots.insert(path);
        } else {
            self.source_roots.insert(path);
        }
        self.check_root_bound(base)
    }

    pub(super) fn permits(
        &self,
        directory: &Path,
        case_insensitive: bool,
        initialized: impl Fn(&Path) -> bool,
    ) -> bool {
        self.source_roots.iter().any(|root| {
            is_within(root, directory, case_insensitive)
                || (is_within(directory, root, case_insensitive) && initialized(directory))
        }) || self.package_roots.iter().any(|root| {
            initialized(root)
                && (is_within(root, directory, case_insensitive)
                    || (is_within(directory, root, case_insensitive) && initialized(directory)))
        })
    }

    /// Extends an owner-held context with a relative root established by its
    /// captured native finder graph, never a guessed module-name directory.
    pub fn add_captured_source_root(&mut self, root: &Path) -> Result<(), DiscoveryError> {
        if root.is_absolute()
            || root
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(context_error(
                root,
                "unconfined captured Python source root",
            ));
        }
        self.source_roots.insert(root.to_owned());
        self.check_root_bound(root)
    }

    fn check_root_bound(&self, path: &Path) -> Result<(), DiscoveryError> {
        if self
            .source_roots
            .len()
            .saturating_add(self.package_roots.len())
            > 4096
        {
            return Err(context_error(
                path,
                "Python source context exceeds declared root bound",
            ));
        }
        Ok(())
    }
}

/// Original bytes consumed for one finite context, owned by this discovery call.
#[derive(Clone, Debug)]
pub struct PythonSourceContextCapture {
    context: PythonSourceContext,
    documents: Vec<(PathBuf, Vec<u8>)>,
    policy: DiscoveryPolicy,
}

impl PythonSourceContextCapture {
    /// Captures only ordinary manifests admitted by the same generic policy.
    /// A manifest inside an excluded output tree cannot reopen its own tree.
    pub fn capture(root: &Path, policy: DiscoveryPolicy) -> Result<Self, DiscoveryError> {
        let policy = policy.source_scope(SourceSelectionScope::Generic);
        let directory = backend_platform::DirectoryCapability::open_read_only_source(root)
            .map_err(|error| context_error(root, &error.to_string()))?;
        let mut documents = Vec::new();
        let mut total = 0usize;
        for entry in policy.clone().walk(root) {
            let entry = entry?;
            if !entry.is_file()
                || !is_python_context_document(entry.path(), policy.case_insensitive)
            {
                continue;
            }
            let relative = entry
                .path()
                .strip_prefix(root)
                .map_err(|_| context_error(entry.path(), "unconfined Python context document"))?;
            let bytes = read_document(&directory, relative)
                .map_err(|error| context_error(entry.path(), &error.to_string()))?;
            total = total.saturating_add(bytes.len());
            if documents.len() >= MAX_DOCUMENTS || total > MAX_DOCUMENT_TOTAL_BYTES {
                return Err(context_error(
                    entry.path(),
                    "Python source context exceeds aggregate input bounds",
                ));
            }
            documents.push((relative.to_owned(), bytes));
        }
        directory
            .verify_path(root)
            .map_err(|error| context_error(root, &error.to_string()))?;
        let context = PythonSourceContext::from_documents(
            documents
                .iter()
                .map(|(path, bytes)| (path.as_path(), bytes.as_slice())),
            policy.case_insensitive,
        )?;
        Ok(Self {
            context,
            documents,
            policy,
        })
    }

    /// The immutable declarations established by these captured documents.
    #[must_use]
    pub const fn context(&self) -> &PythonSourceContext {
        &self.context
    }

    /// Refuses changed bytes, newly present documents, and removed documents.
    pub fn validate_current(&self, root: &Path) -> Result<(), DiscoveryError> {
        let current = Self::capture(root, self.policy.clone())?;
        if current.documents != self.documents {
            return Err(context_error(
                root,
                "Python source context documents changed during discovery",
            ));
        }
        Ok(())
    }
}

/// Whether an ordinary member is a supported source-context document.
#[must_use]
pub fn is_python_context_document(path: &Path, case_insensitive: bool) -> bool {
    path.file_name().is_some_and(|name| {
        ["pyproject.toml", "pyrefly.toml", ".pyrefly.toml"]
            .iter()
            .any(|candidate| names_equal(name, candidate, case_insensitive))
    })
}

fn names_equal(name: &std::ffi::OsStr, candidate: &str, case_insensitive: bool) -> bool {
    if case_insensitive {
        name.eq_ignore_ascii_case(candidate)
    } else {
        name == std::ffi::OsStr::new(candidate)
    }
}

fn read_document(
    root: &backend_platform::DirectoryCapability,
    relative: &Path,
) -> std::io::Result<Vec<u8>> {
    let mut directory = root.clone();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else {
            return Err(std::io::Error::other("unconfined source context document"));
        };
        let name = name
            .to_str()
            .ok_or_else(|| std::io::Error::other("non-Unicode source context document"))?;
        if components.peek().is_some() {
            directory = directory.open_dir(name)?;
        } else {
            return backend_platform::durable::read_regular_bounded_stable_at(
                &directory,
                name,
                MAX_DOCUMENT_BYTES,
            );
        }
    }
    Err(std::io::Error::other("empty source context document path"))
}

// `path` is the descendant (or the same directory); `ancestor` is its prefix.
fn is_within(path: &Path, ancestor: &Path, case_insensitive: bool) -> bool {
    if !case_insensitive {
        return path.starts_with(ancestor);
    }
    let mut path = path.components();
    ancestor.components().all(|component| {
        path.next().is_some_and(|actual| {
            actual
                .as_os_str()
                .eq_ignore_ascii_case(component.as_os_str())
        })
    })
}

fn table<'a>(value: &'a toml::Value, keys: &[&str]) -> Option<&'a toml::Value> {
    keys.iter().try_fold(value, |value, key| value.get(*key))
}

fn context_error(path: &Path, detail: &str) -> DiscoveryError {
    DiscoveryError::Io {
        path: path.to_owned(),
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_roots_need_markers_for_generated_descendants() {
        for declaration in [
            "[tool.pyrefly]\nsearch-path = ['.']\n",
            "[tool.setuptools.packages.find]\nwhere = ['src']\n",
        ] {
            let context = PythonSourceContext::from_documents(
                [(Path::new("pyproject.toml"), declaration.as_bytes())],
                false,
            )
            .expect("context");
            for directory in ["build", "dist", "src/build", "src/ns/dist"] {
                assert!(
                    !context.permits(Path::new(directory), false, |_| false),
                    "uninitialized output reopened: {declaration:?}, {directory}"
                );
            }
        }
        let mut captured = PythonSourceContext::default();
        captured
            .add_captured_source_root(Path::new("build/src"))
            .expect("authentic finder root");
        assert!(captured.permits(Path::new("build"), false, |_| false));
        assert!(!captured.permits(Path::new("build/src/dist"), false, |_| false));
    }

    #[test]
    fn nested_declarations_cannot_authorize_ancestor_or_sibling_trees() {
        let context = PythonSourceContext::from_documents(
            [
                (
                    Path::new("nested/pyrefly.toml"),
                    b"search-path = ['..', '../sibling', '/build', 'C:\\build', 'src']\n"
                        .as_slice(),
                ),
                (
                    Path::new("nested/pyproject.toml"),
                    b"[tool.hatch.build.targets.wheel]\npackages = ['../build', 'src/build']\n"
                        .as_slice(),
                ),
            ],
            false,
        )
        .expect("context");
        for directory in ["build", "dist", "sibling/build", "src/build"] {
            assert!(
                !context.permits(Path::new(directory), false, |_| true),
                "nested declaration escaped its document directory: {directory}"
            );
        }
        assert!(context.permits(Path::new("nested/src/build"), false, |_| true));
        assert!(!context.permits(Path::new("nested/src/dist"), false, |_| false));
    }

    #[test]
    fn config_basenames_follow_the_effective_case_policy() {
        for (name, bytes) in [
            (
                "PYPROJECT.TOML",
                "[tool.setuptools.packages.find]\nwhere = ['src']\n",
            ),
            ("PYREFLY.TOML", "search-path = ['src']\n"),
            (".PYREFLY.TOML", "search-path = ['src']\n"),
        ] {
            for case in [false, true] {
                let path = Path::new(name);
                assert_eq!(is_python_context_document(path, case), case);
                let context = PythonSourceContext::from_documents([(path, bytes.as_bytes())], case)
                    .expect("context");
                assert_eq!(
                    context.permits(Path::new("src/build"), case, |_| true),
                    case,
                    "configuration filename case: {name}, case={case}"
                );
            }
        }
    }

    #[test]
    fn malformed_dynamic_and_non_utf8_documents_grant_no_root() {
        for bytes in [
            b"not [valid toml".as_slice(),
            b"[tool.setuptools.packages.find]\nwhere = {dynamic = 'build'}\n".as_slice(),
            b"[tool.pyrefly]\nsearch-path = [42, '../build']\n".as_slice(),
            b"\xff".as_slice(),
        ] {
            let context =
                PythonSourceContext::from_documents([(Path::new("pyproject.toml"), bytes)], false)
                    .expect("unsupported document");
            assert!(context.source_roots.is_empty());
            assert!(context.package_roots.is_empty());
        }
        let oversized = vec![b' '; MAX_DOCUMENT_BYTES + 1];
        assert!(
            PythonSourceContext::from_documents(
                [(Path::new("pyproject.toml"), oversized.as_slice())],
                false,
            )
            .is_err()
        );
        assert!(
            PythonSourceContext::from_documents(
                [(Path::new("../pyproject.toml"), b"".as_slice())],
                false,
            )
            .is_err()
        );
    }
}
