//! What a type is made of and what you can do with it: an enum's cases, a
//! struct's fields, what a trait's implementor writes, and the methods in
//! groups by what they do with the value.

use super::super::facts::{Facts, Member, Owes, Receives};
use super::super::view::{
    Case, Do, Field, Group, Kind, Lang, Origin, Outcomes, Owed, Row, Shape, Ty, Yours,
};
use super::callable::callable;
use super::docs::row_doc;
use super::known::Conv;
use super::text::{balanced, split_top, squash, strip_leading};
use super::words::{Cx, T, parse, ty_of};

fn cx<'a>(
    facts: &'a Facts,
    generics: &'a [String],
    link: &'a dyn Fn(&str) -> Option<String>,
) -> Cx<'a> {
    Cx {
        generics,
        owner: Some(facts.name.as_str()),
        link,
        origin: Origin::Declared,
    }
}

// ------------------------------------------------------------------ what it is

/// The shape of a type page, when it has one.
pub(super) fn shape(facts: &Facts) -> Option<Shape> {
    match facts.kind {
        Kind::Enum => Some(one_of(facts)),
        Kind::Alias if !facts.made_of.is_empty() => Some(one_of(facts)),
        Kind::Alias if union_cases(facts).is_some_and(|cases| cases.len() > 1) => {
            Some(Shape::OneOf(union_cases(facts).unwrap_or_default()))
        }
        Kind::Struct if !facts.made_of.is_empty() => Some(holds(facts)),
        Kind::Trait => {
            let rows = owed(facts);
            (!rows.is_empty()).then_some(Shape::Write {
                rows,
                implementors: facts.implementors,
            })
        }
        _ => None,
    }
}

/// A case's payload: `String(String)` → `[text]`, `Typed(A, B)` → `[A, B]`,
/// `Point { x: i32 }` → `[an integer]`, `Null` → nothing.
fn payload(facts: &Facts, member: &Member, generics: &[String]) -> Vec<Ty> {
    let Some(signature) = member.signature.as_deref() else {
        return Vec::new();
    };
    let signature = strip_leading(signature);
    // The text after the case's name.
    let after = signature
        .find(member.name.as_str())
        .map_or(signature, |at| &signature[at + member.name.len()..])
        .trim_start();
    let link = |name: &str| facts.link(name).map(ToOwned::to_owned);
    let cx = cx(facts, generics, &link);
    let mut out = Vec::new();
    if after.starts_with('(') {
        if let Some((inner, _)) = balanced(after, 0) {
            for part in split_top(inner, ',') {
                let tree = parse(&part);
                let mut ty = ty_of(&tree, &part, &cx);
                ty.loops = tree.mentions(&facts.name);
                out.push(ty);
            }
        }
    } else if after.starts_with('{')
        && let Some((inner, _)) = balanced(after, 0)
    {
        for part in split_top(inner, ',') {
            if let Some((_, ty_text)) = part.split_once(':') {
                let tree = parse(ty_text);
                let mut ty = ty_of(&tree, ty_text, &cx);
                ty.loops = tree.mentions(&facts.name);
                out.push(ty);
            }
        }
    }
    out
}

fn one_of(facts: &Facts) -> Shape {
    let generics: Vec<String> = Vec::new();
    let cases = facts
        .made_of
        .iter()
        .map(|member| {
            let holds = payload(facts, member, &generics);
            Case {
                name: member.name.clone(),
                holds,
                doc: row_doc(member.summary.as_deref()),
                more: member
                    .more
                    .as_deref()
                    .map(super::text::plain)
                    .filter(|t| !t.is_empty()),
                link: member.link.clone(),
                yours: Yours::default(),
            }
        })
        .collect();
    Shape::OneOf(cases)
}

/// `"a" | "b" | string` as literal cases.
fn union_cases(facts: &Facts) -> Option<Vec<Case>> {
    let signature = facts.signature.as_deref()?;
    let (_, body) = signature.split_once('=')?;
    let body = body.trim().trim_end_matches(';');
    let parts = split_top(body, '|');
    if parts.len() < 2 {
        return None;
    }
    let generics: Vec<String> = Vec::new();
    let link = |name: &str| facts.link(name).map(ToOwned::to_owned);
    let cx = cx(facts, &generics, &link);
    Some(
        parts
            .iter()
            .map(|part| {
                let part = part.trim();
                let tree = parse(part);
                let ty = ty_of(&tree, part, &cx);
                match tree {
                    T::Lit(_) => Case {
                        name: part.to_owned(),
                        holds: Vec::new(),
                        doc: String::new(),
                        more: None,
                        link: None,
                        yours: Yours::default(),
                    },
                    _ => Case {
                        name: part.to_owned(),
                        holds: vec![ty],
                        doc: String::new(),
                        more: None,
                        link: None,
                        yours: Yours::default(),
                    },
                }
            })
            .collect(),
    )
}

