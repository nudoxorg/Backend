//! Licenses as the marks read them: SPDX expressions, the family table
//! (transcribed from choosealicense.com's rules, the data behind GitHub's
//! license card), whether a license fits your project, and what it adds to
//! your tree.
//!
//! Plain words, not legal advice: every card that says any of this carries
//! [`HEDGE`] at its foot. When an expression offers a choice (`OR`) and the
//! lockfile does not record which one you took, the card says which one it
//! assumed ([`Fit::assumed`]).

use super::semver::{list, plural, thousands, word};

/// The one quiet line at the foot of every license card.
pub const HEDGE: &str = "a plain-words summary, not legal advice";

/// An SPDX expression.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Expr {
    /// One license (`MIT`, `Apache-2.0 WITH LLVM-exception`).
    Id(String),
    /// All of these apply.
    And(Vec<Expr>),
    /// Your choice of one.
    Or(Vec<Expr>),
}

/// Parses an SPDX expression (also the legacy `MIT/Apache-2.0` form). `AND`
/// binds tighter than `OR`. Empty input is `None`.
#[must_use]
pub fn parse(spdx: &str) -> Option<Expr> {
    let spaced = spdx
        .replace('/', " OR ")
        .replace('(', " ( ")
        .replace(')', " ) ");
    let toks: Vec<&str> = spaced.split_whitespace().collect();
    if toks.is_empty() {
        return None;
    }
    let mut i = 0;
    Some(or(&toks, &mut i))
}

fn atom(toks: &[&str], i: &mut usize) -> Expr {
    if toks.get(*i) == Some(&"(") {
        *i += 1;
        let e = or(toks, i);
        *i += 1;
        return e;
    }
    let mut id = toks.get(*i).copied().unwrap_or_default().to_owned();
    *i += 1;
    if toks.get(*i) == Some(&"WITH") {
        *i += 1;
        id = format!("{id} WITH {}", toks.get(*i).copied().unwrap_or_default());
        *i += 1;
    }
    Expr::Id(id)
}

fn and(toks: &[&str], i: &mut usize) -> Expr {
    let mut all = vec![atom(toks, i)];
    while toks.get(*i) == Some(&"AND") {
        *i += 1;
        all.push(atom(toks, i));
    }
    if all.len() == 1 {
        all.remove(0)
    } else {
        Expr::And(all)
    }
}

fn or(toks: &[&str], i: &mut usize) -> Expr {
    let mut any = vec![and(toks, i)];
    while toks.get(*i) == Some(&"OR") {
        *i += 1;
        any.push(and(toks, i));
    }
    if any.len() == 1 {
        any.remove(0)
    } else {
        Expr::Or(any)
    }
}

/// Every license id in the expression, in order.
#[must_use]
pub fn ids(e: &Expr) -> Vec<String> {
    match e {
        Expr::Id(id) => vec![id.clone()],
        Expr::And(all) | Expr::Or(all) => all.iter().flat_map(ids).collect(),
    }
}

/// The ways you can take it: each option is the ids that all apply.
#[must_use]
pub fn options(e: &Expr) -> Vec<Vec<String>> {
    match e {
        Expr::Id(id) => vec![vec![id.clone()]],
        Expr::Or(any) => any.iter().flat_map(options).collect(),
        Expr::And(all) => all.iter().map(options).fold(vec![Vec::new()], |acc, opts| {
            acc.iter()
                .flat_map(|a| {
                    opts.iter()
                        .map(move |b| a.iter().chain(b).cloned().collect())
                })
                .collect()
        }),
    }
}

/// An id without its `-only` / `-or-later` suffix and exception.
#[must_use]
pub fn short_id(id: &str) -> String {
    let id = id.split(" WITH ").next().unwrap_or(id);
    id.trim_end_matches("-only")
        .trim_end_matches("-or-later")
        .to_owned()
}

fn base_id(id: &str) -> &str {
    let id = id.trim_end_matches('+');
    id.strip_suffix("-only")
        .or_else(|| id.strip_suffix("-or-later"))
        .unwrap_or(id)
}

