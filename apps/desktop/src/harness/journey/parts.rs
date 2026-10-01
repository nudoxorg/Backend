//! Parts: named, parameterized fragments of steps and checks that every
//! journey reuses (`do install frontends/rust/fixtures/toml_pin`).
//!
//! A part file (`apps/desktop/journeys/parts/*.part`) holds one or more
//! parts. Each starts with a header line and runs to the next header:
//!
//! ```text
//! part install PROJECT:path
//! click "Add a folder" in reader
//! answer-picker {PROJECT}
//! check indexing
//!   text "{PROJECT.name}" in reader
//! ```
//!
//! Its body is journey lines (steps, `check` with indented asserts, `needs`,
//! `do` of other parts); `{NAME}` and `{NAME.field}` are replaced by the
//! bound arguments before a line is parsed, so a part's checks run wherever
//! it is used. Parameters are typed, and an argument is checked against its
//! type where it is written ([`ParamType`]). Expansion ([`super::plan`])
//! records every site a step came through, so an error names the use and
//! the part, and a part that uses itself (directly or through others) is a
//! cycle error naming the chain.

use super::script::{Site, Tok, repo_path, tokens};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// What a parameter accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ParamType {
    /// A path under the repository that exists (`frontends/rust/fixtures/toml_pin`).
    Path,
    /// Quoted text (`"toml Value"`).
    Text,
    /// One bare word (letters, digits, `_ . : / -`).
    Word,
    /// A version (`0.8.23`, `1.1.6+spec-1.1.0`).
    Version,
    /// A crate release in the local cargo cache (`anyhow@1.0.104`).
    Crate,
    /// Harness route words, the rest of the line (`symbol toml::Value view=code`).
    Route,
    /// A window size (`1440x900`).
    Size,
    /// A whole number.
    Int,
}

impl ParamType {
    fn parse(word: &str) -> Result<Self, String> {
        Ok(match word {
            "path" => Self::Path,
            "text" => Self::Text,
            "word" => Self::Word,
            "version" => Self::Version,
            "crate" => Self::Crate,
            "route" => Self::Route,
            "size" => Self::Size,
            "int" => Self::Int,
            other => {
                return Err(format!(
                    "`{other}` is not a parameter type (path, text, word, version, crate, route, size, int)"
                ));
            }
        })
    }

    /// The spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Text => "text",
            Self::Word => "word",
            Self::Version => "version",
            Self::Crate => "crate",
            Self::Route => "route",
            Self::Size => "size",
            Self::Int => "int",
        }
    }
}

/// One declared parameter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Param {
    /// Its name (`PROJECT`).
    pub name: String,
    /// Its type.
    pub ty: ParamType,
}

/// A crate release in the local cargo cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CrateRelease {
    /// `anyhow`.
    pub name: String,
    /// `1.0.104`.
    pub version: String,
}

impl CrateRelease {
    /// `anyhow-1.0.104`, the cache's directory and archive stem.
    #[must_use]
    pub fn stem(&self) -> String {
        format!("{}-{}", self.name, self.version)
    }
}

/// A bound argument, checked against its parameter's type.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Arg {
    /// As written, and where it is.
    Path {
        /// Repo-relative, as written.
        rel: String,
        /// Absolute, canonical.
        abs: PathBuf,
    },
    /// Quoted text.
    Text(String),
    /// A bare word.
    Word(String),
    /// A version.
    Version(String),
    /// A crate release.
    Crate(CrateRelease),
    /// Route words.
    Route(String),
    /// A window size.
    Size(u32, u32),
    /// A whole number.
    Int(u64),
}

impl Arg {
    /// The inputs this argument makes a state read (hashed into its key).
    #[must_use]
    pub fn input(&self) -> Option<Input> {
        match self {
            Self::Path { rel, abs } => Some(Input::Tree {
                rel: rel.clone(),
                abs: abs.clone(),
            }),
            Self::Crate(release) => Some(Input::Crate(release.clone())),
            _ => None,
        }
    }

