//! Discovers Java corpus, module, and Maven source roots.

use super::*;

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
/// [`super::CORPUS_ENV`] and yields an empty set when the variable is unset, so the
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
/// otherwise [`super::CORPUS_ENV`]. `None` (unset or empty) yields no corpus.
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
