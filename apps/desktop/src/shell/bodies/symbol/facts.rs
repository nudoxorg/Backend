//! What the page is derived from: the index's page for the declaration, read
//! into facet's plain [`Facts`]. `facet::anatomy::symbol::compile` does the
//! rest, purely.

use super::place;
use crate::model::pages::{
    DeclRef, DocFragment, DocSection, Member, PackageRef, Receiver, SectionKind, SymbolPage,
};
use backend_library::{DeclarationKind, Obligation};
use facet::anatomy::history::{History, Was};
use facet::anatomy::symbol::facts::{
    Beside, Facts, Implementors, Member as FactMember, Owes, Receives, Section,
    SectionKind as FactSection, Site,
};
use facet::anatomy::symbol::view::{Block, Kind, Lang};

/// The page's kind for an index kind.
pub(super) const fn kind_of(kind: Option<DeclarationKind>) -> Kind {
    match kind {
        Some(DeclarationKind::Function) => Kind::Function,
        Some(DeclarationKind::Method | DeclarationKind::Constructor) => Kind::Method,
        Some(DeclarationKind::Enum) => Kind::Enum,
        Some(DeclarationKind::Struct | DeclarationKind::Class | DeclarationKind::Union) => {
            Kind::Struct
        }
        Some(DeclarationKind::Trait | DeclarationKind::Interface) => Kind::Trait,
        Some(DeclarationKind::Type) => Kind::Alias,
        Some(DeclarationKind::Constant | DeclarationKind::Variable) => Kind::Constant,
        Some(DeclarationKind::Module) => Kind::Module,
        _ => Kind::Other,
    }
}

const fn receives(receiver: Receiver) -> Receives {
    match receiver {
        Receiver::Reads => Receives::Reads,
        Receiver::Changes => Receives::Changes,
        Receiver::Consumes => Receives::UsesUp,
        Receiver::Makes => Receives::Makes,
        Receiver::Unknown => Receives::Unknown,
    }
}

const fn owes(decl: &DeclRef) -> Owes {
    match decl.facts.obligation.known().copied().flatten() {
        Some(Obligation::Required) => Owes::Required,
        Some(Obligation::Optional) => Owes::Optional,
        Some(Obligation::Provided) => Owes::Provided,
        _ => Owes::Unknown,
    }
}

const fn section_kind(kind: SectionKind) -> FactSection {
    match kind {
        SectionKind::Errors => FactSection::Errors,
        SectionKind::Panics => FactSection::Panics,
        SectionKind::Safety => FactSection::Safety,
        SectionKind::Examples => FactSection::Examples,
        SectionKind::Returns => FactSection::Returns,
        SectionKind::Parameters => FactSection::Parameters,
        SectionKind::Deprecated => FactSection::Deprecated,
        SectionKind::Other => FactSection::Other,
    }
}

/// The type a method belongs to (`FlagSet` for `FlagSet.Parse`, `Value` for a
/// Rust method whose trail has one), and its own name.
fn owner(page: &SymbolPage) -> (String, Option<String>) {
    let name = page.identity.name.to_string();
    if let Some((owner, leaf)) = name
        .rsplit_once('.')
        .filter(|(owner, leaf)| !owner.is_empty() && !leaf.is_empty())
    {
        return (leaf.to_owned(), Some(owner.to_owned()));
    }
    if matches!(
        page.identity.kind,
        Some(DeclarationKind::Method | DeclarationKind::Constructor)
    ) {
        let identity = page.identity.coordinate.identity();
        let segments = identity
            .trail()
            .segments()
            .iter()
            .map(|segment| segment.as_str().to_owned())
            .collect::<Vec<_>>();
        if segments.len() >= 2 {
            return (name, segments.get(segments.len() - 2).cloned());
        }
    }
    (name, None)
}

fn separator_only(line: &str) -> bool {
    !line.chars().any(char::is_alphanumeric)
}