/// The mark's word: `MIT/Apache-2.0`, `MIT/Apache-2.0 +1`, `MPL-2.0`.
#[must_use]
pub fn rest_word(e: &Expr) -> String {
    match e {
        Expr::Id(id) => short_id(id),
        Expr::Or(any) => {
            let parts: Vec<String> = any.iter().map(part_word).collect();
            if parts.len() > 2 {
                format!("{} +{}", parts[..2].join("/"), parts.len() - 2)
            } else {
                parts.join("/")
            }
        }
        Expr::And(all) => {
            let parts: Vec<String> = all.iter().map(part_word).collect();
            format!("{} +{}", parts[0], parts.len() - 1)
        }
    }
}

fn part_word(e: &Expr) -> String {
    match e {
        Expr::Id(id) => short_id(id),
        Expr::Or(any) => any.iter().map(rest_word).collect::<Vec<_>>().join("/"),
        Expr::And(_) => rest_word(e),
    }
}

/// A license family: the ring's shape.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Family {
    /// No terms at all (CC0, Unlicense): a dotted ring.
    Public,
    /// Keep the notice (MIT, Apache, BSD): an open ring.
    Permissive,
    /// Share changes to its own files (MPL, LGPL, EPL): a ring with a core.
    Weak,
    /// Share everything you ship with it (GPL, AGPL): a closed ring.
    Strong,
    /// Terms nobody has read yet: a dashed ring.
    Unknown,
}

impl Family {
    /// The family in words (`weak copyleft`).
    #[must_use]
    pub const fn words(self) -> &'static str {
        match self {
            Self::Public => "public domain",
            Self::Permissive => "permissive",
            Self::Weak => "weak copyleft",
            Self::Strong => "strong copyleft",
            Self::Unknown => "unknown terms",
        }
    }

    /// The terms a tree carries of this family (`weak copyleft terms`).
    #[must_use]
    pub const fn terms(self) -> &'static str {
        match self {
            Self::Public => "no terms",
            Self::Permissive => "permissive terms",
            Self::Weak => "weak copyleft terms",
            Self::Strong => "strong copyleft terms",
            Self::Unknown => "custom terms",
        }
    }

    /// Reads a family name (`permissive`, `weak`, `strong`, `public`).
    #[must_use]
    pub fn of(name: &str) -> Self {
        match name {
            "public" => Self::Public,
            "permissive" => Self::Permissive,
            "weak" => Self::Weak,
            "strong" => Self::Strong,
            _ => Self::Unknown,
        }
    }
}

const COMMERCIAL: &str = "commercial use";
const MODIFY: &str = "modification";
const DISTRIBUTE: &str = "distribution";
const PATENT: &str = "patent use";
const PRIVATE: &str = "private use";
const NOTICE: &str = "keep the notice";
const CHANGES: &str = "mark your changes";
const SOURCE: &str = "share the source";
const SAME: &str = "same license";
const SAME_FILE: &str = "same license, per file";
const SAME_LIB: &str = "same license, for the library";
const NETWORK: &str = "share source over a network";
const LIABILITY: &str = "no liability";
const WARRANTY: &str = "no warranty";
const TRADEMARK: &str = "no trademark rights";
const NO_PATENT: &str = "no patent rights";

const BASE: &[&str] = &[COMMERCIAL, MODIFY, DISTRIBUTE, PRIVATE];
const BASE_PATENT: &[&str] = &[COMMERCIAL, MODIFY, DISTRIBUTE, PATENT, PRIVATE];
const NOTICE_ONLY: &[&str] = &[NOTICE];
const LW: &[&str] = &[LIABILITY, WARRANTY];

/// What a license lets you do, asks of you, and does not give you.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Terms {
    /// Its full name.
    pub name: &'static str,
    /// Its family.
    pub family: Family,
    /// Permissions.
    pub permissions: &'static [&'static str],
    /// Conditions.
    pub conditions: &'static [&'static str],
    /// Limitations.
    pub limitations: &'static [&'static str],
}

