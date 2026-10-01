//! The context rail: where the source is, what is next to it, what it can
//! do, its releases, and how we know its types.

use super::super::facts::Facts;
use super::super::view::{Cap, CapMark, Kind, Outcomes, Rail, Release, Sibling, SourceAt};
use super::callable::callable;
use super::known::{Chip, Std};

/// The rail for a page.
pub(super) fn rail(facts: &Facts, untyped: bool) -> Rail {
    let source = facts.site.as_ref().map(|site| SourceAt {
        file: site.file.clone(),
        line: site.line,
        open: site.open.clone(),
        package: match &facts.version {
            Some(version) => format!("{} {}", facts.package, version),
            None => facts.package.clone(),
        },
        pinned: facts.pinned,
    });
    let releases = facts
        .releases
        .iter()
        .map(|(version, differs)| Release {
            version: version.clone(),
            pinned: facts.version.as_deref() == Some(version.as_str()),
            differs: *differs,
        })
        .collect();
    Rail {
        source,
        siblings: siblings(facts),
        can: caps(facts),
        releases,
        across: facts.across.clone(),
        how: untyped.then(|| {
            format!(
                "{} declares no types here. What the page shows is read from its docs and its code; a dotted underline marks each one.",
                facts.package
            )
        }),
    }
}