fn holds(facts: &Facts) -> Shape {
    let generics: Vec<String> = Vec::new();
    let link = |name: &str| facts.link(name).map(ToOwned::to_owned);
    let cx = cx(facts, &generics, &link);
    let mut fields = Vec::new();
    let mut hidden = 0;
    for member in &facts.made_of {
        let text = member
            .signature
            .as_deref()
            .map(strip_leading)
            .unwrap_or_default();
        let public = facts.lang != Lang::Rust
            || member.signature.is_none()
            || (text.starts_with("pub") && !text.starts_with("pub("));
        if !public {
            hidden += 1;
            continue;
        }
        let ty_text = text
            .find(&format!("{}:", member.name))
            .map(|at| {
                text[at + member.name.len() + 1..]
                    .trim()
                    .trim_end_matches([',', ';'])
                    .to_owned()
            })
            .or_else(|| {
                text.split_once(':')
                    .map(|(_, ty)| ty.trim().trim_end_matches([',', ';']).to_owned())
            })
            .unwrap_or_default();
        let tree = parse(&ty_text);
        let mut ty = if ty_text.is_empty() {
            Ty::plain("anything")
        } else {
            ty_of(&tree, &ty_text, &cx)
        };
        ty.loops = !ty_text.is_empty() && tree.mentions(&facts.name);
        fields.push(Field {
            name: member.name.clone(),
            ty,
            doc: row_doc(member.summary.as_deref()),
            more: member
                .more
                .as_deref()
                .map(super::text::plain)
                .filter(|t| !t.is_empty()),
            link: member.link.clone(),
            yours: Yours::default(),
        });
    }
    Shape::Holds { fields, hidden }
}

fn owed(facts: &Facts) -> Vec<Owed> {
    facts
        .does
        .iter()
        .filter(|member| {
            matches!(member.owes, Owes::Required)
                || (member.owes == Owes::Unknown
                    && facts.does.iter().all(|m| m.owes == Owes::Unknown))
        })
        .map(|member| {
            let (takes, outcomes) = member_call(facts, member).map_or(
                (Ty::plain("nothing"), Outcomes::default()),
                |(takes, _gives, outcomes)| {
                    (
                        takes
                            .into_iter()
                            .next()
                            .map_or_else(|| Ty::plain("nothing"), |t| t),
                        outcomes,
                    )
                },
            );
            Owed {
                name: member.name.clone(),
                takes,
                doc: row_doc(member.summary.as_deref()),
                outcomes,
                link: member.link.clone(),
            }
        })
        .collect()
}

// ------------------------------------------------------------------ what you can do

/// A member's call read: what it takes, what it gives, its outcomes.
fn member_call(facts: &Facts, member: &Member) -> Option<(Vec<Ty>, Option<String>, Outcomes)> {
    let signature = member.signature.as_ref()?;
    let mut sub = Facts::new(&member.name, Kind::Method, facts.lang, &facts.package);
    sub.signature = Some(signature.clone());
    sub.owner = Some(facts.name.clone());
    sub.links = facts.links.clone();
    let derived = callable(&sub)?;
    let takes = derived.call.ports.iter().map(|p| p.ty.clone()).collect();
    let gives = derived.call.gives.ty.as_ref().map(|t| t.word.clone());
    Some((takes, gives, derived.call.outcomes()))
}

fn receives(member: &Member) -> Do {
    match member.receives {
        Receives::Changes => Do::Changes,
        Receives::Reads => Do::Reads,
        Receives::UsesUp => Do::UsesUp,
        Receives::Makes | Receives::Unknown => match member.signature.as_deref() {
            Some(sig)
                if squash(sig).contains("(&mut self") || squash(sig).contains("(&mut self,") =>
            {
                Do::Changes
            }
            Some(sig)
                if squash(sig).contains("(&self")
                    || squash(sig).contains("(&'") && squash(sig).contains("self") =>
            {
                Do::Reads
            }
            _ => Do::Makes,
        },
    }
}