    /// `{NAME}` and `{NAME.field}`: the words substituted into a part line.
    fn field(&self, field: Option<&str>) -> Result<String, String> {
        let unknown = |fields: &str| {
            Err(format!(
                "has no field `.{}` (fields: {fields})",
                field.unwrap_or_default()
            ))
        };
        match (self, field) {
            (Self::Path { rel, .. }, None) => Ok(rel.clone()),
            (Self::Path { abs, .. }, Some("abs")) => Ok(abs.display().to_string()),
            (Self::Path { abs, .. }, Some("name")) => Ok(abs
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default()),
            (Self::Path { abs, .. }, Some("package")) => cargo_package_field(abs, "name"),
            (Self::Path { abs, .. }, Some("version")) => cargo_package_field(abs, "version"),
            (Self::Path { abs, .. }, Some(field)) if field.starts_with("lock.") => {
                locked_version(abs, &field["lock.".len()..])
            }
            (Self::Path { .. }, Some(_)) => unknown("abs, name, package, version, lock.CRATE"),
            (Self::Text(text), None) => Ok(text.replace('\\', "\\\\").replace('"', "\\\"")),
            (Self::Text(_), Some(_)) => unknown("none"),
            (Self::Word(word) | Self::Version(word) | Self::Route(word), None) => Ok(word.clone()),
            (Self::Word(_) | Self::Version(_) | Self::Route(_), Some(_)) => unknown("none"),
            (Self::Crate(release), None) => Ok(format!("{}@{}", release.name, release.version)),
            (Self::Crate(release), Some("name")) => Ok(release.name.clone()),
            (Self::Crate(release), Some("version")) => Ok(release.version.clone()),
            (Self::Crate(release), Some("dir")) => Ok(release.stem()),
            (Self::Crate(release), Some("src")) => {
                registry_source(release).map(|path| path.display().to_string())
            }
            (Self::Crate(_), Some(_)) => unknown("name, version, dir, src"),
            (Self::Size(width, height), None) => Ok(format!("{width}x{height}")),
            (Self::Size(width, _), Some("w")) => Ok(width.to_string()),
            (Self::Size(_, height), Some("h")) => Ok(height.to_string()),
            (Self::Size(..), Some(_)) => unknown("w, h"),
            (Self::Int(value), None) => Ok(value.to_string()),
            (Self::Int(_), Some(_)) => unknown("none"),
        }
    }
}

/// Something a state reads from outside the harness: its bytes go into the
/// state's cache key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Input {
    /// A directory tree under the repository (a project a journey installs).
    Tree {
        /// As written.
        rel: String,
        /// Where.
        abs: PathBuf,
    },
    /// A crate release from the local cargo cache.
    Crate(CrateRelease),
}

/// `[package] name|version` of the Cargo project at `root`: expected words
/// come from the real source, never from memory.
fn cargo_package_field(root: &Path, key: &str) -> Result<String, String> {
    let manifest = root.join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest)
        .map_err(|error| format!("{}: {error}", manifest.display()))?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|error| format!("{}: {error}", manifest.display()))?;
    value
        .get("package")
        .and_then(|package| package.get(key))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("{} has no [package] {key}", manifest.display()))
}

/// The version of `name` that the Cargo project at `root` locks
/// (`{PROJECT.lock.toml}` → `0.8.23`): a dependency's expected words come
/// from the project's own `Cargo.lock`.
fn locked_version(root: &Path, name: &str) -> Result<String, String> {
    let lock = root.join("Cargo.lock");
    let text =
        std::fs::read_to_string(&lock).map_err(|error| format!("{}: {error}", lock.display()))?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|error| format!("{}: {error}", lock.display()))?;
    let versions = value
        .get("package")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter(|package| package.get("name").and_then(toml::Value::as_str) == Some(name))
        .filter_map(|package| package.get("version").and_then(toml::Value::as_str))
        .collect::<Vec<_>>();
    match versions.as_slice() {
        [one] => Ok((*one).to_owned()),
        [] => Err(format!("{} locks no `{name}`", lock.display())),
        many => Err(format!(
            "{} locks `{name}` {} times ({}): name the one you mean",
            lock.display(),
            many.len(),
            many.join(", ")
        )),
    }
}

