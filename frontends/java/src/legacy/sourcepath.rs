use std::env;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

const CONVENTIONAL_CHILDREN: [&str; 2] = ["src/main/java", "src/main/generated"];

/// Environment variable naming the fleet Java corpus root, mirroring the Go
/// frontend's `$NUDOX_GO_CORPUS_DIR` staging contract.
pub const CORPUS_ENV: &str = "NUDOX_JAVA_CORPUS_DIR";

/// One discovered corpus version root.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CorpusSourceRoot {
    /// The canonicalized `…/<group-path>/<artifact>/<version>` directory.
    pub path: PathBuf,
    /// True when the root carries a top-level `module-info.java`, so javac
    /// treats it as a JPMS module root. Such roots are admitted to module-mode
    /// discovery but excluded from unnamed-module source paths: a
    /// `module-info.java` anywhere on an unnamed compilation's source path
    /// makes javac resolve that module's `requires` clauses and fail before
    /// any type resolution.
    pub module: bool,
}

/// Discovers every Maven corpus version root under `corpus_root`.
///
/// The corpus layout is `$NUDOX_JAVA_CORPUS_DIR/<group-path>/<artifact>/<version>`
/// where the group path is one or more directory components and every version
/// directory name starts with an ASCII digit or carries a dot (the same
/// layout model the audit's same-group enumeration uses when it walks
/// `<group>/<artifact>/<version>` from a package root). A directory is a
/// corpus root when its name is a version component and its subtree contains
/// Java sources; discovery admits it and never descends further, so nested
/// version-looking names cannot produce duplicate roots. Roots carrying a
/// top-level `module-info.java` are reported with `module = true`.
///
/// Ordering policy: traversal is pre-order with lexicographically sorted
/// entries, except that roots sharing one group/artifact parent — several
/// versions of the same `group:artifact` — are ordered version-descending
/// (newest first; see [`prefer_newest_version_among_duplicates`]). Every
/// other root keeps the deterministic sorted pre-order. Consumers that hand
/// the roots to javac as a source path therefore resolve a duplicated type
/// from the newest corpus sources first, matching how the module system
/// would prefer a newer artifact version over a stale duplicate.
pub fn discover_corpus_roots(corpus_root: &Path) -> io::Result<Vec<CorpusSourceRoot>> {
    let root = fs::canonicalize(corpus_root)?;
    let mut discovered = Vec::new();
    visit_corpus_dir(&root, &root, &mut discovered)?;
    prefer_newest_version_among_duplicates(&mut discovered);
    Ok(discovered)
}

/// Reorders corpus roots so same-artifact duplicates run newest-first.
///
/// The sorted pre-order walk admits the version roots of one artifact
/// directory consecutively and ascending (`2.18.0` before `2.26.1`). javac
/// resolves a type through the source path's *first* match, so that
/// alphabetical order silently pins every duplicated type to the oldest
/// corpus sources: `equalsverifier`'s closure, for example, must see
/// `error_prone_annotations@2.26.1`'s `@RestrictedApi` (whose `link()` has a
/// default), not `2.18.0`'s. This pass stable-sorts the discovered roots with
/// a comparator that orders only roots sharing the same group/artifact
/// parent, descending by [`version_order_key`]; every other pair compares
/// `Equal`, so the deterministic sorted pre-order is preserved everywhere
/// else.
fn prefer_newest_version_among_duplicates(roots: &mut [CorpusSourceRoot]) {
    roots.sort_by(
        |left, right| match (left.path.parent(), right.path.parent()) {
            (Some(left_parent), Some(right_parent)) if left_parent == right_parent => {
                version_order_key(right).cmp(&version_order_key(left))
            }
            _ => std::cmp::Ordering::Equal,
        },
    );
}

/// One version-order segment: a numeric run compares by value, anything else
/// lexicographically.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum VersionSegment {
    Numeric(u64),
    Text(String),
}

/// The version-order key of one version directory name, splitting on `.`
/// and `-` into numeric and text segments, so `2.26.1` orders after `2.18.0`
/// and `1.10.0` after `1.9.0` where plain lexicographic order would not.
fn version_order_key(root: &CorpusSourceRoot) -> Vec<VersionSegment> {
    let name = root
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.split(['.', '-'])
        .map(|segment| match segment.parse::<u64>() {
            Ok(value) => VersionSegment::Numeric(value),
            Err(_) => VersionSegment::Text(segment.to_owned()),
        })
        .collect()
}

