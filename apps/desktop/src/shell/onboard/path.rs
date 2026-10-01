//! What a person typed into the add-a-folder field, judged against the disk.
//!
//! Pure over the file system: no window, no entity. [`admit`] turns the text
//! into a folder the shelf can hold, or into the one refusal that names why
//! not, in the words the field says once under itself. [`complete`] lists the
//! folders the text could continue to (the way a path bar does), and
//! [`Completion::take`] continues the text with one of them.

use crate::core::LocalProjectId;
use std::path::{Path, PathBuf};

/// The most folders a completion lists: more than that is a longer prefix
/// away, and a list that scrolls is not a path bar.
pub(crate) const MOST_SUGGESTIONS: usize = 4;

/// The most directory entries one completion reads before it stops: a
/// folder of a hundred thousand names is a prefix problem, not a scan.
const MOST_ENTRIES: usize = 4_000;

/// What kind of project a folder looks like, by the file that says so.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Ecosystem {
    /// `Cargo.toml`.
    Rust,
    /// `package.json`.
    TypeScript,
    /// `go.mod`.
    Go,
    /// `pyproject.toml`.
    Python,
    /// `pom.xml`, `build.gradle` or `build.gradle.kts`.
    Java,
    /// `CMakeLists.txt`.
    Cxx,
}

impl Ecosystem {
    /// The manifests that name each ecosystem, with the file that names it.
    const MARKERS: [(&'static str, Self); 8] = [
        ("Cargo.toml", Self::Rust),
        ("package.json", Self::TypeScript),
        ("go.mod", Self::Go),
        ("pyproject.toml", Self::Python),
        ("pom.xml", Self::Java),
        ("build.gradle", Self::Java),
        ("build.gradle.kts", Self::Java),
        ("CMakeLists.txt", Self::Cxx),
    ];

    /// The ecosystem `folder` declares, with the file that declares it.
    pub(crate) fn of(folder: &Path) -> Option<(Self, &'static str)> {
        Self::MARKERS
            .iter()
            .find(|(marker, _)| folder.join(marker).is_file())
            .map(|(marker, ecosystem)| (*ecosystem, *marker))
    }

    /// The name a person calls it by.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Rust => "Rust",
            Self::TypeScript => "TypeScript",
            Self::Go => "Go",
            Self::Python => "Python",
            Self::Java => "Java",
            Self::Cxx => "C and C++",
        }
    }
}

/// A folder the text names.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Folder {
    /// The shelf's identity for it: the path as the operating system spells
    /// it (symlinks and `..` resolved), so one folder is one project however
    /// it was typed.
    pub(crate) id: LocalProjectId,
    /// The folder's own name.
    pub(crate) name: String,
    /// The manifest that makes it a project, when one does.
    pub(crate) project: Option<(Ecosystem, &'static str)>,
    /// Whether it is already on the shelf.
    pub(crate) on_shelf: bool,
}

/// Why the text names no folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Refusal {
    /// Nothing was typed.
    Empty,
    /// A path that starts nowhere in particular (`src/lib`).
    Relative,
    /// Nothing is there. `nearest` is the deepest folder on the path that
    /// does exist.
    Missing {
        /// The path as spelled, home expanded.
        path: PathBuf,
        /// The deepest existing ancestor.
        nearest: Option<PathBuf>,
    },
    /// A file, not a folder; `folder` is the one that holds it.
    File {
        /// The folder holding the file.
        folder: Option<PathBuf>,
    },
    /// The folder exists and Nudox may not read it.
    Unreadable(PathBuf),
    /// The path cannot be held as a project identity (an embedded NUL).
    Unspellable,
}