const fn t(
    name: &'static str,
    family: Family,
    permissions: &'static [&'static str],
    conditions: &'static [&'static str],
    limitations: &'static [&'static str],
) -> Terms {
    Terms {
        name,
        family,
        permissions,
        conditions,
        limitations,
    }
}

/// The family table entry for `id` (`None` for a license it does not know).
#[must_use]
pub fn terms(id: &str) -> Option<Terms> {
    use Family::{Permissive, Public, Strong, Weak};
    let exact = |id: &str| -> Option<Terms> {
        Some(match id {
            "MIT" => t("MIT License", Permissive, BASE, NOTICE_ONLY, LW),
            "MIT-0" => t("MIT No Attribution", Permissive, BASE, &[], LW),
            "Apache-2.0" => t(
                "Apache License 2.0",
                Permissive,
                BASE_PATENT,
                &[NOTICE, CHANGES],
                &[TRADEMARK, LIABILITY, WARRANTY],
            ),
            "BSD-1-Clause" => t("BSD 1-Clause", Permissive, BASE, NOTICE_ONLY, LW),
            "BSD-2-Clause" => t("BSD 2-Clause", Permissive, BASE, NOTICE_ONLY, LW),
            "BSD-3-Clause" => t("BSD 3-Clause", Permissive, BASE, NOTICE_ONLY, LW),
            "0BSD" => t("Zero-Clause BSD", Permissive, BASE, &[], LW),
            "ISC" => t("ISC License", Permissive, BASE, NOTICE_ONLY, LW),
            "Zlib" => t("zlib License", Permissive, BASE, &[NOTICE, CHANGES], LW),
            "zlib-acknowledgement" => t(
                "zlib/libpng with acknowledgement",
                Permissive,
                BASE,
                &[NOTICE, CHANGES],
                LW,
            ),
            "BSL-1.0" => t(
                "Boost Software License 1.0",
                Permissive,
                BASE,
                NOTICE_ONLY,
                LW,
            ),
            "Unicode-3.0" => t("Unicode License v3", Permissive, BASE, NOTICE_ONLY, LW),
            "Unicode-DFS-2016" => t("Unicode License 2016", Permissive, BASE, NOTICE_ONLY, LW),
            "NCSA" => t(
                "University of Illinois/NCSA",
                Permissive,
                BASE,
                NOTICE_ONLY,
                LW,
            ),
            "CDLA-Permissive-2.0" => t(
                "Community Data License, Permissive 2.0",
                Permissive,
                BASE,
                NOTICE_ONLY,
                LW,
            ),
            "MPL-2.0" => t(
                "Mozilla Public License 2.0",
                Weak,
                BASE_PATENT,
                &[SOURCE, NOTICE, SAME_FILE],
                &[TRADEMARK, LIABILITY, WARRANTY],
            ),
            "LGPL-2.1" => t(
                "GNU LGPL v2.1",
                Weak,
                BASE,
                &[SOURCE, NOTICE, SAME_LIB, CHANGES],
                LW,
            ),
            "LGPL-3.0" => t(
                "GNU LGPL v3.0",
                Weak,
                BASE_PATENT,
                &[SOURCE, NOTICE, SAME_LIB, CHANGES],
                LW,
            ),
            "EPL-2.0" => t(
                "Eclipse Public License 2.0",
                Weak,
                BASE_PATENT,
                &[SOURCE, NOTICE, SAME_FILE],
                &[TRADEMARK, LIABILITY, WARRANTY],
            ),
            "GPL-2.0" => t(
                "GNU GPL v2.0",
                Strong,
                BASE,
                &[SOURCE, NOTICE, SAME, CHANGES],
                LW,
            ),
            "GPL-3.0" => t(
                "GNU GPL v3.0",
                Strong,
                BASE_PATENT,
                &[SOURCE, NOTICE, SAME, CHANGES],
                LW,
            ),
            "AGPL-3.0" => t(
                "GNU AGPL v3.0",
                Strong,
                BASE_PATENT,
                &[SOURCE, NOTICE, SAME, CHANGES, NETWORK],
                LW,
            ),
            "Unlicense" => t("The Unlicense", Public, BASE, &[], LW),
            "CC0-1.0" => t(
                "Creative Commons Zero",
                Public,
                BASE,
                &[],
                &[LIABILITY, TRADEMARK, NO_PATENT, WARRANTY],
            ),
            _ => return None,
        })
    };
    exact(id).or_else(|| exact(base_id(id.split(" WITH ").next().unwrap_or(id))))
}