/// Fleet sibling roots for one authority extraction.
///
/// `Some(root)` discovers under the given corpus root; `None` reads
/// [`CORPUS_ENV`] and yields an empty set when the variable is unset, so the
/// harness behavior degrades to the caller-provided roots only. Only
/// non-module roots are returned: unnamed-module authority compilations must
/// never see a `module-info.java` on their source path.
pub fn corpus_source_roots(corpus_root: Option<&Path>) -> io::Result<Vec<PathBuf>> {
    Ok(discovered_corpus_roots(corpus_root)?
        .into_iter()
        .filter(|root| !root.module)
        .map(|root| root.path)
        .collect())
}

/// One corpus root carrying a top-level `module-info.java`, together with the
/// JPMS module name parsed from that declaration. The declaration's bytes are
/// never rewritten; the parsed name only keys the `--module-source-path`
/// entry `name=path` javac resolves required modules from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModuleSourceRoot {
    /// The module name declared by the root's `module-info.java`.
    pub name: String,
    /// The canonicalized corpus version root.
    pub path: PathBuf,
}

/// Every nameable module root of the fleet corpus, deduplicated by module
/// name with the first discovery winner kept. Discovery orders same-artifact
/// duplicates newest-first (see [`discover_corpus_roots`]), so two versions
/// of one module yield the newest version's deterministic entry.
pub fn corpus_module_roots(corpus_root: Option<&Path>) -> io::Result<Vec<ModuleSourceRoot>> {
    let mut modules = Vec::new();
    for discovered in discovered_corpus_roots(corpus_root)? {
        if !discovered.module {
            continue;
        }
        let Ok(bytes) = fs::read(discovered.path.join("module-info.java")) else {
            continue;
        };
        let Some(name) = parse_module_name(&bytes) else {
            continue;
        };
        if modules
            .iter()
            .any(|known: &ModuleSourceRoot| known.name == name)
        {
            continue;
        }
        modules.push(ModuleSourceRoot {
            name,
            path: discovered.path,
        });
    }
    Ok(modules)
}

/// Resolves the effective corpus root: the explicit argument when given,
/// otherwise [`CORPUS_ENV`]. `None` (unset or empty) yields no corpus.
fn resolve_corpus_dir(corpus_root: Option<&Path>) -> io::Result<Option<PathBuf>> {
    let owned;
    let root = match corpus_root {
        Some(root) => root,
        None => {
            let Some(found) = env::var_os(CORPUS_ENV) else {
                return Ok(None);
            };
            if found.is_empty() {
                return Ok(None);
            }
            owned = PathBuf::from(found);
            owned.as_path()
        }
    };
    let canonical = match fs::canonicalize(root) {
        Ok(canonical) => canonical,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(canonical))
}

/// Every corpus version root (module and non-module) under the effective
/// corpus root.
pub fn discovered_corpus_roots(corpus_root: Option<&Path>) -> io::Result<Vec<CorpusSourceRoot>> {
    let Some(root) = resolve_corpus_dir(corpus_root)? else {
        return Ok(Vec::new());
    };
    discover_corpus_roots(&root)
}

