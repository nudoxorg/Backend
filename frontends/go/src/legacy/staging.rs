//! Real-package authority fixture staging for the Go oracle.
//!
//! The corpus audit selects one exact source file from an immutable Go module
//! checkout under `$NUDOX_GO_CORPUS_DIR` (`<import path>@<version>`). To ask
//! the `go/packages` oracle about that file's real package without serializing
//! the rest of the module, the fixture:
//!
//! * roots itself at the selected file's real module (the nearest ancestor
//!   whose name carries the `@version` suffix),
//! * synthesizes a `go.mod` naming the real module path at
//!   [`GO_FIXTURE_LANGUAGE_VERSION`],
//! * copies every non-`_test.go` `.go` file of the selected package and of its
//!   same-module import closure under the same relative paths, and
//! * adds `require`/`replace` directives for external imports whose sibling
//!   module checkout exists in the corpus.
//!
//! Sibling packages are import context only. The oracle is then invoked in
//! its package-selected mode, so exactly the selected package is serialized
//! and two packages that share a declaration spelling cannot collide.
//! Modules that are genuinely absent stay unresolved; the oracle refuses the
//! row instead of a fabricated image.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// The language version the synthesized fixture module declares. Every corpus
/// module predates it, so the directive never rejects an authority source.
pub const GO_FIXTURE_LANGUAGE_VERSION: &str = "1.23";

/// A staged Go module fixture rooted at a real corpus checkout.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedGoModule {
    /// The fixture's module root: the `go.mod` directory handed to the oracle.
    pub root: PathBuf,
    /// The binding path carrying the selected source's exact bytes.
    pub source: PathBuf,
}

/// Filesystem failure while staging a Go module fixture.
#[derive(Debug)]
pub enum StagingError {
    /// A fixture file or directory could not be created or read.
    Io(io::Error),
}

impl From<io::Error> for StagingError {
    fn from(source: io::Error) -> Self {
        Self::Io(source)
    }
}

impl core::fmt::Display for StagingError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(source) => write!(formatter, "staging Go authority fixture: {source}"),
        }
    }
}

impl std::error::Error for StagingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(source) => Some(source),
        }
    }
}

/// The module root for one corpus Go source: the nearest ancestor directory
/// whose name carries the corpus `@version` suffix (`…/mod@v1.2.3`, including
/// the `/vN@vN.x.y` spelling of a major-version module).
pub fn module_root(selected_path: &Path) -> Option<PathBuf> {
    let mut current = selected_path.parent()?;
    loop {
        if current
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains('@'))
        {
            return Some(current.to_owned());
        }
        current = current.parent()?;
    }
}

/// Stages the selected file's real Go module under `fixture_root`.
///
/// `fixture_root` must already exist and be empty (or writable). The returned
/// binding path carries exactly `source`. The corpus root is read from
/// `$NUDOX_GO_CORPUS_DIR`; see [`stage_module_with_corpus`] for the explicit
/// form.
pub fn stage_module(
    fixture_root: &Path,
    selected_path: &Path,
    source: &[u8],
) -> Result<StagedGoModule, StagingError> {
    let corpus_root = std::env::var_os("NUDOX_GO_CORPUS_DIR").map(PathBuf::from);
    stage_module_with_corpus(fixture_root, selected_path, source, corpus_root.as_deref())
}