/// Merges number-like words: `a count`, `an integer`, `a number` → `a number`.
fn merge_number(word: &str) -> &str {
    match word {
        "a count" | "an integer" | "a number" | "a byte" => "a number",
        other => other,
    }
}

/// The methods in groups by verb. Signatures that share a name (a
/// language's overloads, `From` for many types) become one row that lists
/// what they take; `FromStr` reads as `parse`, the way you call it.
pub(super) fn verbs(facts: &Facts) -> Vec<Group> {
    // Trait members owed by an implementor are the shape, not the verbs.
    if facts.does.is_empty()
        || !matches!(
            facts.kind,
            Kind::Enum | Kind::Struct | Kind::Alias | Kind::Other
        )
    {
        return Vec::new();
    }
    let mut groups: Vec<Group> = Vec::new();
    for member in &facts.does {
        let verb = receives(member);
        let (takes, gives, outcomes) = member_call(facts, member).unwrap_or_default();
        let conv = Conv::of(facts.lang, &member.name);
        let mut row = Row {
            name: member.name.clone(),
            takes: takes.iter().map(|t| t.word.clone()).collect(),
            gives,
            outcomes,
            doc: row_doc(member.summary.as_deref()),
            link: member.link.clone(),
            yours: 0,
            also: Vec::new(),
        };
        if let Some(conv) = conv.filter(|_| verb == Do::Makes) {
            row.doc = conv.doc(&facts.name);
            row.takes = takes
                .first()
                .map(|first| merge_number(&first.word).to_owned())
                .into_iter()
                .collect();
            row.gives = None;
            if let Some(called) = conv.called().filter(|_| row.outcomes.fails) {
                row.also
                    .push(std::mem::replace(&mut row.name, called.to_owned()));
                row.takes = vec!["text".to_owned()];
                row.outcomes = Outcomes {
                    fails: true,
                    ..Outcomes::default()
                };
            }
        }
        let group = match groups.iter().position(|group| group.verb == verb) {
            Some(at) => &mut groups[at],
            None => {
                groups.push(Group {
                    verb,
                    rows: Vec::new(),
                });
                groups
                    .last_mut()
                    .unwrap_or_else(|| unreachable!("a group was just pushed"))
            }
        };
        match group
            .rows
            .iter_mut()
            .find(|earlier| earlier.name == row.name)
        {
            Some(earlier) => overload(earlier, row),
            None => group.rows.push(row),
        }
    }
    groups.sort_by_key(|group| group.verb);
    for group in &mut groups {
        // `From` leads the ways to make one.
        if let Some(at) = group
            .rows
            .iter()
            .position(|row| row.name == "from" && Conv::of(facts.lang, "from").is_some())
        {
            let row = group.rows.remove(at);
            group.rows.insert(0, row);
        }
    }
    groups
}

