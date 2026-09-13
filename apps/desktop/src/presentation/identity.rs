//! Readable identity parsed from the one coordinate string a row carries.
//! A coordinate is `project::path:line::symbol::path`, and every part of it is
//! separately navigable. Parsing is total: an unparseable label still yields an
//! identity that displays and copies exactly what the producer wrote.
//!
//! Nothing downstream may re-derive identity from a label; it asks an
//! [`Identity`] instead. That keeps one parser honest about the awkward cases —
//! Windows drive letters, paths containing `::`, package rows with no symbol at
//! all — rather than scattering nine slightly different `split("::")` calls
//! through the view layer.

use backend_library::{RowId, SymbolKey, encode_id};

/// Number of hexadecimal characters shown in a key tag.
const KEY_TAG_CHARS: usize = 8;

/// The shape a coordinate turned out to have.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Shape {
    /// A whole project or package: no file, no symbol.
    Package,
    /// A source file inside a project.
    File,
    /// A declaration inside a source file.
    Declaration,
    /// The producer's spelling did not parse; the whole label is the name.
    Opaque,
}

/// One readable coordinate.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Identity {
    coordinate: String,
    project: String,
    project_name: String,
    path: Option<String>,
    line: Option<u32>,
    symbol: Vec<String>,
    shape: Shape,
}

impl Identity {
    /// Parses the coordinate a row carries in its label.
    pub(crate) fn parse(coordinate: &str) -> Self {
        let trimmed = coordinate.trim();
        if trimmed.is_empty() {
            return Self::opaque(coordinate);
        }
        let mut parts = trimmed.split("::");
        let Some(project) = parts.next() else {
            return Self::opaque(coordinate);
        };
        let rest: Vec<&str> = parts.collect();
        let project_name = project_name_of(project);
        match rest.split_first() {
            None => Self::package(coordinate, project, project_name),
            Some((first, symbol)) => {
                let (path, line) = split_line(first);
                Self {
                    coordinate: coordinate.to_owned(),
                    project: project.to_owned(),
                    project_name,
                    path: Some(path),
                    line,
                    symbol: symbol.iter().map(|part| (*part).to_owned()).collect(),
                    shape: if symbol.is_empty() {
                        Shape::File
                    } else {
                        Shape::Declaration
                    },
                }
            }
        }
    }

    fn package(coordinate: &str, project: &str, project_name: String) -> Self {
        Self {
            coordinate: coordinate.to_owned(),
            project: project.to_owned(),
            project_name,
            path: None,
            line: None,
            symbol: Vec::new(),
            shape: Shape::Package,
        }
    }

    fn opaque(coordinate: &str) -> Self {
        Self {
            coordinate: coordinate.to_owned(),
            project: coordinate.to_owned(),
            project_name: coordinate.to_owned(),
            path: None,
            line: None,
            symbol: Vec::new(),
            shape: Shape::Opaque,
        }
    }

    /// Returns the exact producer spelling. This is what gets copied.
    pub(crate) fn coordinate(&self) -> &str {
        &self.coordinate
    }

    /// Returns the project path or package reference.
    pub(crate) fn project(&self) -> &str {
        &self.project
    }

    /// Returns the last meaningful segment of the project path.
    pub(crate) fn project_name(&self) -> &str {
        &self.project_name
    }

    /// Returns the package-relative source path, when the row has one.
    pub(crate) fn path(&self) -> Option<&str> {
        self.path.as_deref()
    }

    /// Returns the one-based declaration line, when the row has one.
    pub(crate) const fn line(&self) -> Option<u32> {
        self.line
    }

    /// Returns the symbol path segments, outermost first.
    pub(crate) fn symbol(&self) -> &[String] {
        &self.symbol
    }

    /// Returns the display name: the innermost symbol, else the file, else the project.
    pub(crate) fn name(&self) -> &str {
        self.symbol
            .last()
            .map(String::as_str)
            .or(self.path.as_deref())
            .unwrap_or(&self.project_name)
    }

    /// Returns what kind of thing this coordinate names.
    pub(crate) const fn shape(&self) -> Shape {
        self.shape
    }

    /// Returns the crumb trail, project first.
    pub(crate) fn trail(&self) -> Vec<Crumb> {
        let mut trail = vec![Crumb::Project(self.project_name.clone())];
        if let Some(path) = &self.path {
            trail.push(Crumb::Path(path.clone()));
        }
        for (depth, segment) in self.symbol.iter().enumerate() {
            trail.push(Crumb::Symbol {
                label: segment.clone(),
                depth,
            });
        }
        trail
    }