/// The family of `id` (unknown when the table does not know it).
#[must_use]
pub fn family(id: &str) -> Family {
    terms(id).map_or(Family::Unknown, |t| t.family)
}

/// The id that stands for "its terms are in its own LICENSE file".
pub const LICENSE_FILE: &str = "LICENSE-FILE";

/// Whether a license fits your project, in words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fit {
    /// 0 fits, 1 fits with a condition, 2 does not fit, 3 unknown.
    pub level: u8,
    /// The sentence.
    pub line: String,
    /// The option the card assumes you take (its tab starts selected).
    pub choice: Vec<String>,
    /// There was a choice to make: the card names the one it assumed.
    pub assumed: bool,
    /// The part of the choice that was a choice (what `AND` always
    /// applies is not assumed, it just applies).
    pub chose: Vec<String>,
}

struct One {
    level: u8,
    words: String,
    short: String,
}

fn fit_one(ids: &[String], yours: &[String], project: &str) -> One {
    let mut one = One {
        level: 0,
        words: String::new(),
        short: String::new(),
    };
    let mut worse = |level: u8, words: String, short: &str| {
        if level >= one.level {
            one = One {
                level,
                words,
                short: short.to_owned(),
            };
        }
    };
    for id in ids {
        let b = base_id(id);
        match family(id) {
            Family::Unknown => worse(
                3,
                if id == LICENSE_FILE {
                    "Its terms are in its own LICENSE file: read them before you ship it".to_owned()
                } else {
                    format!("{id} is not a license this knows: read it before you ship it")
                },
                "has terms nobody has read yet",
            ),
            Family::Strong if !yours.iter().any(|y| y == b) => worse(
                2,
                format!("Shipping it would make {project} {}: anyone you ship to could ask for all of its source", short_id(id)),
                &format!("would make {project} {}", short_id(id)),
            ),
            Family::Weak if b == "MPL-2.0" || b == "EPL-2.0" => worse(
                1,
                format!("Fits, with one duty: keep its own files under {} and publish your changes to them; the rest of {project} stays yours", short_id(id)),
                "asks you to publish changes to its files",
            ),
            Family::Weak => worse(
                1,
                "Fits if people can swap in their own build of it; Rust links statically, so that means shipping your object files".to_owned(),
                "asks that people can relink it",
            ),
            _ if b == "Apache-2.0"
                && yours.iter().any(|y| y == "GPL-2.0")
                && !yours.iter().any(|y| y == "GPL-3.0") =>
            {
                worse(2, "Apache-2.0's patent terms do not mix with GPL-2.0".to_owned(), "does not mix with GPL-2.0");
            }
            _ => {}
        }
    }
    one
}