/// Folds another signature of the same name into `row`.
fn overload(row: &mut Row, other: Row) {
    for word in other.takes {
        if !row.takes.contains(&word) {
            row.takes.push(word);
        }
    }
    if row.gives != other.gives {
        row.gives = None;
    }
    row.outcomes = Outcomes {
        fails: row.outcomes.fails || other.outcomes.fails,
        none: row.outcomes.none || other.outcomes.none,
        later: row.outcomes.later || other.outcomes.later,
        many: row.outcomes.many || other.outcomes.many,
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::symbol::facts::Member;

    fn member(name: &str, signature: &str, summary: &str) -> Member {
        Member {
            name: name.into(),
            signature: Some(signature.into()),
            summary: Some(summary.into()),
            ..Member::default()
        }
    }

    #[test]
    fn an_enum_is_a_fork_with_payloads_and_loops() {
        let mut f = Facts::new("Value", Kind::Enum, Lang::Rust, "serde_json");
        f.made_of = vec![
            member("Null", "Null", "JSON null value."),
            member("Bool", "Bool(bool)", "JSON boolean."),
            member("Array", "Array(Vec<Value>)", "JSON array."),
            member("Object", "Object(Map<String, Value>)", "JSON object."),
        ];
        let Some(Shape::OneOf(cases)) = shape(&f) else {
            panic!("a fork")
        };
        assert_eq!(cases[0].holds.len(), 0);
        assert_eq!(cases[1].holds[0].word, "yes or no");
        assert_eq!(cases[2].holds[0].word, "a list of Value");
        assert!(cases[2].holds[0].loops, "an array holds more Values");
        assert_eq!(cases[3].holds[0].word, "a map of text to Value");
        assert_eq!(cases[3].doc, "JSON object");
    }

    #[test]
    fn a_struct_is_a_bracket_of_its_public_fields() {
        let mut f = Facts::new(
            "AllocationInfo",
            Kind::Struct,
            Lang::Rust,
            "allocation_counter",
        );
        f.made_of = vec![
            member(
                "count_total",
                "pub count_total: u64",
                "The total number of allocations.",
            ),
            member(
                "count_current",
                "pub count_current: i64",
                "The current number.",
            ),
            member("secret", "secret: u8", "hidden"),
        ];
        let Some(Shape::Holds { fields, hidden }) = shape(&f) else {
            panic!("a bracket")
        };
        assert_eq!(
            fields
                .iter()
                .map(|f| f.ty.word.as_str())
                .collect::<Vec<_>>(),
            ["a count", "an integer"]
        );
        assert_eq!(fields[0].ty.written.as_deref(), Some("u64"));
        assert_eq!(hidden, 1);
    }

    #[test]
    fn a_typescript_union_of_literals_is_a_fork_of_literals() {
        let mut f = Facts::new("$ZodStringFormats", Kind::Alias, Lang::TypeScript, "zod");
        f.signature =
            Some("export type $ZodStringFormats = \"email\" | \"url\" | \"uuid\";".into());
        let Some(Shape::OneOf(cases)) = shape(&f) else {
            panic!("a fork")
        };
        assert_eq!(
            cases.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
            ["\"email\"", "\"url\"", "\"uuid\""]
        );
    }

    #[test]
    fn a_trait_is_what_you_write() {
        let mut f = Facts::new("Serialize", Kind::Trait, Lang::Rust, "serde");
        let mut m = member(
            "serialize",
            "fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>\n    where\n        S: Serializer;",
            "Serialize this value into the given Serde serializer.",
        );
        m.owes = Owes::Required;
        f.does = vec![m];
        let Some(Shape::Write { rows, .. }) = shape(&f) else {
            panic!("what you write")
        };
        assert_eq!(rows[0].name, "serialize");
        assert_eq!(rows[0].takes.word, "a serializer");
        assert!(rows[0].outcomes.fails);
    }

    #[test]
    fn methods_group_by_verb_with_from_folded_and_fromstr_as_parse() {
        let mut f = Facts::new("Value", Kind::Enum, Lang::Rust, "serde_json");
        let mk = |name: &str, sig: &str, receives: Receives| Member {
            receives,
            ..member(name, sig, "")
        };
        f.does = vec![
            mk(
                "as_str",
                "pub fn as_str(&self) -> Option<&str>",
                Receives::Reads,
            ),
            mk(
                "as_array_mut",
                "pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>>",
                Receives::Changes,
            ),
            mk("from", "fn from(val: &'a str) -> Value", Receives::Makes),
            mk("from", "fn from(val: Vec<V>) -> Value", Receives::Makes),
            mk("from", "fn from(val: i64) -> Value", Receives::Makes),
            mk("from", "fn from(val: u64) -> Value", Receives::Makes),
            mk(
                "from_str",
                "fn from_str(s: &str) -> Result<Value, Self::Err>",
                Receives::Makes,
            ),
            mk(
                "try_into",
                "pub fn try_into<'de, T>(self) -> Result<T, crate::de::Error>",
                Receives::UsesUp,
            ),
        ];
        let groups = verbs(&f);
        let heads: Vec<&str> = groups.iter().map(|g| g.verb.head()).collect();
        assert_eq!(heads, ["Makes one", "Reads it", "Changes it", "Uses it up"]);
        let makes = &groups[0].rows;
        assert_eq!(makes[0].name, "from");
        assert_eq!(
            makes[0].takes,
            ["text", "a list of V", "a number"],
            "From is one row listing what it converts from"
        );
        assert_eq!(makes[1].name, "parse");
        assert!(makes[1].outcomes.fails);
        assert!(groups[1].rows[0].outcomes.none);
    }
}