impl Refusal {
    /// The sentence the field says under itself, once.
    pub(crate) fn says(&self) -> String {
        match self {
            Self::Empty => "Enter the path of a folder.".to_owned(),
            Self::Relative => "Start the path at the root (/) or at your home folder (~).".to_owned(),
            Self::Missing { path, nearest: Some(near) } => {
                format!("There is no folder at {}. The nearest that exists is {}.", path.display(), near.display())
            }
            Self::Missing { path, nearest: None } => format!("There is no folder at {}.", path.display()),
            Self::File { folder: Some(folder) } => {
                format!("That is a file, not a folder. Its folder is {}.", folder.display())
            }
            Self::File { folder: None } => "That is a file, not a folder.".to_owned(),
            Self::Unreadable(path) => format!("Nudox cannot read {}. Check its permissions.", path.display()),
            Self::Unspellable => "That path cannot be used.".to_owned(),
        }
    }
}

/// The home folder `~` stands for.
#[must_use]
pub(crate) fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
}

/// The path a piece of text spells, as a person pastes it: trimmed, one pair
/// of quotes taken off, a `file://` link read as its path, backslash-escaped
/// spaces (a path pasted from a terminal) unescaped, and a leading `~`
/// standing for `home`.
///
/// # Errors
/// [`Refusal::Empty`] for nothing, [`Refusal::Relative`] for a path that is
/// not absolute once `~` is expanded.
pub(crate) fn spell(text: &str, home: Option<&Path>) -> Result<PathBuf, Refusal> {
    let mut text = text.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = text.strip_prefix(quote).and_then(|rest| rest.strip_suffix(quote)) {
            text = inner.trim();
        }
    }
    if text.is_empty() {
        return Err(Refusal::Empty);
    }
    let owned;
    if let Some(link) = text.strip_prefix("file://") {
        owned = unescape_link(link);
        text = &owned;
    } else if text.contains("\\ ") {
        owned = text.replace("\\ ", " ");
        text = &owned;
    }
    let path = match text {
        "~" => home.map(Path::to_path_buf),
        _ => match text.strip_prefix("~/") {
            Some(rest) => home.map(|home| home.join(rest)),
            None => Some(PathBuf::from(text)),
        },
    };
    path.filter(|path| path.is_absolute()).ok_or(Refusal::Relative)
}

/// The path of a `file://` link's tail: `%20` and the like decoded.
fn unescape_link(link: &str) -> String {
    let bytes = link.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|pair| std::str::from_utf8(pair).ok())
            .and_then(|pair| u8::from_str_radix(pair, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The folder `text` names, judged against the disk.
///
/// # Errors
/// The refusal that says why the text names no folder.
pub(crate) fn admit(text: &str, home: Option<&Path>, shelf: &[LocalProjectId]) -> Result<Folder, Refusal> {
    let spelled = spell(text, home)?;
    let metadata = match std::fs::metadata(&spelled) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return Err(Refusal::Unreadable(spelled)),
        Err(_) => {
            let nearest = spelled.ancestors().skip(1).find(|ancestor| ancestor.is_dir()).map(Path::to_path_buf);
            return Err(Refusal::Missing { path: spelled, nearest });
        }
    };
    if !metadata.is_dir() {
        return Err(Refusal::File { folder: spelled.parent().map(Path::to_path_buf) });
    }
    if std::fs::read_dir(&spelled).is_err() {
        return Err(Refusal::Unreadable(spelled));
    }
    let resolved = spelled.canonicalize().unwrap_or(spelled);
    let id = LocalProjectId::from_path(&resolved).map_err(|_| Refusal::Unspellable)?;
    Ok(Folder {
        name: resolved.file_name().map_or_else(|| resolved.display().to_string(), |name| name.to_string_lossy().into_owned()),
        project: Ecosystem::of(&resolved),
        on_shelf: shelf.contains(&id),
        id,
    })
}

/// One folder a completion offers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Suggestion {
    /// The folder's name.
    pub(crate) name: String,
    /// What it declares itself to be, when it does.
    pub(crate) ecosystem: Option<Ecosystem>,
}

/// The folders a piece of text could continue to.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Completion {
    /// How many characters of the text the suggestions replace (the name
    /// being typed after the last separator).
    pub(crate) typed: usize,
    /// The folders whose names continue what was typed, best first, at most
    /// [`MOST_SUGGESTIONS`].
    pub(crate) shown: Vec<Suggestion>,
    /// How many more there were.
    pub(crate) more: usize,
}