/// Whether `expr` (None: no license field at all) fits a project licensed
/// `yours`, called `project` in consequence words.
#[must_use]
pub fn fit(expr: Option<&Expr>, yours: &str, project: &str) -> Fit {
    let your_expr = parse(yours);
    let your_word = your_expr
        .as_ref()
        .map_or_else(|| yours.to_owned(), rest_word);
    let your_ids: Vec<String> = your_expr
        .as_ref()
        .map(ids)
        .unwrap_or_default()
        .iter()
        .map(|i| base_id(i).to_owned())
        .collect();
    let Some(expr) = expr else {
        return Fit {
            level: 3,
            line: "No license at all: by default nobody may copy it. Ask the author before you ship it.".to_owned(),
            choice: Vec::new(),
            assumed: false,
            chose: Vec::new(),
        };
    };
    let rated: Vec<(Vec<String>, One)> = options(expr)
        .into_iter()
        .map(|ids| {
            let one = fit_one(&ids, &your_ids, project);
            (ids, one)
        })
        .collect();
    let name = |ids: &[String]| {
        ids.iter()
            .map(|i| short_id(i))
            .collect::<Vec<_>>()
            .join(" + ")
    };
    if rated.len() == 1 {
        let (ids, one) = &rated[0];
        let line = if one.level == 0 {
            if ids.len() > 1 {
                format!("All of it applies, and all of it fits your {your_word} project.")
            } else {
                format!("Fits your {your_word} project.")
            }
        } else {
            format!("{}.", one.words)
        };
        return Fit {
            level: one.level,
            line,
            choice: ids.clone(),
            assumed: false,
            chose: Vec::new(),
        };
    }
    let best = rated.iter().map(|(_, one)| one.level).min().unwrap_or(3);
    let good: Vec<&(Vec<String>, One)> =
        rated.iter().filter(|(_, one)| one.level == best).collect();
    let bad: Vec<&(Vec<String>, One)> = rated.iter().filter(|(_, one)| one.level > best).collect();
    // The option assumed is the first that fits best, in the expression's
    // own order: that is how a lockfile-wide reading counts it. What every
    // option shares (an `AND`'s fixed part) is not a choice.
    let pick = good[0];
    let chose: Vec<String> = pick
        .0
        .iter()
        .filter(|i| !rated.iter().all(|(ids, _)| ids.contains(i)))
        .cloned()
        .collect();
    if best == 0 && bad.is_empty() && matches!(expr, Expr::And(_)) {
        let always: Vec<String> = pick
            .0
            .iter()
            .filter(|i| rated.iter().all(|(ids, _)| ids.contains(i)))
            .cloned()
            .collect();
        let mut rest: Vec<String> = rated
            .iter()
            .flat_map(|(ids, _)| ids.iter().filter(|i| !always.contains(i)).cloned())
            .collect();
        rest.dedup();
        let mut seen = Vec::new();
        rest.retain(|r| {
            if seen.contains(r) {
                false
            } else {
                seen.push(r.clone());
                true
            }
        });
        let always_words: Vec<String> = always.iter().map(|i| short_id(i)).collect();
        let rest_words: Vec<String> = rest.iter().map(|i| short_id(i)).collect();
        let line = format!(
            "{} always {}; for the rest, {}. All of it fits your {your_word} project.",
            list(&always_words, "and"),
            if always.len() > 1 { "apply" } else { "applies" },
            if rest.len() > 1 {
                format!("either of {}", list(&rest_words, "or"))
            } else {
                rest_words.join("")
            },
        );
        return Fit {
            level: 0,
            line,
            choice: pick.0.clone(),
            assumed: true,
            chose: chose.clone(),
        };
    }
    if best == 0 && bad.is_empty() {
        let patent = good.iter().find(|(ids, _)| {
            ids.iter()
                .any(|i| terms(i).is_some_and(|t| t.permissions.contains(&PATENT)))
        });
        let line = format!(
            "Either fits your {your_word} project{}.",
            patent.map_or_else(String::new, |(ids, _)| format!(
                "; {} adds a patent grant",
                name(ids)
            ))
        );
        return Fit {
            level: 0,
            line,
            choice: pick.0.clone(),
            assumed: true,
            chose: chose.clone(),
        };
    }
    let lead = if best == 0 {
        format!(
            "Choose {}: it fits your {your_word} project.",
            name(&pick.0)
        )
    } else {
        format!("Choose {}. {}.", name(&pick.0), pick.1.words)
    };
    let tail = bad
        .iter()
        .map(|(ids, one)| format!("{} {}", name(ids), one.short))
        .collect::<Vec<_>>()
        .join("; ");
    Fit {
        level: best,
        line: format!("{lead} {tail}."),
        choice: pick.0.clone(),
        assumed: true,
        chose,
    }
}

