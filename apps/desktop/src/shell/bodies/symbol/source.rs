//! What the page is compiled from: the index's page for the declaration,
//! the pinned world's anatomy when it knows the declaration (Rust today),
//! and the companion pages (a Go named type's constants), read into facet's
//! language-neutral [`Source`]. `facet::anatomy::plan::compile` does the
//! rest, purely.

use super::presentation::{WorldPacket, is_failure};
use crate::model::pages::{DeclRef, Member, Receiver, SymbolPage};
use crate::runtime::fixture_world::Anatomy;
use backend_library::DeclarationKind;
use facet::anatomy::plan::{self, DeclKind, Deprecated, Effect, Lang, SectionFacts, SectionId, Source, SourceMember, SourceRail, Tier};
use facet::semantics::Word;

/// The plan's kind for an index kind.
pub(super) const fn decl_kind(kind: Option<DeclarationKind>) -> DeclKind {
    match kind {
        Some(DeclarationKind::Struct) => DeclKind::Struct,
        Some(DeclarationKind::Class) => DeclKind::Class,
        Some(DeclarationKind::Interface) => DeclKind::Interface,
        Some(DeclarationKind::Trait) => DeclKind::Trait,
        Some(DeclarationKind::Enum) => DeclKind::Enum,
        Some(DeclarationKind::Union) => DeclKind::Union,
        Some(DeclarationKind::Type) => DeclKind::Alias,
        Some(DeclarationKind::Function) => DeclKind::Function,
        Some(DeclarationKind::Method | DeclarationKind::Constructor) => DeclKind::Method,
        Some(DeclarationKind::Constant | DeclarationKind::Variable) => DeclKind::Constant,
        Some(DeclarationKind::Module) => DeclKind::Module,
        Some(DeclarationKind::Field | DeclarationKind::Property) => DeclKind::Field,
        Some(DeclarationKind::Variant) => DeclKind::Variant,
        _ => DeclKind::Other,
    }
}

const fn effect(receiver: Receiver) -> Effect {
    match receiver {
        Receiver::Reads => Effect::Reads,
        Receiver::Changes => Effect::Changes,
        Receiver::Consumes => Effect::UsesUp,
        Receiver::Makes => Effect::Makes,
        Receiver::Unknown => Effect::None,
    }
}

fn deprecated(decl: &DeclRef) -> Option<Deprecated> {
    decl.facts.deprecated().map(|notice| Deprecated {
        since: notice.since.as_deref().map(ToOwned::to_owned),
        note: notice.note.as_deref().map(ToOwned::to_owned),
    })
}

fn member(member: &Member, effect: Effect) -> SourceMember {
    SourceMember {
        name: member.decl.name.to_string(),
        kind: decl_kind(member.decl.kind),
        signature: member.signature.known().map(|signature| signature.text.to_string()),
        summary: member.summary.as_deref().map(ToOwned::to_owned),
        deprecated: deprecated(&member.decl),
        effect,
        link: Some(member.decl.coordinate.as_str().to_owned()),
    }
}

/// The type a method belongs to: `FlagSet` for `FlagSet.Parse`.
fn owner(page: &SymbolPage) -> (String, Option<String>) {
    let name = page.identity.name.to_string();
    if let Some((owner, leaf)) = name.rsplit_once('.').filter(|(owner, leaf)| !owner.is_empty() && !leaf.is_empty()) {
        return (leaf.to_owned(), Some(owner.to_owned()));
    }
    if matches!(page.identity.kind, Some(DeclarationKind::Method)) {
        let identity = page.identity.coordinate.identity();
        let segments = identity.trail().segments().iter().map(|segment| segment.as_str().to_owned()).collect::<Vec<_>>();
        if segments.len() >= 2 {
            return (name, segments.get(segments.len() - 2).cloned());
        }
    }
    (name, None)
}

/// What the world knows about how to get one: its recipe's rails, and how
/// many ways it counts (the rails plus every maker it lists).
fn world_rails(anatomy: &Anatomy) -> Vec<SourceRail> {
    let Some(recipe) = &anatomy.recipe else { return Vec::new() };
    recipe
        .rails
        .iter()
        .filter_map(|rail| {
            let step = rail.steps.first()?;
            let from = match rail.starts.first() {
                Some(port) => plan::spelled(&port.ty, &port.ty.iter().map(facet::semantics::types::Piece::text).collect::<String>()),
                None => plan::spelled(&rail.lead, ""),
            };
            Some(SourceRail {
                from,
                verb: step.verb.to_string(),
                fails: step.fails,
                maybe: step.maybe,
                lands: None,
                code: Some(rail.code.to_string()).filter(|code| !code.trim().is_empty()),
                link: None,
            })
        })
        .collect()
}