impl Completion {
    /// `text` without the name being typed: what stays when a suggestion
    /// replaces it.
    fn head(&self, text: &str) -> String {
        let keep = text.chars().count().saturating_sub(self.typed);
        text.chars().take(keep).collect()
    }

    /// `text` continued with the suggestion at `index`: the name being typed
    /// is replaced by the folder's, and a separator follows it, ready for
    /// the next name.
    pub(crate) fn take(&self, text: &str, index: usize) -> Option<String> {
        let suggestion = self.shown.get(index)?;
        Some(format!("{}{}/", self.head(text), suggestion.name))
    }

    /// The longest name every suggestion shares beyond what is typed (Tab
    /// with nothing chosen goes as far as it can without guessing), or
    /// `None` when that is no further than the text already goes.
    pub(crate) fn common(&self, text: &str) -> Option<String> {
        let first = self.shown.first()?;
        let mut prefix: Vec<char> = first.name.chars().collect();
        for other in &self.shown[1..] {
            let shared = prefix.iter().zip(other.name.chars()).take_while(|(a, b)| a.eq_ignore_ascii_case(b)).count();
            prefix.truncate(shared);
        }
        (prefix.len() > self.typed).then(|| format!("{}{}", self.head(text), prefix.into_iter().collect::<String>()))
    }
}