/// [`stage_module`] with the corpus root passed explicitly, the seam the
/// unit tests drive with their own temporary corpus trees.
fn stage_module_with_corpus(
    fixture_root: &Path,
    selected_path: &Path,
    source: &[u8],
    corpus_root: Option<&Path>,
) -> Result<StagedGoModule, StagingError> {
    let module_root = module_root(selected_path)
        .or_else(|| selected_path.parent().map(Path::to_owned))
        .unwrap_or_else(|| fixture_root.to_owned());
    let module_path = fs::read(module_root.join("go.mod"))
        .ok()
        .and_then(|bytes| module_directive(&bytes))
        .or_else(|| module_path_from_corpus(&module_root, corpus_root))
        .unwrap_or_else(|| "fixture".to_owned());
    let selected_relative = selected_path
        .strip_prefix(&module_root)
        .map(Path::to_owned)
        .unwrap_or_else(|_| {
            selected_path
                .file_name()
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("package.go"))
        });
    let selected_dir_relative = selected_relative
        .parent()
        .map(Path::to_owned)
        .unwrap_or_default();
    let selected_file_name = selected_relative
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("package.go")
        .to_owned();

    // Breadth-first over the selected package and its same-module import
    // closure. Every staged non-test file is scanned so a staged subpackage's
    // own same-module imports are staged in turn.
    let mut visited: Vec<PathBuf> = Vec::new();
    let mut queue: Vec<PathBuf> = vec![selected_dir_relative.clone()];
    let mut external: Vec<String> = Vec::new();
    while let Some(relative) = queue.pop() {
        if visited.contains(&relative) {
            continue;
        }
        visited.push(relative.clone());
        let source_dir = module_root.join(&relative);
        let Ok(entries) = fs::read_dir(&source_dir) else {
            continue;
        };
        let mut names: Vec<String> = entries
            .flatten()
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .collect();
        names.sort();
        for name in names {
            if !name.ends_with(".go") || name.ends_with("_test.go") {
                continue;
            }
            let file_path = source_dir.join(&name);
            let is_selected = relative == selected_dir_relative && name == selected_file_name;
            let bytes = if is_selected {
                source.to_vec()
            } else {
                match fs::read(&file_path) {
                    Ok(bytes) => bytes,
                    Err(_) => continue,
                }
            };
            for import in source_imports(&bytes) {
                let Ok(import) = core::str::from_utf8(import) else {
                    continue;
                };
                let same_module = format!("{module_path}/");
                if let Some(subpackage) = import.strip_prefix(&same_module) {
                    let subpackage = PathBuf::from(subpackage);
                    if !visited.contains(&subpackage) {
                        queue.push(subpackage);
                    }
                } else if import != module_path
                    && import
                        .split('/')
                        .next()
                        .is_some_and(|first| first.contains('.'))
                {
                    external.push(import.to_owned());
                }
            }
            if !is_selected {
                write_fixture_file(&fixture_root.join(&relative).join(&name), &bytes)?;
            }
        }
    }

    // Every external import whose module checkout exists in the corpus gains a
    // local `require`/`replace`, so a genuinely present sibling resolves.
    external.sort();
    external.dedup();
    let mut replacements: Vec<(String, String, PathBuf)> = Vec::new();
    if let Some(corpus_root) = corpus_root {
        for import in &external {
            if let Some((path, version, directory)) = corpus_module(corpus_root, import) {
                if !replacements.iter().any(|(known, _, _)| known == &path) {
                    replacements.push((path, version, directory));
                }
            }
        }
    }

    // A sibling checkout whose proxy snapshot legitimately omits `go.mod`
    // (pre-modules tags, pseudo-versions) cannot satisfy a `replace` as-is:
    // the go tool refuses the row with a module read error before any package
    // is loaded. Stage such siblings as fixture-local copies carrying a
    // synthesized minimal manifest, so the module system sees exactly the
    // module it already knows the coordinate to name.
    let mut replacement_targets: Vec<(String, String, PathBuf)> = Vec::new();
    for (path, version, directory) in replacements {
        let target = if directory.join("go.mod").is_file() {
            directory
        } else {
            stage_sibling_module(fixture_root, &directory, &path)?
        };
        replacement_targets.push((path, version, target));
    }

    let mut manifest = format!("module {module_path}\n\ngo {GO_FIXTURE_LANGUAGE_VERSION}\n");
    for (path, version, _) in &replacement_targets {
        manifest.push_str(&format!("\nrequire {path} {version}\n"));
    }
    for (path, _, directory) in &replacement_targets {
        manifest.push_str(&format!("replace {path} => {}\n", directory.display()));
    }
    fs::write(fixture_root.join("go.mod"), manifest.as_bytes())?;

    // The binding path always carries the audit's exact selected bytes, even
    // when the selected file itself is a `_test.go` sibling-excluded name.
    let source_path = fixture_root.join(&selected_relative);
    write_fixture_file(&source_path, source)?;
    Ok(StagedGoModule {
        root: fixture_root.to_owned(),
        source: source_path,
    })
}

fn write_fixture_file(path: &Path, bytes: &[u8]) -> Result<(), StagingError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

/// The fixture-local directory that holds copies of sibling corpus checkouts
/// staged only to carry a synthesized `go.mod`. The leading dot keeps the
/// directory out of every package walk (`…/...`, module scans), so the copies
/// stay invisible to package semantics and to the oracle image; they exist
/// only as `replace` targets.
const SIBLING_STAGING_DIR: &str = ".corpus-siblings";