/// The unpacked source of a release in the local cargo cache. Offline:
/// nothing is fetched.
///
/// # Errors
/// The release is not unpacked in the cache.
pub fn registry_source(release: &CrateRelease) -> Result<PathBuf, String> {
    super::super::registry_source(&release.stem())
}

/// A part: its name, parameters, where it is written, and its body lines.
#[derive(Clone, Debug)]
pub struct PartDef {
    /// Its name.
    pub name: String,
    /// Its parameters, in order.
    pub params: Vec<Param>,
    /// Its header line.
    pub site: Site,
    /// Its body: each line with its site, indentation kept.
    pub body: Vec<(Site, String)>,
}

/// Every part a journey may use, by name.
#[derive(Clone, Debug, Default)]
pub struct Parts {
    defs: BTreeMap<String, PartDef>,
}

impl Parts {
    /// The journeys' own parts directory.
    #[must_use]
    pub fn dir() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("journeys/parts")
    }

    /// Every `*.part` file in `dir` (none is fine).
    ///
    /// # Errors
    /// A file that does not read or parse, or one name defined twice.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let mut parts = Self::default();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(parts);
        };
        let mut files = entries
            .filter_map(|entry| Some(entry.ok()?.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "part")
            })
            .collect::<Vec<_>>();
        files.sort();
        for file in files {
            let text = std::fs::read_to_string(&file)
                .map_err(|error| format!("{}: {error}", file.display()))?;
            parts.add_file(&file, &text)?;
        }
        Ok(parts)
    }

    /// Adds the parts one file's text defines.
    ///
    /// # Errors
    /// A body line before the first header, a malformed header, or a name
    /// already defined (both sites are named).
    pub fn add_file(&mut self, file: &Path, text: &str) -> Result<(), String> {
        let mut current: Option<PartDef> = None;
        for (index, raw) in text.lines().enumerate() {
            let site = Site::new(file, index + 1);
            let trimmed = raw.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            if let Some(header) = raw.strip_prefix("part ") {
                if let Some(done) = current.take() {
                    self.define(done)?;
                }
                current = Some(
                    header_of(header, site.clone())
                        .map_err(|message| format!("{site}: {message}\n    {raw}"))?,
                );
                continue;
            }
            let Some(def) = current.as_mut() else {
                return Err(format!(
                    "{site}: a part file starts with `part NAME PARAM:type…`\n    {raw}"
                ));
            };
            def.body.push((site, raw.trim_end().to_owned()));
        }
        if let Some(done) = current {
            self.define(done)?;
        }
        Ok(())
    }

    fn define(&mut self, def: PartDef) -> Result<(), String> {
        if def.body.is_empty() {
            return Err(format!("{}: part `{}` has no body", def.site, def.name));
        }
        if let Some(first) = self.defs.get(&def.name) {
            return Err(format!(
                "{}: part `{}` is already defined at {}",
                def.site, def.name, first.site
            ));
        }
        self.defs.insert(def.name.clone(), def);
        Ok(())
    }

    /// The part named `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&PartDef> {
        self.defs.get(name)
    }

    /// Every part's name.
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.defs.keys().map(String::as_str)
    }
}

fn header_of(header: &str, site: Site) -> Result<PartDef, String> {
    let mut words = header.split_whitespace();
    let name = words
        .next()
        .ok_or("`part NAME PARAM:type…`: a part needs a name")?;
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "`{name}` is not a part name (letters, digits, `-`, `_`)"
        ));
    }
    let mut params: Vec<Param> = Vec::new();
    for word in words {
        let (param, ty) = word
            .split_once(':')
            .ok_or_else(|| format!("`{word}`: a parameter is NAME:type"))?;
        if param.is_empty()
            || !param
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            return Err(format!(
                "`{param}` is not a parameter name (upper case, digits, `_`)"
            ));
        }
        if params.iter().any(|seen| seen.name == param) {
            return Err(format!("`{param}` is declared twice"));
        }
        let ty = ParamType::parse(ty)?;
        if params
            .last()
            .is_some_and(|last| last.ty == ParamType::Route)
        {
            return Err(
                "a `route` parameter takes the rest of the line, so it must be last".to_owned(),
            );
        }
        params.push(Param {
            name: param.to_owned(),
            ty,
        });
    }
    Ok(PartDef {
        name: name.to_owned(),
        params,
        site,
        body: Vec::new(),
    })
}