/// The assumption line under the fit, when the expression offered a choice.
#[must_use]
pub fn assumption(fit: &Fit) -> Option<String> {
    fit.assumed.then(|| {
        let named = if fit.chose.is_empty() {
            &fit.choice
        } else {
            &fit.chose
        };
        format!(
            "assuming you take it under {}",
            named
                .iter()
                .map(|i| short_id(i))
                .collect::<Vec<_>>()
                .join(" + ")
        )
    })
}

/// One package in your tree whose terms are worth naming.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notable {
    /// The crate.
    pub krate: String,
    /// Its license as written (`custom` for a license file).
    pub license: String,
    /// Its family, read at its most permissive option.
    pub family: Family,
    /// Reached only at build time.
    pub build_only: bool,
}

/// What the lockfile says about every license in your tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TreeLicenses {
    /// Packages in the lockfile.
    pub total: usize,
    /// Packages whose sources are not unpacked here (never "unlicensed").
    pub unread: usize,
    /// The ones worth naming (not permissive, or chosen from an `OR`).
    pub notable: Vec<Notable>,
}

fn rank(f: Family) -> u8 {
    match f {
        Family::Public => 0,
        Family::Permissive => 1,
        Family::Weak => 2,
        Family::Strong => 3,
        Family::Unknown => 4,
    }
}

/// The project hero's tree sentence.
#[must_use]
pub fn tree_summary(tree: &TreeLicenses) -> (String, Option<String>) {
    let notable: Vec<&Notable> = tree
        .notable
        .iter()
        .filter(|n| matches!(n.family, Family::Weak | Family::Strong | Family::Unknown))
        .collect();
    let mut by: Vec<(String, Vec<&Notable>)> = Vec::new();
    for n in notable {
        match by.iter_mut().find(|(lic, _)| *lic == n.license) {
            Some((_, xs)) => xs.push(n),
            None => by.push((n.license.clone(), vec![n])),
        }
    }
    let parts: Vec<String> = by
        .iter()
        .map(|(lic, xs)| {
            let run: Vec<&str> = xs
                .iter()
                .filter(|x| !x.build_only)
                .map(|x| x.krate.as_str())
                .collect();
            let built: Vec<&str> = xs
                .iter()
                .filter(|x| x.build_only)
                .map(|x| x.krate.as_str())
                .collect();
            if lic == "custom" {
                format!(
                    "custom terms in {}",
                    list(if run.is_empty() { &built } else { &run }, "and")
                )
            } else {
                let tail = if built.is_empty() {
                    String::new()
                } else {
                    format!(" ({} too, at build time only)", list(&built, "and"))
                };
                format!("{lic} in {}{tail}", list(&run, "and"))
            }
        })
        .collect();
    let line = format!(
        "{}: permissive throughout{}.",
        plural(tree.total, "package", "packages"),
        if parts.is_empty() {
            String::new()
        } else {
            format!(", plus {}", list(&parts, "and"))
        }
    );
    let unread = (tree.unread > 0).then(|| {
        format!(
            "{} are not unpacked here, so unread.",
            thousands(tree.unread)
        )
    });
    (line, unread)
}