/// Parses the module name declared by `module-info.java` bytes.
///
/// Comments and string or char literals are blanked, the directive prefix
/// before the first `{` is scanned, and the dotted identifier following the
/// `module` keyword (optionally after `open` and after annotations) is
/// returned. Anything else — including a declaration-less or unparseable
/// file — yields `None`.
pub fn parse_module_name(bytes: &[u8]) -> Option<String> {
    let source = blank_comments_and_literals(String::from_utf8_lossy(bytes).chars().collect());
    let mut index = 0;
    while index < source.len() {
        if !is_ident_start(source[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < source.len() && is_ident_part(source[index]) {
            index += 1;
        }
        let word: String = source[start..index].iter().collect();
        if word != "module" {
            continue;
        }
        let mut name = String::new();
        let mut cursor = index;
        loop {
            while cursor < source.len() && source[cursor].is_whitespace() {
                cursor += 1;
            }
            if cursor >= source.len() || !is_ident_start(source[cursor]) {
                break;
            }
            let begin = cursor;
            while cursor < source.len() && is_ident_part(source[cursor]) {
                cursor += 1;
            }
            if !name.is_empty() {
                name.push('.');
            }
            name.extend(source[begin..cursor].iter());
            while cursor < source.len() && source[cursor].is_whitespace() {
                cursor += 1;
            }
            if cursor < source.len() && source[cursor] == '.' {
                cursor += 1;
                continue;
            }
            break;
        }
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}

fn is_ident_start(character: char) -> bool {
    character.is_alphabetic() || character == '_' || character == '$'
}

fn is_ident_part(character: char) -> bool {
    is_ident_start(character) || character.is_numeric()
}

/// Blanks comment interiors and string or char literal contents so keyword
/// scanning cannot be fooled by documentation or annotation text.
fn blank_comments_and_literals(source: Vec<char>) -> Vec<char> {
    let mut blanked = Vec::with_capacity(source.len());
    let mut index = 0;
    while index < source.len() {
        let character = source[index];
        let next = source.get(index + 1).copied();
        if character == '/' && next == Some('/') {
            while index < source.len() && source[index] != '\n' {
                blanked.push(' ');
                index += 1;
            }
        } else if character == '/' && next == Some('*') {
            blanked.push(' ');
            blanked.push(' ');
            index += 2;
            while index < source.len() {
                if source[index] == '*' && source.get(index + 1) == Some(&'/') {
                    blanked.push(' ');
                    blanked.push(' ');
                    index += 2;
                    break;
                }
                blanked.push(if source[index] == '\n' { '\n' } else { ' ' });
                index += 1;
            }
        } else if character == '"' || character == '\'' {
            let quote = character;
            blanked.push(' ');
            index += 1;
            while index < source.len() {
                if source[index] == '\\' {
                    blanked.push(' ');
                    blanked.push(' ');
                    index += 2;
                    continue;
                }
                if source[index] == quote {
                    blanked.push(' ');
                    index += 1;
                    break;
                }
                blanked.push(if source[index] == '\n' { '\n' } else { ' ' });
                index += 1;
            }
        } else {
            blanked.push(character);
            index += 1;
        }
    }
    blanked
}

/// The final source path for one authority extraction: caller roots first
/// (canonicalized, conventional children expanded), then corpus fleet roots
/// (same-artifact duplicates newest-first; see [`discover_corpus_roots`]),
/// deduplicated by first occurrence so caller roots keep precedence.
pub fn merged_source_roots(
    caller_roots: &[&Path],
    corpus_root: Option<&Path>,
) -> io::Result<Vec<PathBuf>> {
    let mut merged = discover_source_roots(caller_roots)?;
    for corpus in corpus_source_roots(corpus_root)? {
        if !merged.contains(&corpus) {
            merged.push(corpus);
        }
    }
    Ok(merged)
}

/// Pre-order corpus walk. Admits version-component directories that carry
/// Java sources and never descends into version-looking names.
///
/// Two exceptions keep the walk honest:
///
/// * A version-looking directory whose Java sources live only under
///   version-looking children is a dotted Maven *artifact* directory
///   (`org/osgi/org.osgi.framework/1.10.0`), not a version root — real Maven
///   artifact names do carry dots. Admitting such a directory yields a
///   useless root (its sources sit below the version level) and prunes the
///   real roots, so the walk instead descends into the version directories
///   beneath it.
/// * The corpus root itself is never a candidate root: a store path whose
///   final component happens to look like a version (`419880wb…-corpus`)
///   would otherwise be admitted wholesale and prune every real root.
fn visit_corpus_dir(
    root: &Path,
    dir: &Path,
    discovered: &mut Vec<CorpusSourceRoot>,
) -> io::Result<()> {
    let name = dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned());
    if dir != root && name.as_deref().is_some_and(is_version_component) {
        if dir_contains_java_outside_versions(dir)? {
            discovered.push(CorpusSourceRoot {
                path: dir.to_path_buf(),
                module: dir.join("module-info.java").is_file(),
            });
            return Ok(());
        }
        // Fall through: keep walking so the version roots below this dotted
        // artifact directory are discovered.
    }
    if dir != root && dir.join("module-info.java").is_file() {
        discovered.push(CorpusSourceRoot {
            path: dir.to_path_buf(),
            module: true,
        });
        return Ok(());
    }
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries
        .iter()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
    {
        visit_corpus_dir(root, &entry.path(), discovered)?;
    }
    Ok(())
}

/// True when `dir` holds a `*.java` file that is not confined to
/// version-looking child directories. Files directly below `dir` and trees
/// under non-version children count; a version-looking child owns its own
/// candidate root and is ignored here.
fn dir_contains_java_outside_versions(dir: &Path) -> io::Result<bool> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in &entries {
        let name = entry.file_name();
        if entry.file_type().is_ok_and(|t| t.is_file()) && name.to_string_lossy().ends_with(".java")
        {
            return Ok(true);
        }
    }
    for entry in entries
        .iter()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|entry| !is_version_component(entry.file_name().to_string_lossy().as_ref()))
    {
        if dir_contains_java(&entry.path())? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// True when `dir` or anything below it holds a `*.java` file.
fn dir_contains_java(dir: &Path) -> io::Result<bool> {
    let mut entries: Vec<fs::DirEntry> = fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in &entries {
        let name = entry.file_name();
        if entry.file_type().is_ok_and(|t| t.is_file()) && name.to_string_lossy().ends_with(".java")
        {
            return Ok(true);
        }
    }
    for entry in entries
        .iter()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
    {
        if dir_contains_java(&entry.path())? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A corpus version directory: starts with an ASCII digit or carries a dot.
/// Java package segments and artifact directory names never do.
fn is_version_component(name: &str) -> bool {
    name.starts_with(|first: char| first.is_ascii_digit()) || name.contains('.')
}

pub fn discover_source_roots(roots: &[&Path]) -> io::Result<Vec<PathBuf>> {
    let mut discovered = Vec::new();
    for root in roots {
        let canonical = match fs::canonicalize(root) {
            Ok(canonical) => canonical,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        admit_source_roots(&canonical, &mut discovered)?;
    }
    Ok(discovered)
}

pub fn discover_maven_sibling_roots(
    package_root: &Path,
    repository_root: &Path,
) -> io::Result<Vec<PathBuf>> {
    let repository = fs::canonicalize(repository_root)?;
    let package = fs::canonicalize(package_root)?;
    let relative = package
        .strip_prefix(&repository)
        .map_err(|_| escape_error(&package, &repository))?;
    let mut components: Vec<Component<'_>> = relative.components().collect();
    if components.len() < 3 {
        return Err(layout_error(&package));
    }
    let Some(version) = components.pop() else {
        return Err(layout_error(&package));
    };
    let Some(artifact) = components.pop() else {
        return Err(layout_error(&package));
    };
    let Some(artifact_dir) = package.parent() else {
        return Err(layout_error(&package));
    };
    let Some(group_dir) = artifact_dir.parent() else {
        return Err(layout_error(&package));
    };
    let version = version.as_os_str();
    let artifact = artifact.as_os_str();
    let mut entries = Vec::new();
    for entry in fs::read_dir(group_dir)? {
        entries.push(entry?);
    }
    entries.sort_by_key(fs::DirEntry::file_name);
    let mut own: Option<Vec<PathBuf>> = None;
    let mut siblings: Vec<Vec<PathBuf>> = Vec::new();
    for entry in entries {
        let candidate = entry.path().join(version);
        if !candidate.is_dir() {
            continue;
        }
        let canonical = fs::canonicalize(&candidate)?;
        if canonical.strip_prefix(&repository).is_err() {
            continue;
        }
        let mut admitted = Vec::new();
        admit_source_roots(&canonical, &mut admitted)?;
        if admitted.is_empty() {
            continue;
        }
        if entry.file_name() == artifact {
            own = Some(admitted);
        } else {
            siblings.push(admitted);
        }
    }
    let mut roots = own.unwrap_or_default();
    for sibling in siblings {
        roots.extend(sibling);
    }
    let mut unique = Vec::with_capacity(roots.len());
    for root in roots {
        push_unique(&mut unique, root);
    }
    Ok(unique)
}

fn admit_source_roots(root: &Path, discovered: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut expanded = false;
    for child in CONVENTIONAL_CHILDREN {
        let candidate = root.join(child);
        if !candidate.is_dir() {
            continue;
        }
        let canonical = fs::canonicalize(&candidate)?;
        push_unique(discovered, canonical);
        expanded = true;
    }
    if !expanded && root.is_dir() {
        push_unique(discovered, root.to_path_buf());
    }
    Ok(())
}

fn push_unique(discovered: &mut Vec<PathBuf>, root: PathBuf) {
    if !discovered.contains(&root) {
        discovered.push(root);
    }
}

fn escape_error(package: &Path, repository: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "package root {} escapes repository root {}",
            package.display(),
            repository.display()
        ),
    )
}

fn layout_error(package: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "package root {} is not a Maven group/artifact/version directory",
            package.display()
        ),
    )
}

#[cfg(test)]
mod sourcepath_tests {
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        CorpusSourceRoot, discover_corpus_roots, discover_maven_sibling_roots,
        discover_source_roots, merged_source_roots, parse_module_name,
    };

    struct TempDir(PathBuf);

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    impl TempDir {
        fn new(label: &str) -> Self {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let ordinal = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "nudox-java-sourcepath-{label}-{}-{ordinal}-{nanos}",
                std::process::id()
            ));
            fs::create_dir(&path).expect("create unique temporary directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn dir(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(&path).expect("create temporary layout");
            path
        }

        fn file(&self, relative: &str) -> PathBuf {
            let path = self.0.join(relative);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("create temporary parent");
            }
            fs::write(&path, []).expect("write temporary marker");
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

    fn canonical(path: &Path) -> PathBuf {
        fs::canonicalize(path).expect("canonicalize expected path")
    }

    #[test]
    fn discovers_flat_and_conventional_roots_in_caller_order_and_skips_missing() {
        let temp = TempDir::new("roots");
        let flat = temp.dir("flat");
        temp.file("flat/legacy-1.0-sources.jar");
        let java = temp.dir("project/src/main/java");
        let generated = temp.dir("project/src/main/generated");
        let missing = temp.path().join("absent");
        let discovered = discover_source_roots(&[&flat, &temp.path().join("project"), &missing])
            .expect("discover source roots");
        assert_eq!(
            discovered,
            vec![canonical(&flat), canonical(&java), canonical(&generated)]
        );
        let empty = discover_source_roots(&[&missing]).expect("missing roots are skipped");
        assert!(empty.is_empty());
    }

    #[test]
    fn deduplicates_repeated_and_nested_roots_preserving_first_occurrence() {
        let temp = TempDir::new("dedup");
        let java = temp.dir("project/src/main/java");
        let generated = temp.dir("project/src/main/generated");
        let project = temp.path().join("project");
        let dotted = temp.path().join(".").join("project");
        let nested_java = project.join("src").join("main").join("java");
        let discovered =
            discover_source_roots(&[&project, &nested_java, &dotted]).expect("discover roots");
        assert_eq!(discovered, vec![canonical(&java), canonical(&generated)]);
        let flat = temp.dir("flat");
        let dotted_flat = temp.path().join(".").join("flat");
        let discovered =
            discover_source_roots(&[&flat, &dotted_flat]).expect("discover flat roots twice");
        assert_eq!(discovered, vec![canonical(&flat)]);
    }

    #[test]
    fn flat_root_without_conventional_children_is_admitted_itself() {
        let temp = TempDir::new("flat");
        let flat = temp.dir("flat");
        temp.file("flat/Foo.java");
        let discovered = discover_source_roots(&[&flat]).expect("discover flat root");
        assert_eq!(discovered, vec![canonical(&flat)]);
    }

    #[test]
    fn discovers_group_siblings_only_at_exact_version() {
        let temp = TempDir::new("siblings");
        let alpha = temp.dir("com/example/alpha/1.0.0");
        temp.file("com/example/alpha/1.0.0/Alpha.java");
        let beta_java = temp.dir("com/example/beta/1.0.0/src/main/java");
        let gamma = temp.dir("com/example/gamma/1.0.0");
        temp.dir("com/example/beta/1.9.0");
        temp.dir("com/example/alpha/2.0.0");
        temp.dir("com/other/delta/1.0.0");
        temp.file("com/example/stray.txt");
        let package = temp.path().join("com/example/alpha/1.0.0");
        let discovered =
            discover_maven_sibling_roots(&package, temp.path()).expect("discover siblings");
        assert_eq!(
            discovered,
            vec![canonical(&alpha), canonical(&beta_java), canonical(&gamma)]
        );
    }

    #[test]
    fn sibling_without_version_directory_or_sources_is_skipped() {
        let temp = TempDir::new("empty-sibling");
        let alpha = temp.dir("g/a/1.0");
        temp.dir("g/b/2.0");
        temp.file("g/c/notes.txt");
        let package = temp.path().join("g/a/1.0");
        let discovered =
            discover_maven_sibling_roots(&package, temp.path()).expect("discover siblings");
        assert_eq!(discovered, vec![canonical(&alpha)]);
    }

    #[test]
    fn rejects_package_roots_outside_repository_boundary() {
        let temp = TempDir::new("boundary");
        let outside = TempDir::new("outside");
        let package = outside.dir("com/example/alpha/1.0.0");
        let error = discover_maven_sibling_roots(&package, temp.path())
            .expect_err("outside package root must not enter discovery");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let shallow = temp.dir("only-artifact");
        let error = discover_maven_sibling_roots(&shallow, temp.path())
            .expect_err("shallow layout is not a Maven version root");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        let missing = temp.path().join("absent-repository");
        let error = discover_maven_sibling_roots(&package, &missing)
            .expect_err("missing repository propagates");
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    }

    // Creating a symlink needs developer mode or elevation on Windows.
    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_package_root_that_resolves_outside_repository() {
        let temp = TempDir::new("symlink-escape");
        let outside = TempDir::new("symlink-target");
        let group = outside.dir("com/example");
        outside.dir("com/example/alpha/1.0.0");
        let link = temp.path().join("com");
        fs::create_dir_all(&link).expect("create group parent");
        symlink(&group, link.join("example")).expect("create escaping symlink");
        let package = link.join("example").join("alpha").join("1.0.0");
        let error = discover_maven_sibling_roots(&package, temp.path())
            .expect_err("symlinked ancestor escaping the repository is rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    // Creating a symlink needs developer mode or elevation on Windows.
    #[cfg(unix)]
    #[test]
    fn skips_sibling_symlinks_that_escape_the_repository() {
        let temp = TempDir::new("sibling-escape");
        let alpha = temp.dir("g/a/1.0");
        let outside = TempDir::new("sibling-target");
        let target = outside.dir("b/1.0");
        fs::create_dir_all(temp.path().join("g")).expect("create group directory");
        symlink(&target, temp.path().join("g").join("b")).expect("create escaping sibling");
        let package = temp.path().join("g/a/1.0");
        let discovered =
            discover_maven_sibling_roots(&package, temp.path()).expect("discover siblings");
        assert_eq!(discovered, vec![canonical(&alpha)]);
    }

    #[test]
    fn rejects_repository_equal_to_package_root() {
        let temp = TempDir::new("equal");
        let error = discover_maven_sibling_roots(temp.path(), temp.path())
            .expect_err("repository root alone is not a version root");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[test]
    fn corpus_discovery_admits_flat_deep_and_single_component_group_roots() {
        let temp = TempDir::new("corpus");
        temp.dir("commons-io/commons-io/2.15.1/org/example/flat");
        temp.file("commons-io/commons-io/2.15.1/org/example/flat/Flat.java");
        temp.dir("org/apache/commons/commons-text/1.11.0/org/apache/commons/text");
        temp.file("org/apache/commons/commons-text/1.11.0/org/apache/commons/text/T.java");
        temp.dir("io/vavr/vavr/0.10.4/io/vavr");
        temp.file("io/vavr/vavr/0.10.4/io/vavr/API.java");
        temp.dir("org/junit/platform/junit-platform-commons/1.10.1/org/junit/platform/commons");
        temp.file("org/junit/platform/junit-platform-commons/1.10.1/module-info.java");
        temp.file("org/junit/platform/junit-platform-commons/1.10.1/org/junit/platform/commons/Preconditions.java");
        temp.file("org/jetbrains/annotations/24.0.1/org/jetbrains/annotations/NotNull.java");
        temp.file("empty-version/1.0/notes.txt");
        temp.file("loose.java");
        let discovered = discover_corpus_roots(temp.path()).expect("discover corpus roots");
        let expected = [
            "commons-io/commons-io/2.15.1",
            "io/vavr/vavr/0.10.4",
            "org/apache/commons/commons-text/1.11.0",
            "org/jetbrains/annotations/24.0.1",
            "org/junit/platform/junit-platform-commons/1.10.1",
        ]
        .iter()
        .map(|relative| CorpusSourceRoot {
            path: canonical(&temp.path().join(relative)),
            module: relative.ends_with("junit-platform-commons/1.10.1"),
        })
        .collect::<Vec<_>>();
        assert_eq!(discovered, expected, "sorted pre-order with module flag");
    }

    #[test]
    fn corpus_discovery_prunes_below_admitted_roots_and_multiple_versions() {
        let temp = TempDir::new("corpus-versions");
        temp.dir("org/jspecify/jspecify/1.0.0/org/jspecify/annotations");
        temp.file("org/jspecify/jspecify/1.0.0/org/jspecify/annotations/NullMarked.java");
        temp.dir("org/jspecify/jspecify/2.0.10/org/jspecify/annotations");
        temp.file("org/jspecify/jspecify/2.0.10/org/jspecify/annotations/NullMarked.java");
        let discovered = discover_corpus_roots(temp.path()).expect("discover both versions");
        assert_eq!(
            discovered,
            [
                "org/jspecify/jspecify/2.0.10",
                "org/jspecify/jspecify/1.0.0"
            ]
            .iter()
            .map(|relative| CorpusSourceRoot {
                path: canonical(&temp.path().join(relative)),
                module: false,
            })
            .collect::<Vec<_>>(),
            "same-artifact versions run newest-first"
        );
    }

    #[test]
    fn duplicate_artifact_roots_order_newest_version_first() {
        let temp = TempDir::new("corpus-duplicate-versions");
        // error_prone_annotations-style pair: the oldest version's
        // `@RestrictedApi.link()` carries no default, so javac's first-match
        // source path resolution must see 2.26.1 first.
        temp.dir("com/google/errorprone/error_prone_annotations/2.18.0/com/google/errorprone/annotations");
        temp.file("com/google/errorprone/error_prone_annotations/2.18.0/com/google/errorprone/annotations/RestrictedApi.java");
        temp.dir("com/google/errorprone/error_prone_annotations/2.26.1/com/google/errorprone/annotations");
        temp.file("com/google/errorprone/error_prone_annotations/2.26.1/com/google/errorprone/annotations/RestrictedApi.java");
        // Numeric segments compare by value: 1.10.0 is newer than 1.9.0 even
        // though plain lexicographic order would put it first.
        temp.dir("org/example/one/1.10.0/org/example/one");
        temp.file("org/example/one/1.10.0/org/example/one/Newer.java");
        temp.dir("org/example/one/1.9.0/org/example/one");
        temp.file("org/example/one/1.9.0/org/example/one/Older.java");
        // A single-version artifact between the duplicates keeps its
        // lexicographic pre-order position.
        temp.dir("commons-io/commons-io/2.16.1/org/example/flat");
        temp.file("commons-io/commons-io/2.16.1/org/example/flat/Flat.java");
        let discovered = discover_corpus_roots(temp.path()).expect("discover duplicates");
        let corpus = canonical(temp.path());
        let relatives = discovered
            .iter()
            .map(|root| {
                root.path
                    .strip_prefix(&corpus)
                    .expect("corpus-relative root")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            relatives,
            vec![
                "com/google/errorprone/error_prone_annotations/2.26.1",
                "com/google/errorprone/error_prone_annotations/2.18.0",
                "commons-io/commons-io/2.16.1",
                "org/example/one/1.10.0",
                "org/example/one/1.9.0",
            ],
            "same-artifact duplicates run newest-first; everything else keeps sorted pre-order"
        );
    }

    #[test]
    fn corpus_discovery_descends_through_dotted_artifact_directories() {
        let temp = TempDir::new("corpus-dotted-artifacts");
        // A dotted Maven artifact directory carries only version children;
        // the version directories beneath it are the real roots.
        temp.dir("org/osgi/org.osgi.framework/1.10.0/org/osgi/framework");
        temp.file("org/osgi/org.osgi.framework/1.10.0/org/osgi/framework/Bundle.java");
        temp.dir("biz/aQute/bnd/biz.aQute.bnd.annotation/6.4.1/aQute/bnd/annotation/spi");
        temp.file("biz/aQute/bnd/biz.aQute.bnd.annotation/6.4.1/aQute/bnd/annotation/spi/ServiceProvider.java");
        // A dotted artifact whose version child carries a top-level
        // module-info.java is still discovered as a module root.
        temp.dir("javax/xml/bind/jaxb-api/2.3.1/javax/xml/bind");
        temp.file("javax/xml/bind/jaxb-api/2.3.1/module-info.java");
        temp.file("javax/xml/bind/jaxb-api/2.3.1/javax/xml/bind/JAXB.java");
        // A dotted artifact with no version children and no Java stays out.
        temp.dir("net/example/example.empty");
        let discovered = discover_corpus_roots(temp.path()).expect("discover through dots");
        assert_eq!(
            discovered,
            [
                "biz/aQute/bnd/biz.aQute.bnd.annotation/6.4.1",
                "javax/xml/bind/jaxb-api/2.3.1",
                "org/osgi/org.osgi.framework/1.10.0",
            ]
            .iter()
            .map(|relative| CorpusSourceRoot {
                path: canonical(&temp.path().join(relative)),
                module: relative.ends_with("jaxb-api/2.3.1"),
            })
            .collect::<Vec<_>>(),
            "dotted artifact directories never shadow their version roots"
        );
    }

    #[test]
    fn corpus_discovery_never_admits_a_version_looking_corpus_root() {
        let temp = TempDir::new("corpus-root-guard");
        // A corpus root whose final path component starts with a digit (a
        // content-addressed store path) must never be admitted as a version
        // root: doing so prunes every real root beneath it.
        let corpus = temp.dir("12345-fleet-corpus-java");
        temp.dir("12345-fleet-corpus-java/org/example/lib/1.0/org/example");
        temp.file("12345-fleet-corpus-java/org/example/lib/1.0/org/example/Lib.java");
        let discovered = discover_corpus_roots(&corpus).expect("discover under version-like root");
        assert_eq!(
            discovered,
            vec![CorpusSourceRoot {
                path: canonical(&corpus.join("org/example/lib/1.0")),
                module: false,
            }]
        );
    }

    #[test]
    fn merged_roots_keep_caller_precedence_and_exclude_module_roots() {
        let corpus = TempDir::new("corpus-merge");
        corpus.dir("org/testlib/testlib/1.0/org/testlib");
        corpus.file("org/testlib/testlib/1.0/org/testlib/Helper.java");
        corpus.dir("org/modlib/modlib/1.0/org/modlib");
        corpus.file("org/modlib/modlib/1.0/module-info.java");
        corpus.file("org/modlib/modlib/1.0/org/modlib/ModDep.java");
        let app = TempDir::new("app-root");
        app.file("demo/App.java");
        let app_root = app.path().to_owned();
        let merged = merged_source_roots(&[&app_root], Some(corpus.path()))
            .expect("merge caller and corpus roots");
        assert_eq!(
            merged,
            vec![
                canonical(&app_root),
                canonical(&corpus.path().join("org/testlib/testlib/1.0")),
            ]
        );
        assert!(
            !merged
                .iter()
                .any(|root| root.starts_with(corpus.path().join("org/modlib"))),
            "module roots must never enter an unnamed source path: {merged:?}"
        );
        let empty = merged_source_roots(&[], Some(app.path())).expect("corpus without versions");
        assert!(empty.is_empty());
    }

    #[test]
    fn parses_module_names_from_plain_annotated_and_open_declarations() {
        assert_eq!(
            parse_module_name(b"module org.example.lib {\n\texports org.example.lib;\n}\n"),
            Some(String::from("org.example.lib"))
        );
        assert_eq!(
            parse_module_name(b"open module org.example.open {\n}\n"),
            Some(String::from("org.example.open"))
        );
        assert_eq!(
            parse_module_name(
                b"// a comment mentioning module fake.decoy\n/* block module also.fake */\n@Deprecated\nmodule org.example.doc {\n}\n"
            ),
            Some(String::from("org.example.doc"))
        );
        assert_eq!(
            parse_module_name(
                b"@Deprecated(\"module not.a.decoy\")\nmodule org.example.strings {\n}\n"
            ),
            Some(String::from("org.example.strings"))
        );
    }

    #[test]
    fn module_name_parsing_refuses_declarationless_files() {
        assert_eq!(
            parse_module_name(b"package demo;\n\nfinal class X {}\n"),
            None
        );
        assert_eq!(parse_module_name(b""), None);
        assert_eq!(parse_module_name(b"module {\n}\n"), None);
        assert_eq!(
            parse_module_name(
                b"module org.example.lib {\n\trequires transitive org.example.dep;\n}\n"
            ),
            Some(String::from("org.example.lib")),
            "directives after the body brace never leak into the name"
        );
    }

    #[test]
    fn corpus_module_roots_deduplicate_by_module_name_keeping_newest_version() {
        let temp = TempDir::new("corpus-modules");
        temp.file("org/testlib/testlib.api/1.0.0/module-info.java");
        fs::write(
            temp.path()
                .join("org/testlib/testlib.api/1.0.0/module-info.java"),
            b"module org.testlib.api {\n\texports org.testlib.api;\n}\n",
        )
        .expect("write first module declaration");
        temp.dir("org/testlib/testlib.api/1.0.0/org/testlib/api");
        temp.file("org/testlib/testlib.api/2.0.0/module-info.java");
        fs::write(
            temp.path()
                .join("org/testlib/testlib.api/2.0.0/module-info.java"),
            b"module org.testlib.api {\n\texports org.testlib.api;\n}\n",
        )
        .expect("write second module declaration");
        temp.dir("org/testlib/testlib.api/2.0.0/org/testlib/api");
        temp.file("org/plain/plain/1.0.0/org/plain/Plain.java");
        temp.file("org/unnamed/unnamed/1.0.0/module-info.java");
        fs::write(
            temp.path()
                .join("org/unnamed/unnamed/1.0.0/module-info.java"),
            b"// no declaration here\nfinal class Junk {}\n",
        )
        .expect("write unparseable declaration");
        let modules = super::corpus_module_roots(Some(temp.path())).expect("module corpus roots");
        assert_eq!(
            modules,
            vec![super::ModuleSourceRoot {
                name: String::from("org.testlib.api"),
                path: canonical(&temp.path().join("org/testlib/testlib.api/2.0.0")),
            }],
            "one entry per module name, newest version wins, nameless roots skipped"
        );
    }
}