/// Stages a writable copy of one sibling corpus checkout under `fixture_root`
/// and synthesizes its minimal `go.mod`.
///
/// Some proxy zips legitimately omit `go.mod`: pre-modules tags and
/// pseudo-versions snapshot a checkout that predates module files, so the
/// directory the corpus stores *is* the module at that version but carries no
/// manifest. A `replace` pointing straight at such a checkout dies with a
/// module read error before any package loads. This pass copies the checkout
/// under [`SIBLING_STAGING_DIR`] (never mutating the read-only corpus store)
/// and writes the minimal manifest the module system already knows:
/// `module <the pin coordinate's module path>` plus the parent fixture's
/// `go` directive ([`GO_FIXTURE_LANGUAGE_VERSION`]). Package sources are
/// copied verbatim; nothing else about the row changes — the oracle still
/// serializes exactly the selected package, and the copy's packages are
/// import context only.
fn stage_sibling_module(
    fixture_root: &Path,
    directory: &Path,
    module_path: &str,
) -> Result<PathBuf, StagingError> {
    let checkout_name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("sibling");
    let target = fixture_root.join(SIBLING_STAGING_DIR).join(checkout_name);
    copy_tree(directory, &target)?;
    let manifest = format!("module {module_path}\n\ngo {GO_FIXTURE_LANGUAGE_VERSION}\n");
    fs::write(target.join("go.mod"), manifest.as_bytes())?;
    Ok(target)
}

/// Copies `source`'s whole subtree to `target`, creating directories as
/// needed. Only sibling checkouts without a checked-in `go.mod` take this
/// path, and those snapshots are small (a handful of package files).
fn copy_tree(source: &Path, target: &Path) -> Result<(), StagingError> {
    fs::create_dir_all(target)?;
    let mut names: Vec<PathBuf> = fs::read_dir(source)?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|entry| entry.path())
        .collect();
    names.sort();
    for path in names {
        let destination = target.join(path.file_name().ok_or_else(|| {
            StagingError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "sibling checkout entry has no file name: {}",
                    path.display()
                ),
            ))
        })?);
        if path.is_dir() {
            copy_tree(&path, &destination)?;
        } else {
            fs::copy(&path, &destination)?;
        }
    }
    Ok(())
}

/// Reads the `module` directive from a `go.mod` byte buffer.
fn module_directive(bytes: &[u8]) -> Option<String> {
    let text = core::str::from_utf8(bytes).ok()?;
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("module") else {
            continue;
        };
        if !(rest.starts_with(char::is_whitespace) || rest.starts_with('"')) {
            continue;
        }
        let value = rest.trim().trim_matches('"').trim();
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

/// Derives a module path from a corpus-relative module directory when the
/// checkout carries no `go.mod` (for example the pre-modules `go-spew`).
fn module_path_from_corpus(module_root: &Path, corpus_root: Option<&Path>) -> Option<String> {
    let corpus = corpus_root?;
    let relative = module_root.strip_prefix(corpus).ok()?;
    let mut components = Vec::new();
    for component in relative.components() {
        components.push(component.as_os_str().to_str()?.to_owned());
    }
    let last = components.last_mut()?;
    if let Some((base, _version)) = last.split_once('@') {
        *last = base.to_owned();
    }
    Some(components.join("/"))
}

/// Resolves the longest corpus module prefix of one import path that exists as
/// a sibling module checkout, returning `(module_path, version, directory)`.
fn corpus_module(corpus_root: &Path, import_path: &str) -> Option<(String, String, PathBuf)> {
    let components: Vec<&str> = import_path.split('/').collect();
    for end in (1..=components.len()).rev() {
        let module_path = components[..end].join("/");
        if let Some(directory) = corpus_module_dir(corpus_root, &components[..end]) {
            let version = directory
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.split_once('@'))
                .map(|(_, version)| version.to_owned())
                .unwrap_or_default();
            return Some((module_path, version, directory));
        }
    }
    None
}

/// Locates `<corpus>/<module path>@<version>` by scanning the module path's
/// parent for a version-suffixed directory with the right base name.
fn corpus_module_dir(corpus_root: &Path, components: &[&str]) -> Option<PathBuf> {
    let (last, parents) = components.split_last()?;
    let mut parent = corpus_root.to_owned();
    for component in parents {
        parent.push(component);
    }
    let mut candidates: Vec<PathBuf> = fs::read_dir(&parent)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.split_once('@'))
                .is_some_and(|(base, _version)| base == *last)
        })
        .collect();
    candidates.sort();
    candidates.into_iter().next()
}