fn word_ok(word: &str) -> bool {
    !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_.:/-*".contains(c))
}

/// Checks `raw` (the words after the part's name at the use site) against
/// the part's parameters.
///
/// # Errors
/// A missing, extra or mistyped argument, in words.
pub fn bind(def: &PartDef, raw: &str) -> Result<Vec<(String, Arg)>, String> {
    let toks = tokens(raw)?;
    let mut bound = Vec::with_capacity(def.params.len());
    let mut rest = toks.as_slice();
    for param in &def.params {
        let wrong = |got: &str| {
            format!(
                "part `{}` parameter {}:{} does not take {got}",
                def.name,
                param.name,
                param.ty.name()
            )
        };
        if param.ty == ParamType::Route {
            let words = rest
                .iter()
                .map(|tok| match tok {
                    Tok::Word(word) => Ok(word.clone()),
                    Tok::Str(text) => Err(wrong(&format!("the quoted \"{text}\""))),
                })
                .collect::<Result<Vec<_>, _>>()?;
            if words.is_empty() {
                return Err(format!(
                    "part `{}` needs {} (route words)",
                    def.name, param.name
                ));
            }
            let words = words.join(" ");
            super::super::route::parse(&words)
                .map_err(|error| format!("{}: {error}", wrong(&format!("`{words}`"))))?;
            bound.push((param.name.clone(), Arg::Route(words)));
            rest = &[];
            continue;
        }
        let Some((tok, tail)) = rest.split_first() else {
            return Err(format!(
                "part `{}` needs {} argument{} ({}); got {}",
                def.name,
                def.params.len(),
                if def.params.len() == 1 { "" } else { "s" },
                def.params
                    .iter()
                    .map(|param| format!("{}:{}", param.name, param.ty.name()))
                    .collect::<Vec<_>>()
                    .join(" "),
                bound.len()
            ));
        };
        rest = tail;
        let arg = match (param.ty, tok) {
            (ParamType::Text, Tok::Str(text)) => Arg::Text(text.clone()),
            (ParamType::Text, Tok::Word(word)) => {
                return Err(wrong(&format!("the bare word `{word}` (quote it)")));
            }
            (_, Tok::Str(text)) => return Err(wrong(&format!("the quoted \"{text}\""))),
            (ParamType::Path, Tok::Word(word)) => {
                let abs = repo_path(word)
                    .map_err(|error| format!("{}: {error}", wrong(&format!("`{word}`"))))?;
                Arg::Path {
                    rel: word.clone(),
                    abs,
                }
            }
            (ParamType::Word, Tok::Word(word)) if word_ok(word) => Arg::Word(word.clone()),
            (ParamType::Version, Tok::Word(word)) if version_ok(word) => Arg::Version(word.clone()),
            (ParamType::Crate, Tok::Word(word)) => {
                let (name, version) = word
                    .split_once('@')
                    .filter(|(name, version)| word_ok(name) && version_ok(version))
                    .ok_or_else(|| wrong(&format!("`{word}` (NAME@VERSION)")))?;
                Arg::Crate(CrateRelease {
                    name: name.to_owned(),
                    version: version.to_owned(),
                })
            }
            (ParamType::Size, Tok::Word(word)) => {
                let (width, height) = word
                    .split_once('x')
                    .and_then(|(width, height)| Some((width.parse().ok()?, height.parse().ok()?)))
                    .ok_or_else(|| wrong(&format!("`{word}` (WxH)")))?;
                Arg::Size(width, height)
            }
            (ParamType::Int, Tok::Word(word)) => {
                Arg::Int(word.parse().map_err(|_| wrong(&format!("`{word}`")))?)
            }
            (_, Tok::Word(word)) => return Err(wrong(&format!("`{word}`"))),
        };
        bound.push((param.name.clone(), arg));
    }
    if let Some(extra) = rest.first() {
        return Err(format!(
            "part `{}` takes {} argument{}; `{}` is one too many",
            def.name,
            def.params.len(),
            if def.params.len() == 1 { "" } else { "s" },
            match extra {
                Tok::Word(word) => word.clone(),
                Tok::Str(text) => format!("\"{text}\""),
            }
        ));
    }
    Ok(bound)
}