/// The words a name is made of: `from_slice` → `[from, slice]`,
/// `asStr` → `[as, str]`.
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut previous_lower = false;
    for ch in name.chars() {
        if ch == '_' || ch == '.' || ch == '-' {
            if !current.is_empty() {
                out.push(std::mem::take(&mut current));
            }
            previous_lower = false;
            continue;
        }
        if ch.is_uppercase() && previous_lower && !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
        previous_lower = ch.is_lowercase() || ch.is_ascii_digit();
        current.push(ch.to_ascii_lowercase());
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// How much a sibling's name resembles the page's: shared leading or
/// trailing words.
fn affinity(current: &[String], other: &[String]) -> usize {
    let lead = current
        .iter()
        .zip(other)
        .take_while(|(a, b)| a == b)
        .count();
    let tail = current
        .iter()
        .rev()
        .zip(other.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    lead.max(tail)
}

/// What differs, in words: `from_slice` beside `from_str` reads "slice",
/// or what its signature takes when it is read ("from bytes").
fn differs(
    facts: &Facts,
    current: &[String],
    name: &str,
    kind: Kind,
    signature: Option<&str>,
) -> String {
    let other = words(name);
    let lead = current
        .iter()
        .zip(&other)
        .take_while(|(a, b)| a == b)
        .count();
    let tail: Vec<&str> = other[lead.min(other.len())..]
        .iter()
        .map(String::as_str)
        .collect();
    if let Some(signature) = signature {
        let mut sub = Facts::new(name, Kind::Function, facts.lang, &facts.package);
        sub.signature = Some(signature.to_owned());
        sub.links = facts.links.clone();
        if let Some(derived) = callable(&sub)
            && let Some(port) = derived.call.ports.first()
            && lead > 0
            && other
                .first()
                .is_some_and(|w| matches!(w.as_str(), "from" | "to" | "as" | "into" | "try"))
        {
            return format!("{} {}", other[0], port.ty.word);
        }
    }
    // Only the tail is shared (`ser_value` beside `de_value`): what differs
    // is its head.
    let trail = current
        .iter()
        .rev()
        .zip(other.iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    if lead == 0 && trail > 0 && trail < other.len() {
        return other[..other.len() - trail].join(" ");
    }
    // Nothing differs in the words of the name, or nothing is shared (the
    // whole name differs, and it is already shown): say what it is instead
    // of saying its name twice (`Settings` beside `read_settings` read
    // "Settings settings").
    if tail.is_empty() || lead == 0 {
        kind.word().to_owned()
    } else {
        tail.join(" ")
    }
}

fn siblings(facts: &Facts) -> Vec<Sibling> {
    let current = words(&facts.name);
    let mut scored: Vec<(usize, usize, &super::super::facts::Beside)> = facts
        .beside
        .iter()
        .enumerate()
        .filter(|(_, beside)| beside.name != facts.name)
        .map(|(index, beside)| {
            let mut score = affinity(&current, &words(&beside.name));
            if beside.kind == facts.kind {
                score += 1;
            }
            (score, index, beside)
        })
        .collect();
    let here = facts
        .beside
        .iter()
        .position(|b| b.name == facts.name)
        .unwrap_or(0);
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then_with(|| a.1.abs_diff(here).cmp(&b.1.abs_diff(here)))
    });
    let mut chosen: Vec<_> = scored.into_iter().take(6).collect();
    // Shown in outline order.
    chosen.sort_by_key(|(_, index, _)| *index);
    chosen
        .into_iter()
        .map(|(_, _, beside)| {
            let outcomes =
                beside
                    .signature
                    .as_deref()
                    .map_or_else(Outcomes::default, |signature| {
                        let mut sub =
                            Facts::new(&beside.name, beside.kind, facts.lang, &facts.package);
                        sub.signature = Some(signature.to_owned());
                        callable(&sub)
                            .map_or_else(Outcomes::default, |derived| derived.call.outcomes())
                    });
            Sibling {
                name: beside.name.clone(),
                kind: beside.kind,
                differs: differs(
                    facts,
                    &current,
                    &beside.name,
                    beside.kind,
                    beside.signature.as_deref(),
                ),
                outcomes,
                link: beside.link.clone(),
            }
        })
        .collect()
}

/// The traits it implements as chips: the ones the language names get their
/// mark and words; the rest show their name.
fn caps(facts: &Facts) -> Vec<Cap> {
    let mut out: Vec<Cap> = Vec::new();
    for (name, derived) in &facts.implements {
        let (mark, word) = match Std::of(name).map_or(Chip::Named, Std::chip) {
            Chip::Hidden => continue,
            Chip::Says(mark, word) => (mark, word),
            Chip::Named => (CapMark::Other, name.as_str()),
        };
        if out
            .iter()
            .any(|cap| cap.word == word && cap.mark == mark && mark != CapMark::Other)
        {
            continue;
        }
        if out.iter().any(|cap| cap.name == *name) {
            continue;
        }
        out.push(Cap {
            name: name.clone(),
            word: word.to_owned(),
            mark,
            derived: *derived,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::symbol::facts::Beside;
    use crate::anatomy::symbol::view::Lang;

    fn beside(name: &str, kind: Kind, sig: Option<&str>) -> Beside {
        Beside {
            name: name.into(),
            kind,
            signature: sig.map(Into::into),
            link: Some(format!("addr:{name}")),
        }
    }

    #[test]
    fn siblings_are_the_names_that_read_alike_and_say_what_differs() {
        let mut f = Facts::new("from_str", Kind::Function, Lang::Rust, "serde_json");
        f.beside = vec![
            beside("Error", Kind::Struct, None),
            beside(
                "from_reader",
                Kind::Function,
                Some(
                    "pub fn from_reader<R>(rdr: R) -> Result<T> where R: io::Read, T: DeserializeOwned",
                ),
            ),
            beside(
                "from_slice",
                Kind::Function,
                Some("pub fn from_slice<'a, T>(v: &'a [u8]) -> Result<T> where T: Deserialize<'a>"),
            ),
            beside("from_str", Kind::Function, None),
            beside(
                "to_string",
                Kind::Function,
                Some("pub fn to_string<T>(value: &T) -> Result<String>"),
            ),
            beside("Map", Kind::Struct, None),
        ];
        let rail = rail(&f, false);
        let names: Vec<&str> = rail.siblings.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"from_reader") && names.contains(&"from_slice"),
            "{names:?}"
        );
        let slice = rail
            .siblings
            .iter()
            .find(|s| s.name == "from_slice")
            .expect("from_slice");
        assert_eq!(slice.differs, "from bytes");
        assert!(slice.outcomes.fails);
        let reader = rail
            .siblings
            .iter()
            .find(|s| s.name == "from_reader")
            .expect("from_reader");
        assert_eq!(reader.differs, "from a reader");
    }

    /// A name that shares no words with the page's is not said twice: its
    /// kind is; one that shares only its tail says its head (J1: "Settings
    /// settings", "project_name project name" beside `read_settings`).
    #[test]
    fn a_sibling_that_shares_nothing_says_its_kind_and_one_that_shares_a_tail_says_its_head() {
        let mut f = Facts::new("read_settings", Kind::Function, Lang::Rust, "toml_pin");
        f.beside = vec![
            beside("Settings", Kind::Struct, None),
            beside("read_settings", Kind::Function, None),
            beside("project_name", Kind::Function, None),
            beside("write_settings", Kind::Function, None),
        ];
        let rail = rail(&f, false);
        let said = |name: &str| {
            rail.siblings
                .iter()
                .find(|s| s.name == name)
                .map(|s| s.differs.clone())
                .unwrap_or_else(|| panic!("{name} is beside it"))
        };
        assert_eq!(said("Settings"), Kind::Struct.word());
        assert_eq!(said("project_name"), Kind::Function.word());
        assert_eq!(said("write_settings"), "write");
    }

    #[test]
    fn traits_become_chips_and_unknown_ones_keep_their_name() {
        let mut f = Facts::new("Value", Kind::Enum, Lang::Rust, "serde_json");
        f.implements = vec![
            ("Clone".into(), true),
            ("PartialEq".into(), true),
            ("Eq".into(), true),
            ("Serialize".into(), false),
            ("FromIterator".into(), false),
        ];
        let caps = rail(&f, false).can;
        assert_eq!(
            caps.iter().map(|c| c.word.as_str()).collect::<Vec<_>>(),
            [
                "copies",
                "compares with ==",
                "serde can write it",
                "FromIterator"
            ]
        );
        assert_eq!(caps[3].mark, CapMark::Other);
    }
}
