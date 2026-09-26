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

mod corpus;

use corpus::stage_module_with_corpus;
#[cfg(test)]
use corpus::{SIBLING_STAGING_DIR, module_directive, source_imports};

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
