//! Import resolution for the impact code graph.
//!
//! The component graph follows imports from file to file. Which file a
//! specifier names is the compiler's decision (relative paths, tsconfig `paths`
//! and `baseUrl`, `extends` chains, directory indexes, extension rewriting), so
//! the project is resolved by the same resolver the native TypeScript program
//! uses, served the indexed file set and the project's own JSON configuration.
//! A specifier which should name project code but does not resolve is reported
//! with the reason; it is never silently dropped.
use backend_frontend_typescript::project_resolver::{
    ProjectModule, ProjectModuleResolver, looks_like_path_alias,
};
use super::impact_code::resolve_relative;
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

/// What an import specifier names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum Lookup {
    /// An indexed source file.
    File(String),
    /// A package outside the project; it cannot be a consumer of project code.
    Package,
    /// Project code which could not be located, with the reason.
    Unresolved(String),
}

pub(super) trait ImportLookup {
    fn lookup(&self, importer: &str, spec: &str) -> Lookup;
}

const CODE_EXTENSIONS: &[&str] = &["tsx", "ts", "jsx", "js", "mjs", "cjs", "mts", "cts"];

/// Extensions of files which are not code modules. Stylesheets have their own
/// links; assets are not part of the component graph.
const NON_CODE_EXTENSIONS: &[&str] = &[
    "css", "scss", "sass", "less", "styl", "svg", "png", "jpg", "jpeg", "gif", "webp", "avif", "ico", "bmp", "json",
    "woff", "woff2", "ttf", "otf", "eot", "mp3", "mp4", "webm", "ogg", "wav", "md", "mdx", "txt", "html", "vue",
    "svelte", "astro", "graphql", "gql", "sql", "wasm", "pdf", "csv", "yaml", "yml", "toml", "xml", "node",
];

/// Whether a specifier can name a TypeScript or JavaScript module. A bundler
/// `?query` or `#hash` (`./logo.svg?react`) is not part of the file name.
pub(super) fn is_code_specifier(spec: &str) -> bool {
    // A leading `#` is a subpath import (`#internal/x`), not a hash.
    let end = spec.char_indices().skip(1).find(|(_, c)| matches!(c, '?' | '#')).map_or(spec.len(), |(index, _)| index);
    let spec = &spec[..end];
    let name = spec.rsplit('/').next().unwrap_or(spec);
    match name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => !NON_CODE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()),
        _ => true,
    }
}

fn probe(files: &BTreeSet<String>, base: &str) -> Option<String> {
    if files.contains(base) {
        return Some(base.to_owned());
    }
    for extension in CODE_EXTENSIONS {
        let candidate = format!("{base}.{extension}");
        if files.contains(&candidate) {
            return Some(candidate);
        }
    }
    for extension in CODE_EXTENSIONS {
        let candidate = format!("{base}/index.{extension}");
        if files.contains(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// Relative specifiers only, probed the way bundlers do. It is the fallback
/// when the compiler cannot place a relative path, and the whole lookup when
/// no project directory is known.
pub(super) struct RelativeImports<'a>(pub &'a BTreeSet<String>);

impl ImportLookup for RelativeImports<'_> {
    fn lookup(&self, importer: &str, spec: &str) -> Lookup {
        if let Some(base) = resolve_relative(importer, spec) {
            return probe(self.0, &base)
                .map_or_else(|| Lookup::Unresolved("no indexed file matches this relative path".to_owned()), Lookup::File);
        }
        if spec.starts_with("./") || spec.starts_with("../") {
            return Lookup::Unresolved("the path leaves the project".to_owned());
        }
        if looks_like_path_alias(spec) {
            return Lookup::Unresolved("path aliases are resolved from the project's tsconfig, which was not read".to_owned());
        }
        Lookup::Package
    }
}

/// The compiler's resolution of the project, with the relative probe as a
/// fallback for relative paths only.
pub(super) struct CompilerImports<'a> {
    resolver: ProjectModuleResolver<'a>,
    relative: RelativeImports<'a>,
    memo: RefCell<BTreeMap<(String, String), Lookup>>,
}

impl<'a> CompilerImports<'a> {
    pub(super) fn new(
        files: &'a BTreeSet<String>,
        read: &'a backend_frontend_typescript::project_resolver::ProjectReader<'a>,
    ) -> Self {
        Self { resolver: ProjectModuleResolver::new(files, read), relative: RelativeImports(files), memo: RefCell::default() }
    }

    /// Project JSON the resolver read, as `(path, bytes)`. Complete only after
    /// the lookups it serves have been made.
    pub(super) fn files_read(&self) -> Vec<(String, Vec<u8>)> {
        self.resolver.files_read()
    }

    /// Configurations which could not be loaded, as `(path, reason)`.
    pub(super) fn configuration_errors(&self) -> Vec<(String, String)> {
        self.resolver.configuration_errors()
    }
}

impl ImportLookup for CompilerImports<'_> {
    fn lookup(&self, importer: &str, spec: &str) -> Lookup {
        let key = (importer.to_owned(), spec.to_owned());
        if let Some(known) = self.memo.borrow().get(&key) {
            return known.clone();
        }
        let answer = match self.resolver.resolve(importer, spec) {
            ProjectModule::File(path) => Lookup::File(path),
            ProjectModule::Package => Lookup::Package,
            ProjectModule::Unresolved(reason) => match self.relative.lookup(importer, spec) {
                found @ Lookup::File(_) => found,
                _ => Lookup::Unresolved(reason),
            },
        };
        self.memo.borrow_mut().insert(key, answer.clone());
        answer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assets_and_stylesheets_are_not_code_specifiers() {
        assert!(is_code_specifier("@/components/Button"));
        assert!(is_code_specifier("./Button.tsx"));
        assert!(is_code_specifier("./util.helpers"));
        assert!(!is_code_specifier("./card.module.css"));
        assert!(!is_code_specifier("@/assets/logo.svg"));
        assert!(!is_code_specifier("./data.json"));
        for config in ["./settings.yaml", "./settings.yml", "./Cargo.toml", "./feed.xml", "./addon.node"] {
            assert!(!is_code_specifier(config), "{config}");
        }
    }

    #[test]
    fn a_query_or_hash_does_not_hide_the_extension() {
        for asset in ["./logo.svg?react", "./a.css?inline", "./x.png?url", "./icons.svg#sprite", "@/a/b.css?inline&x=/y.ts"] {
            assert!(!is_code_specifier(asset), "{asset}");
        }
        assert!(is_code_specifier("./worker.ts?worker"));
        assert!(is_code_specifier("./util?v=1.css.map"));
        assert!(is_code_specifier("#internal/util"));
        assert!(is_code_specifier("#internal/util?x"));
        assert!(!is_code_specifier("#internal/theme.css"));
    }

    #[test]
    fn relative_lookup_probes_extensions_and_indexes() {
        let files: BTreeSet<String> = ["src/a.tsx", "src/ui/index.ts", "src/ui/Card.tsx"].map(str::to_owned).into();
        let lookup = RelativeImports(&files);
        assert_eq!(lookup.lookup("src/a.tsx", "./ui"), Lookup::File("src/ui/index.ts".to_owned()));
        assert_eq!(lookup.lookup("src/a.tsx", "./ui/Card"), Lookup::File("src/ui/Card.tsx".to_owned()));
        assert!(matches!(lookup.lookup("src/a.tsx", "./missing"), Lookup::Unresolved(_)));
        assert!(matches!(lookup.lookup("src/a.tsx", "@/ui/Card"), Lookup::Unresolved(_)));
        assert_eq!(lookup.lookup("src/a.tsx", "react"), Lookup::Package);
    }
}