/// The one line a package card adds about your tree, only when its terms
/// are not permissive: whether it brings a new kind of terms, or the tree
/// already carries them. The bool says "new".
#[must_use]
pub fn tree_line(
    expr: &Expr,
    tree: &TreeLicenses,
    package: Option<&str>,
) -> Option<(String, bool)> {
    let eff = options(expr)
        .iter()
        .map(|o| o.iter().map(|i| rank(family(i))).max().unwrap_or(0))
        .min()
        .unwrap_or(0);
    if eff < 2 {
        return None;
    }
    let fam = [Family::Weak, Family::Strong, Family::Unknown]
        .into_iter()
        .find(|f| rank(*f) == eff)?;
    let all: Vec<&Notable> = tree.notable.iter().filter(|n| n.family == fam).collect();
    let carried: Vec<&Notable> = all
        .iter()
        .copied()
        .filter(|n| Some(n.krate.as_str()) != package)
        .collect();
    if package.is_some() && carried.len() < all.len() {
        let build = |n: &Notable| {
            if n.build_only {
                format!("{} (build-time only)", n.krate)
            } else {
                n.krate.clone()
            }
        };
        let names: Vec<String> = carried.iter().map(|n| build(n)).collect();
        return Some((
            if carried.is_empty() {
                format!("The only package in your tree with {}.", fam.terms())
            } else {
                format!(
                    "One of {} packages in your tree with {}; the others are {}.",
                    word(all.len()),
                    fam.terms(),
                    list(&names, "and")
                )
            },
            false,
        ));
    }
    let lic: Vec<String> = ids(expr)
        .iter()
        .map(|i| short_id(i))
        .filter(|i| rank(family(i)) >= 2)
        .collect();
    let lic = if lic.is_empty() {
        "these terms".to_owned()
    } else {
        lic.join(", ")
    };
    Some(if carried.is_empty() {
        (
            format!(
                "Adding it brings {lic} into a tree with no {} today.",
                fam.terms()
            ),
            true,
        )
    } else {
        let names: Vec<&str> = carried.iter().take(3).map(|n| n.krate.as_str()).collect();
        (
            format!(
                "Your tree already has {} in {}; this adds no new kind.",
                fam.terms(),
                list(&names, "and")
            ),
            false,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{Expr, Family, assumption, family, fit, options, parse, rest_word};

    #[test]
    fn expressions_parse_with_and_binding_tighter() {
        let e = parse("(MIT OR Apache-2.0) AND Unicode-3.0").expect("parses");
        assert_eq!(
            options(&e),
            vec![
                vec!["MIT".to_owned(), "Unicode-3.0".to_owned()],
                vec!["Apache-2.0".to_owned(), "Unicode-3.0".to_owned()]
            ]
        );
        assert_eq!(rest_word(&e), "MIT/Apache-2.0 +1");
        assert_eq!(
            parse("MIT/Apache-2.0"),
            Some(Expr::Or(vec![
                Expr::Id("MIT".into()),
                Expr::Id("Apache-2.0".into())
            ]))
        );
        assert_eq!(
            rest_word(&parse("MIT OR Apache-2.0 OR LGPL-2.1-or-later").expect("parses")),
            "MIT/Apache-2.0 +1"
        );
        assert_eq!(family("GPL-2.0-only"), Family::Strong);
        assert_eq!(family("Apache-2.0 WITH LLVM-exception"), Family::Permissive);
    }

    #[test]
    fn an_or_fits_and_names_what_it_assumed() {
        let toml = fit(
            parse("MIT OR Apache-2.0").as_ref(),
            "MIT OR Apache-2.0",
            "present",
        );
        assert_eq!(
            toml.line,
            "Either fits your MIT/Apache-2.0 project; Apache-2.0 adds a patent grant."
        );
        assert_eq!(
            assumption(&toml).as_deref(),
            Some("assuming you take it under MIT")
        );
        let cell = fit(
            parse("Apache-2.0 OR GPL-2.0-only").as_ref(),
            "MIT OR Apache-2.0",
            "present",
        );
        assert_eq!(
            cell.line,
            "Choose Apache-2.0: it fits your MIT/Apache-2.0 project. GPL-2.0 would make present GPL-2.0."
        );
        assert_eq!(
            assumption(&cell).as_deref(),
            Some("assuming you take it under Apache-2.0")
        );
        let mpl = fit(parse("MPL-2.0").as_ref(), "MIT OR Apache-2.0", "present");
        assert_eq!(mpl.level, 1);
        assert_eq!(assumption(&mpl), None, "no choice, nothing assumed");
    }
}