/// The folders `text` could continue to, read from the disk.
///
/// The name being typed is the part after the last `/`; the folders that
/// hold a name starting with it (hidden ones only when it starts with a dot)
/// are listed, project folders first, then by name.
pub(crate) fn complete(text: &str, home: Option<&Path>) -> Completion {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return Completion::default();
    }
    let (parent_text, typed) = match trimmed.rfind('/') {
        Some(at) => (&trimmed[..=at], &trimmed[at + 1..]),
        None => return Completion::default(),
    };
    let Ok(parent) = spell(parent_text, home) else {
        return Completion::default();
    };
    let Ok(entries) = std::fs::read_dir(&parent) else {
        return Completion::default();
    };
    let wanted = typed.to_lowercase();
    let hidden = typed.starts_with('.');
    let mut found: Vec<Suggestion> = entries
        .filter_map(Result::ok)
        .take(MOST_ENTRIES)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let matches = name.to_lowercase().starts_with(&wanted) && (hidden || !name.starts_with('.'));
            (matches && entry.path().is_dir()).then(|| Suggestion { ecosystem: Ecosystem::of(&entry.path()).map(|(kind, _)| kind), name })
        })
        .collect();
    found.sort_by(|a, b| {
        (a.ecosystem.is_none(), a.name.to_lowercase()).cmp(&(b.ecosystem.is_none(), b.name.to_lowercase()))
    });
    let more = found.len().saturating_sub(MOST_SUGGESTIONS);
    found.truncate(MOST_SUGGESTIONS);
    Completion { typed: typed.chars().count(), shown: found, more }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A private directory this test owns.
    fn scratch(tag: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default();
        let dir = std::env::temp_dir().join(format!("nudox-onboard-path-{tag}-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir.canonicalize().expect("canonical scratch dir")
    }

    #[test]
    fn a_pasted_path_is_read_the_way_a_person_meant_it() {
        let home = PathBuf::from("/home/ana");
        let cases = [
            ("  /srv/app  ", "/srv/app"),
            ("\"/srv/my app\"", "/srv/my app"),
            ("'/srv/my app'", "/srv/my app"),
            ("/srv/my\\ app", "/srv/my app"),
            ("file:///srv/my%20app", "/srv/my app"),
            ("~/code/x", "/home/ana/code/x"),
            ("~", "/home/ana"),
        ];
        for (typed, meant) in cases {
            assert_eq!(spell(typed, Some(&home)), Ok(PathBuf::from(meant)), "{typed:?}");
        }
        assert_eq!(spell("   ", Some(&home)), Err(Refusal::Empty));
        assert_eq!(spell("src/lib", Some(&home)), Err(Refusal::Relative));
        assert_eq!(spell("~/code", None), Err(Refusal::Relative), "no home, no tilde");
    }

    #[test]
    fn every_way_a_path_can_fail_says_which() {
        let dir = scratch("refusals");
        std::fs::write(dir.join("notes.txt"), "hi").expect("file");
        let missing = dir.join("nope").join("deeper");
        let cases = [
            (String::new(), "Enter the path of a folder.".to_owned()),
            ("code/app".to_owned(), "Start the path at the root (/) or at your home folder (~).".to_owned()),
            (
                missing.display().to_string(),
                format!("There is no folder at {}. The nearest that exists is {}.", missing.display(), dir.display()),
            ),
            (
                dir.join("notes.txt").display().to_string(),
                format!("That is a file, not a folder. Its folder is {}.", dir.display()),
            ),
        ];
        for (typed, says) in cases {
            let refusal = admit(&typed, None, &[]).expect_err(&typed);
            assert_eq!(refusal.says(), says, "{typed:?}");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_folder_is_one_project_however_it_was_typed_and_says_what_it_is() {
        let dir = scratch("folder");
        let app = dir.join("app");
        std::fs::create_dir_all(app.join("src")).expect("app");
        std::fs::write(app.join("Cargo.toml"), "[package]\nname='app'\n").expect("manifest");
        let plain = admit(&app.display().to_string(), None, &[]).expect("folder");
        let roundabout = admit(&format!("{}/src/..", app.display()), None, &[]).expect("folder");
        assert_eq!(plain.id, roundabout.id, "`..` and symlinks resolve to one identity");
        assert_eq!(plain.name, "app");
        assert_eq!(plain.project, Some((Ecosystem::Rust, "Cargo.toml")));
        assert!(!plain.on_shelf);
        let again = admit(&app.display().to_string(), None, std::slice::from_ref(&plain.id)).expect("folder");
        assert!(again.on_shelf, "a folder on the shelf says so");
        let bare = admit(&dir.join("app").join("src").display().to_string(), None, &[]).expect("folder");
        assert_eq!(bare.project, None, "a folder with no manifest is still a folder");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn completion_lists_the_folders_the_text_continues_to_projects_first() {
        let dir = scratch("complete");
        for name in ["zeta", "alpha", "Alps", ".hidden", "beta"] {
            std::fs::create_dir_all(dir.join(name)).expect("folder");
        }
        std::fs::write(dir.join("alpha.txt"), "not a folder").expect("file");
        std::fs::write(dir.join("zeta").join("go.mod"), "module zeta").expect("manifest");
        let listed = |typed: &str| complete(typed, None).shown.into_iter().map(|s| s.name).collect::<Vec<_>>();
        let base = format!("{}/", dir.display());
        assert_eq!(listed(&format!("{base}al")), ["alpha", "Alps"], "case-blind prefix, folders only, sorted");
        assert_eq!(listed(&base), ["zeta", "alpha", "Alps", "beta"], "a project folder leads; dot folders stay hidden");
        assert_eq!(listed(&format!("{base}.")), [".hidden"], "a leading dot asks for them");
        assert!(listed("no-separator").is_empty(), "with no `/` there is no parent to list");
        let alps = complete(&format!("{base}al"), None);
        assert_eq!(alps.take(&format!("{base}al"), 1), Some(format!("{base}Alps/")));
        assert_eq!(alps.common(&format!("{base}al")), Some(format!("{base}alp")), "Tab goes as far as alpha and Alps agree");
        assert_eq!(complete(&format!("{base}alp"), None).common(&format!("{base}alp")), None, "and no further than they agree");
        let zeta = complete(&format!("{base}ze"), None);
        assert_eq!(zeta.common(&format!("{base}ze")), Some(format!("{base}zeta")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn completion_is_bounded() {
        let dir = scratch("bounded");
        for index in 0..(MOST_SUGGESTIONS + 3) {
            std::fs::create_dir_all(dir.join(format!("p{index:02}"))).expect("folder");
        }
        let all = complete(&format!("{}/p", dir.display()), None);
        assert_eq!(all.shown.len(), MOST_SUGGESTIONS);
        assert_eq!(all.more, 3);
        std::fs::remove_dir_all(&dir).ok();
    }
}