/// Doc fragments as blocks in markup: paragraphs (two breaks or a blank
/// line), inline code as `` `code` ``, a code fragment with lines as an
/// example, a link as `[label](coordinate)`, a Markdown heading as a head.
pub(super) fn blocks(fragments: &[DocFragment]) -> Vec<Block> {
    let mut out: Vec<Block> = Vec::new();
    let mut para = String::new();
    let mut breaks = 0;
    let flush = |para: &mut String, out: &mut Vec<Block>| {
        let text = para.trim().to_owned();
        para.clear();
        if text.is_empty() {
            return;
        }
        match text.strip_prefix('#') {
            Some(rest)
                if text.starts_with("# ")
                    || text.starts_with("## ")
                    || text.starts_with("### ") =>
            {
                out.push(Block::Head(rest.trim_start_matches('#').trim().to_owned()))
            }
            _ if text.starts_with("- ") || text.starts_with("* ") => {
                out.push(Block::Item(text[2..].trim().to_owned()))
            }
            _ => out.push(Block::Para(text)),
        }
    };
    for fragment in fragments {
        if matches!(fragment, DocFragment::Break) {
            breaks += 1;
            continue;
        }
        if breaks >= 2 {
            flush(&mut para, &mut out);
        } else if breaks == 1 && !para.is_empty() && !para.ends_with(char::is_whitespace) {
            para.push(' ');
        }
        breaks = 0;
        match fragment {
            DocFragment::Text(text) => {
                for (index, part) in text.split("\n\n").enumerate() {
                    if index > 0 {
                        flush(&mut para, &mut out);
                    }
                    for line in part.lines() {
                        if separator_only(line.trim()) {
                            continue;
                        }
                        if line.trim_start().starts_with('#') && para.trim().is_empty() {
                            para.push_str(line.trim());
                            flush(&mut para, &mut out);
                            continue;
                        }
                        if !para.is_empty() && !para.ends_with(char::is_whitespace) {
                            para.push(' ');
                        }
                        para.push_str(line.trim());
                    }
                }
            }
            DocFragment::Code(code) if code.contains('\n') => {
                flush(&mut para, &mut out);
                out.push(Block::Code(code.to_string()));
            }
            DocFragment::Code(code) => {
                para.push('`');
                para.push_str(code);
                para.push('`');
            }
            DocFragment::Link {
                label, coordinate, ..
            } => match coordinate {
                Some(target) => para.push_str(&format!("[{label}]({})", target.as_str())),
                None => para.push_str(label),
            },
            DocFragment::Break => {}
        }
    }
    flush(&mut para, &mut out);
    out
}