    /// Returns the one-line spelling used in dense rows and tooltips.
    pub(crate) fn compact(&self) -> String {
        let mut text = String::new();
        if let Some(path) = &self.path {
            text.push_str(path);
            if !self.symbol.is_empty() {
                text.push_str(" · ");
            }
        }
        text.push_str(&self.symbol.join("::"));
        if text.is_empty() {
            text.push_str(&self.project_name);
        }
        text
    }

    /// Returns the source coordinate `path:line`, when both are known.
    pub(crate) fn source_site(&self) -> Option<String> {
        let path = self.path.as_ref()?;
        let line = self.line?;
        Some(format!("{path}:{line}"))
    }
}

/// One clickable step of a crumb trail.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Crumb {
    /// The project; opens the package page.
    Project(String),
    /// The source path; opens the source view.
    Path(String),
    /// One symbol segment; opens that declaration.
    Symbol {
        /// The segment text.
        label: String,
        /// Zero-based depth within the symbol path.
        depth: usize,
    },
}

impl Crumb {
    /// Returns the text drawn for this crumb.
    pub(crate) fn label(&self) -> &str {
        match self {
            Self::Project(text) | Self::Path(text) | Self::Symbol { label: text, .. } => text,
        }
    }
}

/// A short, copyable spelling of a stable row identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct KeyTag {
    short: String,
    full: String,
}

impl KeyTag {
    /// Derives a tag from a symbol key.
    pub(crate) fn of_symbol(symbol: SymbolKey) -> Self {
        Self::of_hex(encode_id(symbol.as_bytes()))
    }

    /// Derives a tag from any stable row identity.
    pub(crate) fn of_row(id: RowId) -> Self {
        Self::of_hex(id.stable_key())
    }

    fn of_hex(full: String) -> Self {
        let short = full
            .rsplit(':')
            .next()
            .unwrap_or(&full)
            .chars()
            .take(KEY_TAG_CHARS)
            .collect();
        Self { short, full }
    }

    /// Returns the eight-character tag drawn beside a page title.
    pub(crate) fn short(&self) -> &str {
        &self.short
    }

    /// Returns the complete key, shown on hover and used when copying.
    pub(crate) fn full(&self) -> &str {
        &self.full
    }
}

/// Splits a trailing `:line` off a path, tolerating Windows drive letters.
fn split_line(text: &str) -> (String, Option<u32>) {
    let Some((head, tail)) = text.rsplit_once(':') else {
        return (text.to_owned(), None);
    };
    if head.is_empty() || tail.is_empty() || !tail.bytes().all(|byte| byte.is_ascii_digit()) {
        return (text.to_owned(), None);
    }
    match tail.parse::<u32>() {
        Ok(line) => (head.to_owned(), Some(line)),
        Err(_) => (text.to_owned(), None),
    }
}

/// Returns the last non-empty segment of a project path or package reference.
fn project_name_of(project: &str) -> String {
    let cleaned = project.trim_end_matches(['/', '\\']);
    if let Some(rest) = cleaned.strip_prefix("pkg:") {
        return registry_name_of(rest);
    }
    cleaned
        .rsplit(['/', '\\'])
        .find(|segment| !segment.is_empty())
        .unwrap_or(cleaned)
        .to_owned()
}

/// Returns the bare package name inside a package-URL coordinate.
fn registry_name_of(rest: &str) -> String {
    let without_version = rest.split('@').next().unwrap_or(rest);
    without_version
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or(without_version)
        .to_owned()
}

/// Shortens a long path by eliding its middle, never its ends.
///
/// Truncation is display-only: [`Identity::coordinate`] keeps the complete
/// spelling, so a copied path is never the shortened one.
pub(crate) fn elide_middle(text: &str, budget: usize) -> String {
    let characters: Vec<char> = text.chars().collect();
    if characters.len() <= budget || budget < 6 {
        return text.to_owned();
    }
    let keep = budget.saturating_sub(1);
    let head = keep / 2;
    let tail = keep.saturating_sub(head);
    let mut out: String = characters.iter().take(head).collect();
    out.push('…');
    out.extend(characters.iter().skip(characters.len().saturating_sub(tail)));
    out
}