fn version_ok(word: &str) -> bool {
    word.chars().next().is_some_and(|c| c.is_ascii_digit())
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".+-".contains(c))
}

/// Replaces every `{NAME}` / `{NAME.field}` in `line` with its argument.
///
/// # Errors
/// A name no parameter has, a field its type has not, or an unclosed brace.
pub fn substitute(line: &str, bound: &[(String, Arg)]) -> Result<String, String> {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let close = after
            .find('}')
            .ok_or_else(|| "an unclosed `{`".to_owned())?;
        let reference = &after[..close];
        let (name, field) = match reference.split_once('.') {
            Some((name, field)) => (name, Some(field)),
            None => (reference, None),
        };
        let (_, arg) = bound
            .iter()
            .find(|(param, _)| param == name)
            .ok_or_else(|| {
                format!(
                    "`{{{reference}}}` names no parameter (have: {})",
                    bound
                        .iter()
                        .map(|(param, _)| param.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })?;
        out.push_str(
            &arg.field(field)
                .map_err(|error| format!("`{{{reference}}}` {error}"))?,
        );
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{Arg, CrateRelease, ParamType, Parts, bind, substitute};
    use std::path::Path;

    fn parts(text: &str) -> Parts {
        let mut parts = Parts::default();
        parts
            .add_file(Path::new("t.part"), text)
            .expect("parts parse");
        parts
    }

    #[test]
    fn headers_declare_typed_parameters_and_bodies_keep_their_sites() {
        let parts = parts(
            "# library\npart install PROJECT:path\nclick \"Add a folder\"\ncheck x\n  text \"{PROJECT.name}\"\n\npart search QUERY:text\nkey cmd-k\n",
        );
        let install = parts.get("install").expect("install");
        assert_eq!(install.params[0].ty, ParamType::Path);
        assert_eq!(install.site.line, 2);
        assert_eq!(install.body.len(), 3);
        assert_eq!(
            install.body[2].1, "  text \"{PROJECT.name}\"",
            "indentation is kept for asserts"
        );
        assert_eq!(install.body[2].0.line, 5);
        assert_eq!(parts.names().collect::<Vec<_>>(), ["install", "search"]);
    }

    #[test]
    fn malformed_headers_and_duplicates_are_refused_with_both_sites() {
        let mut parts = Parts::default();
        assert!(
            parts.add_file(Path::new("a.part"), "click x\n").is_err(),
            "a body before any header"
        );
        assert!(
            parts
                .add_file(Path::new("a.part"), "part x P:colour\nsettle\n")
                .expect_err("an unknown type")
                .contains("not a parameter type")
        );
        assert!(
            parts
                .add_file(Path::new("a.part"), "part x lower:word\nsettle\n")
                .is_err(),
            "parameter names are upper case"
        );
        assert!(
            parts
                .add_file(Path::new("a.part"), "part x R:route W:word\nsettle\n")
                .expect_err("route not last")
                .contains("must be last")
        );
        assert!(
            parts
                .add_file(Path::new("a.part"), "part x\n")
                .expect_err("no body")
                .contains("no body")
        );
        parts
            .add_file(Path::new("a.part"), "part x\nsettle\n")
            .expect("first");
        let error = parts
            .add_file(Path::new("b.part"), "part x\nsettle\n")
            .expect_err("defined twice");
        assert!(
            error.contains("b.part:1") && error.contains("a.part:1"),
            "{error}"
        );
    }

    #[test]
    fn arguments_are_checked_against_their_types_where_they_are_written() {
        let parts = parts(
            "part p PROJECT:path QUERY:text AT:version CRATE:crate SIZE:size N:int\nsettle\npart r TO:route\nsettle\n",
        );
        let p = parts.get("p").expect("p");
        let bound = bind(
            p,
            r#"frontends/rust/fixtures/toml_pin "toml Value" 0.5.11 anyhow@1.0.104 1440x900 3"#,
        )
        .expect("binds");
        assert!(
            matches!(&bound[0].1, Arg::Path { rel, abs } if rel == "frontends/rust/fixtures/toml_pin" && abs.is_absolute())
        );
        assert_eq!(bound[1].1, Arg::Text("toml Value".to_owned()));
        assert_eq!(
            bound[3].1,
            Arg::Crate(CrateRelease {
                name: "anyhow".to_owned(),
                version: "1.0.104".to_owned()
            })
        );
        assert_eq!(bound[4].1, Arg::Size(1440, 900));
        for (args, says) in [
            (r#"no/such/dir "q" 1 a@1 1x1 1"#, "PROJECT:path"),
            (
                r#"frontends/rust/fixtures/toml_pin bare 1 a@1 1x1 1"#,
                "quote it",
            ),
            (
                r#"frontends/rust/fixtures/toml_pin "q" v1 a@1 1x1 1"#,
                "AT:version",
            ),
            (
                r#"frontends/rust/fixtures/toml_pin "q" 1 anyhow 1x1 1"#,
                "NAME@VERSION",
            ),
            (
                r#"frontends/rust/fixtures/toml_pin "q" 1 a@1 wide 1"#,
                "WxH",
            ),
            (
                r#"frontends/rust/fixtures/toml_pin "q" 1 a@1 1x1"#,
                "needs 6 arguments",
            ),
            (
                r#"frontends/rust/fixtures/toml_pin "q" 1 a@1 1x1 1 more"#,
                "one too many",
            ),
        ] {
            let error = bind(p, args).expect_err(args);
            assert!(error.contains(says), "`{args}`: {error}");
        }
        let r = parts.get("r").expect("r");
        assert_eq!(
            bind(r, "symbol toml::Value view=code").expect("route")[0].1,
            Arg::Route("symbol toml::Value view=code".to_owned())
        );
        assert!(
            bind(r, "nowhere at all").is_err(),
            "route words are parsed where they are written"
        );
    }

    #[test]
    fn substitution_fills_fields_from_the_real_sources_and_escapes_text() {
        let parts = parts("part p PROJECT:path Q:text C:crate\nsettle\n");
        let bound = bind(
            parts.get("p").expect("p"),
            r#"frontends/rust/fixtures/toml_pin "say \"hi\"" toml@0.5.11"#,
        )
        .expect("binds");
        assert_eq!(
            substitute(
                r#"text "{PROJECT.package}" "{PROJECT.name}" "{Q}" "{C.name} {C.version}""#,
                &bound
            )
            .expect("fills"),
            r#"text "toml-pin-fixture" "toml_pin" "say \"hi\"" "toml 0.5.11""#,
            "the package name is read from toml_pin's own Cargo.toml"
        );
        assert_eq!(
            substitute(
                "toml {PROJECT.lock.toml}, serde {PROJECT.lock.serde}",
                &bound
            )
            .expect("locked"),
            "toml 0.8.23, serde 1.0.229",
            "read from toml_pin's Cargo.lock"
        );
        assert!(
            substitute("{PROJECT.lock.anyhow}", &bound)
                .expect_err("not locked")
                .contains("locks no `anyhow`")
        );
        assert!(
            substitute("{NOPE}", &bound)
                .expect_err("unknown")
                .contains("names no parameter")
        );
        assert!(
            substitute("{Q.name}", &bound)
                .expect_err("text has no fields")
                .contains("no field")
        );
        assert!(substitute("{Q", &bound).is_err());
    }
}