fn markup(fragments: &[DocFragment]) -> String {
    blocks(fragments)
        .into_iter()
        .map(|block| match block {
            Block::Para(text) | Block::Item(text) | Block::Head(text) | Block::Code(text) => text,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn section(section: &DocSection) -> Section {
    Section {
        kind: section_kind(section.kind),
        body: markup(&section.body),
        entries: section
            .entries
            .iter()
            .map(|entry| (entry.subject.to_string(), markup(&entry.body)))
            .collect(),
    }
}

fn member(member: &Member, effect: Receives) -> FactMember {
    let docs = blocks(&member.docs);
    let mut paragraphs = docs.iter().filter_map(|block| match block {
        Block::Para(text) => Some(text.clone()),
        _ => None,
    });
    let first = paragraphs.next();
    FactMember {
        name: member.decl.name.to_string(),
        signature: member
            .signature
            .known()
            .map(|signature| signature.text.to_string()),
        summary: member.summary.as_deref().map(ToOwned::to_owned).or(first),
        more: paragraphs.next(),
        receives: effect,
        owes: owes(&member.decl),
        link: Some(member.decl.coordinate.as_str().to_owned()),
        errors: member
            .sections
            .sections
            .iter()
            .find(|s| s.kind == SectionKind::Errors)
            .map(|s| markup(&s.body)),
    }
}

/// The signature's tokens that carry an address: `(text, address)`.
fn links(page: &SymbolPage) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut take = |signature: &crate::model::pages::SignatureText| {
        for token in signature.links() {
            let name = signature.token_text(token).to_owned();
            if let Some(link) = &token.link
                && !out.iter().any(|(known, _)| *known == name)
            {
                out.push((name, link.target.as_str().to_owned()));
            }
        }
    };
    if let Some(signature) = page.signature.known() {
        take(signature);
    }
    if let Some(members) = page.members.known() {
        for member in members.all() {
            if let Some(signature) = member.signature.known() {
                take(signature);
            }
        }
    }
    out
}

/// The language as the page names it: an index that files JavaScript under
/// TypeScript is told apart by the file's extension.
fn lang(page: &SymbolPage) -> Lang {
    let lang = Lang::from_name(page.identity.language.name());
    let javascript = page.identity.path.as_deref().is_some_and(|path| {
        [".js", ".mjs", ".cjs", ".jsx"]
            .iter()
            .any(|ext| path.ends_with(ext))
    });
    if lang == Lang::TypeScript && javascript {
        Lang::JavaScript
    } else {
        lang
    }
}

/// `serde_json`, `value`, `Value`: the package, the file's stem unless it is
/// `lib`, `mod` or `main`, the owner of a method.
fn path(package: &PackageRef, page: &SymbolPage, owner: Option<&str>) -> Vec<String> {
    let mut parts = vec![package.display_name().replace('-', "_")];
    if let Some(stem) = page
        .identity
        .coordinate
        .identity()
        .path()
        .map(|path| path.stem().to_owned())
        && !stem.is_empty()
        && !matches!(stem.as_str(), "lib" | "mod" | "main")
    {
        parts.push(stem);
    }
    if let Some(owner) = owner {
        parts.push(owner.to_owned());
    }
    parts.retain(|part| !part.is_empty());
    parts
}

/// What the page reads of the declaration: everything the index says, and the
/// companions (a Go named type's constants), history and siblings the shell
/// gathered.
pub(super) fn facts(
    page: &SymbolPage,
    package: &str,
    companions: &[(DeclRef, Option<SymbolPage>)],
    history: &History,
) -> Facts {
    let (name, owner) = owner(page);
    let pinned = PackageRef::parse(package).ok();
    let display = pinned.as_ref().map_or_else(
        || package.to_owned(),
        |package| package.display_name().to_owned(),
    );
    let mut facts = Facts::new(&name, kind_of(page.identity.kind), lang(page), &display);
    facts.path = pinned.as_ref().map_or_else(
        || vec![display.clone()],
        |package| path(package, page, owner.as_deref()),
    );
    facts.version = pinned
        .as_ref()
        .and_then(PackageRef::release_version)
        .map(ToOwned::to_owned);
    facts.owner = owner;
    facts.signature = page
        .signature
        .known()
        .map(|signature| signature.text.to_string());
    facts.links = links(page);
    // Docs: the prose before any section, then each section the conventions
    // named; with no sections, the docs whole.
    let lead: Vec<DocFragment> = if page.sections.sections.is_empty() {
        page.docs.to_vec()
    } else {
        page.sections.lead.to_vec()
    };
    facts.docs = blocks(&lead);
    facts.sections = page.sections.sections.iter().map(section).collect();
    if let Some(location) = page.site.location.known() {
        facts.site = Some(Site {
            file: location.path.to_string(),
            line: location.line,
            open: pinned
                .as_ref()
                .and_then(|package| place::absolute(package, &location.path)),
        });
    }
    if let Some(members) = page.members.known() {
        facts.made_of = members
            .made_of
            .iter()
            .map(|m| member(m, Receives::Unknown))
            .collect();
        for group in members.does.iter() {
            facts.does.extend(
                group
                    .members
                    .iter()
                    .map(|m| member(m, receives(group.receiver))),
            );
        }
    }
    // A Go named type's constants, as the index records each one.
    for (decl, companion) in companions {
        facts.made_of.push(FactMember {
            name: decl.name.to_string(),
            signature: companion
                .as_ref()
                .and_then(|page| page.signature.known())
                .map(|signature| signature.text.to_string()),
            summary: companion.as_ref().and_then(|page| {
                blocks(&page.docs).into_iter().find_map(|b| match b {
                    Block::Para(text) => Some(text),
                    _ => None,
                })
            }),
            link: Some(decl.coordinate.as_str().to_owned()),
            ..FactMember::default()
        });
    }
    if let Some(up) = page.rose.up.known() {
        facts.implements = up
            .iter()
            .filter(|relation| {
                relation.decl.kind == Some(DeclarationKind::Trait)
                    || matches!(
                        relation.kind,
                        crate::model::pages::RelationKind::Semantic(
                            backend_library::SemanticLinkKind::Implements
                        )
                    )
            })
            .map(|relation| {
                (
                    relation.decl.name.to_string(),
                    matches!(relation.arrival, crate::model::pages::Arrival::Auto),
                )
            })
            .collect();
    }
    facts.implementors = page.rose.implemented_by.known().and_then(|doers| {
        let crates: std::collections::BTreeSet<_> = doers
            .iter()
            .filter_map(|relation| relation.decl.coordinate.package())
            .collect();
        Some(Implementors {
            total: u32::try_from(doers.len()).ok().filter(|n| *n > 0)?,
            crates: u32::try_from(crates.len().max(1)).ok()?,
            derived: None,
        })
    });
    if let Some(outline) = page.outline.known() {
        facts.beside = outline
            .siblings
            .iter()
            // What the package declares beside it (not `&str`, not `crate`).
            .filter(|decl| crate::shell::kit::names_a_declaration(&decl.name))
            .take(96)
            .map(|decl| Beside {
                name: decl.name.to_string(),
                kind: kind_of(decl.kind),
                signature: None,
                link: Some(decl.coordinate.as_str().to_owned()),
            })
            .collect();
    }
    let read: Vec<_> = history.read().collect();
    if read.len() > 1 {
        facts.releases = read
            .iter()
            .map(|release| {
                (
                    release.version.clone(),
                    matches!(release.was, Was::Differs(_) | Was::NotYet | Was::Gone),
                )
            })
            .collect();
        facts.across = Some(history.caption()).filter(|_| {
            read.iter()
                .any(|release| matches!(release.was, Was::Differs(_) | Was::NotYet | Was::Gone))
        });
    }
    facts
}