/// Reads one Go source's import path literals. `import` is a keyword, so the
/// scanner only has to skip comments and string/rune literals before reading
/// the keyword's spec list; a name token that merely begins with `import`
/// (`importing`) is not a declaration.
fn source_imports(bytes: &[u8]) -> Vec<&[u8]> {
    let n = bytes.len();
    let mut imports = Vec::new();
    let mut i = 0;
    while i < n {
        if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'/' {
            i += 2;
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if bytes[i] == b'/' && i + 1 < n && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        if bytes[i] == b'"' || bytes[i] == b'\'' || bytes[i] == b'`' {
            let quote = bytes[i];
            i += 1;
            while i < n {
                if bytes[i] == b'\\' && quote != b'`' {
                    i = (i + 2).min(n);
                    continue;
                }
                if bytes[i] == quote {
                    i += 1;
                    break;
                }
                i += 1;
            }
            continue;
        }
        if bytes[i..].starts_with(b"import") {
            let before = i == 0 || !is_ident_byte(bytes[i - 1]);
            let after = i + 6;
            let after = after >= n || !is_ident_byte(bytes[after]);
            if before && after {
                i += 6;
                i = skip_space(bytes, i);
                let block = i < n && bytes[i] == b'(';
                if block {
                    i += 1;
                }
                loop {
                    i = skip_space(bytes, i);
                    if i >= n {
                        break;
                    }
                    if block && bytes[i] == b')' {
                        i += 1;
                        break;
                    }
                    if is_ident_byte(bytes[i]) || bytes[i] == b'.' {
                        while i < n && (is_ident_byte(bytes[i]) || bytes[i] == b'.') {
                            i += 1;
                        }
                        i = skip_space(bytes, i);
                    }
                    if i < n && (bytes[i] == b'"' || bytes[i] == b'`') {
                        let quote = bytes[i];
                        let start = i + 1;
                        i += 1;
                        while i < n {
                            if bytes[i] == b'\\' && quote != b'`' {
                                i += 2;
                                continue;
                            }
                            if bytes[i] == quote {
                                break;
                            }
                            i += 1;
                        }
                        imports.push(&bytes[start..i.min(n)]);
                        if i < n {
                            i += 1;
                        }
                    } else {
                        i += 1;
                    }
                    if !block {
                        break;
                    }
                }
                continue;
            }
        }
        i += 1;
    }
    imports
}

const fn is_ident_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn skip_space(bytes: &[u8], mut i: usize) -> usize {
    let n = bytes.len();
    loop {
        while i < n && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'/' {
            i += 2;
            while i < n && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if i + 1 < n && bytes[i] == b'/' && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < n && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                i += 1;
            }
            i = (i + 2).min(n);
            continue;
        }
        return i;
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{
        GO_FIXTURE_LANGUAGE_VERSION, SIBLING_STAGING_DIR, module_directive, source_imports,
        stage_module_with_corpus,
    };

    fn imports(source: &str) -> Vec<String> {
        source_imports(source.as_bytes())
            .into_iter()
            .map(|import| String::from_utf8(import.to_vec()).expect("utf-8 import"))
            .collect()
    }

    /// A unique writable root for one test's fixture or corpus tree.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "nudox-go-staging-{label}-{}-{}",
                std::process::id(),
                NEXT_TEMP.with(|next| {
                    let value = next.get();
                    next.set(value + 1);
                    value
                }),
            ));
            fs::create_dir_all(&path).expect("create unique temporary directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn file(&self, relative: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create temporary parent");
            }
            fs::write(&path, bytes).expect("write temporary file");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            if let Err(error) = fs::remove_dir_all(&self.0) {
                if !std::thread::panicking() {
                    panic!("remove temporary directory: {error}");
                }
            }
        }
    }

    thread_local! {
        static NEXT_TEMP: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    /// A staged sibling directory without `go.mod` (the proxy-zip shape of
    /// `bytebufferpool@v1.0.0` and the two pseudo-versions) must stage
    /// successfully: the sibling becomes a fixture-local copy carrying the
    /// synthesized minimal manifest, and the fixture's `replace` targets the
    /// copy. The corpus checkout itself is never written.
    #[test]
    fn sibling_without_go_mod_is_staged_with_synthesized_manifest() {
        let corpus = TempDir::new("corpus-no-manifest");
        corpus.file(
            "example.com/sib@v1.0.0/sib.go",
            b"package sib\n\nconst Answer = 42\n",
        );
        corpus.file("example.com/sib@v1.0.0/LICENSE", b"sample license bytes\n");
        let root_module = TempDir::new("module-no-manifest");
        let selected = root_module.file(
            "example.com/root@v1.0.0/root.go",
            b"package root\n\nimport \"example.com/sib\"\n\nvar _ = sib.Answer\n",
        );
        let source = fs::read(&selected).expect("read selected source");
        let fixture = TempDir::new("fixture-no-manifest");

        let staged =
            stage_module_with_corpus(fixture.path(), &selected, &source, Some(corpus.path()))
                .expect("a go.mod-less sibling must not fail staging");

        let manifest = fs::read_to_string(staged.root.join("go.mod")).expect("fixture manifest");
        assert!(
            manifest.contains("require example.com/sib v1.0.0\n"),
            "the sibling must be required: {manifest}"
        );
        let copy = fixture.path().join(SIBLING_STAGING_DIR).join("sib@v1.0.0");
        assert!(
            manifest.contains(&format!("replace example.com/sib => {}", copy.display())),
            "the replace must target the fixture-local copy: {manifest}"
        );
        let synthesized =
            fs::read_to_string(copy.join("go.mod")).expect("synthesized sibling manifest");
        assert_eq!(
            synthesized,
            format!("module example.com/sib\n\ngo {GO_FIXTURE_LANGUAGE_VERSION}\n"),
            "the synthesized manifest names the coordinate's module path and the fixture go directive"
        );
        assert_eq!(
            fs::read(copy.join("sib.go")).expect("copied sibling source"),
            b"package sib\n\nconst Answer = 42\n",
            "the sibling's package sources are copied verbatim"
        );
        assert!(copy.join("LICENSE").is_file(), "non-Go files copy too");
        assert!(
            !corpus.path().join("example.com/sib@v1.0.0/go.mod").exists(),
            "the corpus checkout must never gain a manifest"
        );
    }

    /// A sibling directory that carries its own `go.mod` must be replaced in
    /// place exactly as before: no copy is staged and no manifest is
    /// synthesized.
    #[test]
    fn sibling_with_go_mod_is_replaced_in_place_without_synthesis() {
        let corpus = TempDir::new("corpus-manifest");
        corpus.file(
            "example.com/sib@v1.0.0/go.mod",
            b"module example.com/sib\n\ngo 1.19\n",
        );
        corpus.file(
            "example.com/sib@v1.0.0/sib.go",
            b"package sib\n\nconst Answer = 42\n",
        );
        let root_module = TempDir::new("module-manifest");
        let selected = root_module.file(
            "example.com/root@v1.0.0/root.go",
            b"package root\n\nimport \"example.com/sib\"\n\nvar _ = sib.Answer\n",
        );
        let source = fs::read(&selected).expect("read selected source");
        let fixture = TempDir::new("fixture-manifest");

        let staged =
            stage_module_with_corpus(fixture.path(), &selected, &source, Some(corpus.path()))
                .expect("a checked-in sibling manifest stages in place");

        let manifest = fs::read_to_string(staged.root.join("go.mod")).expect("fixture manifest");
        assert!(
            manifest.contains(&format!(
                "replace example.com/sib => {}",
                corpus.path().join("example.com/sib@v1.0.0").display()
            )),
            "a checked-in manifest is replaced in place: {manifest}"
        );
        assert!(
            !fixture.path().join(SIBLING_STAGING_DIR).exists(),
            "no sibling copy is staged when the checkout carries go.mod"
        );
        assert_eq!(
            fs::read(corpus.path().join("example.com/sib@v1.0.0/go.mod"))
                .expect("corpus manifest untouched"),
            b"module example.com/sib\n\ngo 1.19\n",
        );
    }

    #[test]
    fn single_and_grouped_imports_are_read() {
        let source = "package p\n\nimport \"fmt\"\n\nimport (\n\t\"os\"\n\talias \"net/http\"\n\t_ \"embed\"\n)\n";
        assert_eq!(imports(source), vec!["fmt", "os", "net/http", "embed"]);
    }

    #[test]
    fn comments_literals_and_identifiers_do_not_yield_imports() {
        let source = "package p\n\n// import \"fake\"\nvar importing = \"not/an/import\"\nvar doc = `import \"raw\"`\n/* import \"block\" */\nfunc f() { s := \"a/b\"; _ = s }\n";
        assert!(imports(source).is_empty());
    }

    #[test]
    fn raw_string_import_is_read() {
        assert_eq!(imports("package p\nimport `net/url`\n"), vec!["net/url"]);
    }

    #[test]
    fn module_directive_handles_quotes_and_ignores_other_lines() {
        assert_eq!(
            module_directive(b"module \"rsc.io/quote\"\n\nrequire x v1\n").as_deref(),
            Some("rsc.io/quote")
        );
        assert_eq!(
            module_directive(b"// module fake\nmodule example.com/real\n").as_deref(),
            Some("example.com/real")
        );
        assert_eq!(module_directive(b"modulepath\n"), None);
    }
}