/// The page's source.
pub(super) fn source(
    page: &SymbolPage,
    package: &str,
    lede: Option<String>,
    anatomy: Option<&Anatomy>,
    packet: Option<&WorldPacket>,
    companions: &[(DeclRef, Option<SymbolPage>)],
) -> Source {
    let (name, owner) = owner(page);
    let members = page.members.known();
    let mut made_of = members.map(|members| members.made_of.iter().map(|m| member(m, Effect::None)).collect::<Vec<_>>()).unwrap_or_default();
    // A Go named type's constants, as the index records each one.
    for (decl, companion) in companions {
        made_of.push(SourceMember {
            name: decl.name.to_string(),
            kind: DeclKind::Constant,
            signature: companion.as_ref().and_then(|page| page.signature.known()).map(|signature| signature.text.to_string()),
            summary: companion.as_ref().and_then(|page| super::teaser_source(&page.docs)).map(|sentence| super::plain_markup(&sentence)),
            deprecated: deprecated(decl),
            effect: Effect::None,
            link: Some(super::companions::link(&decl.coordinate)),
        });
    }
    let does = members
        .map(|members| members.does.iter().flat_map(|group| group.members.iter().map(move |m| member(m, effect(group.receiver)))).collect::<Vec<_>>())
        .unwrap_or_default();
    let extends = page.rose.up.known().map(|up| up.iter().filter(|relation| relation.decl.kind != Some(DeclarationKind::Trait)).map(|relation| relation.decl.name.to_string()).collect()).unwrap_or_default();
    let rails = anatomy.map(world_rails).unwrap_or_default();

    // What each section has to say, and what its stub counts.
    let mut sections = Vec::new();
    let made_by = packet.map_or(0, |packet| packet.relations.iter().filter(|row| row.word == Word::MadeBy).map(|row| row.names.len()).sum::<usize>());
    let makers = does.iter().filter(|member| member.effect == Effect::Makes).count();
    let ways = if anatomy.is_some() { rails.len().max(made_by) } else { makers };
    if ways > 0 {
        sections.push(SectionFacts { id: SectionId::Getting, count: u32::try_from(ways).ok(), yours: None, tier: Tier::Compiler });
    }
    let own = does.iter().filter(|member| member.effect != Effect::Makes).count();
    let caps = anatomy.map_or(0, |anatomy| anatomy.page.caps.len());
    if own + caps > 0 {
        sections.push(SectionFacts { id: SectionId::Does, count: u32::try_from(own).ok(), yours: None, tier: Tier::Compiler });
    }
    let documented = page.sections.sections.iter().any(|section| is_failure(section.kind))
        || members.is_some_and(|members| members.all().any(|m| m.sections.sections.iter().any(|section| is_failure(section.kind))));
    let traced = packet.is_some_and(|packet| packet.failure.is_some() || packet.callable_failure.as_ref().is_some_and(|line| line.all > 0));
    if documented || traced {
        sections.push(SectionFacts { id: SectionId::Fails, count: None, yours: None, tier: Tier::Compiler });
    }
    let (uses, yours) = match (packet, page.references.known()) {
        (Some(packet), _) => {
            let rows = packet.relations.iter().filter(|row| matches!(row.word, Word::UsedBy | Word::TakenBy | Word::HeldBy | Word::CalledFrom | Word::CallsIt));
            rows.fold((0, 0), |(n, yours), row| (n + row.names.len(), yours + row.yours))
        }
        (None, Some(sites)) => (sites.len(), 0),
        (None, None) => (0, 0),
    };
    if uses > 0 || anatomy.is_some_and(|anatomy| !anatomy.uses.is_empty()) {
        sections.push(SectionFacts { id: SectionId::Uses, count: u32::try_from(uses).ok(), yours: u32::try_from(yours).ok(), tier: Tier::Compiler });
    }
    if super::has_words(page) {
        sections.push(SectionFacts { id: SectionId::Words, count: None, yours: None, tier: Tier::Compiler });
    }

    Source {
        name,
        owner,
        kind: decl_kind(page.identity.kind),
        lang: Lang::from_name(page.identity.language.name()),
        package: package.to_owned(),
        version: None,
        module: String::new(),
        signature: page.signature.known().map(|signature| signature.text.to_string()),
        lede,
        deprecated: deprecated(&page.identity),
        made_of,
        extends,
        sections,
        does,
        rails,
        since: None,
        yours: false,
    }
}
