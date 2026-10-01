//! A contract read in its own language, into one shape: the socket. What an
//! implementor must write are notches, what it may write dotted notches,
//! what it gets for free tabs; fields it holds sit inside. Who does it
//! plugs in from the left margin.
//!
//! - Rust `trait`: bodiless methods are notches, default methods tabs.
//! - TypeScript `interface` with methods: every method is a notch (`m?()` a
//!   dotted one), properties are held; an `abstract class`: `abstract`
//!   members are notches, concrete ones tabs.
//! - Go `interface`: every method is a notch; Go has no provided methods,
//!   so no tab is drawn. Satisfaction is implicit: the plug is computed.
//! - Python `Protocol`/ABC, Java and C# interfaces, C++ pure virtuals: the
//!   producer's obligation facts say which is which.

use super::{Contract, DeclKind, Lang, Owes, Rung, Slot, Source, SourceMember, callable, field_of};

/// The socket for `source`, when it is a contract.
pub(super) fn contract(source: &Source) -> Option<Contract> {
    let members = source
        .made_of
        .iter()
        .chain(source.does.iter())
        .collect::<Vec<_>>();
    let callables = members
        .iter()
        .filter(|member| member.kind != DeclKind::Field)
        .count();
    let abstract_class = source.kind == DeclKind::Class
        && members.iter().any(|member| member.owes == Owes::Required);
    let protocol = source.lang == Lang::Python
        && source
            .extends
            .iter()
            .any(|base| matches!(base.rsplit('.').next().unwrap_or(base), "Protocol" | "ABC"));
    let is_contract = match source.kind {
        DeclKind::Trait => true,
        DeclKind::Interface => callables > 0,
        DeclKind::Class => abstract_class || protocol,
        // Go records an interface as a named type: `Value interface`.
        DeclKind::Alias | DeclKind::Other => {
            source.lang == Lang::Go
                && source.signature.as_deref().is_some_and(|sig| {
                    sig.trim_end().ends_with("interface") || sig.contains("interface {")
                })
        }
        _ => false,
    };
    if !is_contract {
        return None;
    }
    // A Go or TypeScript interface method owes by the language's own rule.
    let by_rule = |member: &SourceMember| match (member.owes, source.lang, source.kind) {
        (Owes::Unknown, Lang::Go, _)
        | (Owes::Unknown, Lang::TypeScript | Lang::Java, DeclKind::Interface) => Owes::Required,
        (owes, ..) => owes,
    };
    let mut contract = Contract {
        write: Vec::new(),
        get: Vec::new(),
        other: Vec::new(),
        held: Vec::new(),
        doers: source.doers.clone(),
    };
    let mut seen = std::collections::BTreeSet::new();
    for member in members {
        if !seen.insert(member.name.clone()) {
            continue;
        }
        if member.kind == DeclKind::Field {
            if let Some((ty, optional, readonly)) = field_of(member, source.lang) {
                contract.held.push(Rung {
                    name: member.name.clone(),
                    ty,
                    optional,
                    readonly,
                    doc: member.summary.clone(),
                    deprecated: member.deprecated.is_some(),
                });
            }
            continue;
        }
        let pipe = member.signature.as_deref().and_then(|signature| {
            // `abstract`, `public`, `readonly`: modifiers say nothing of the shape.
            let mut signature = signature.trim();
            while let Some(rest) = [
                "abstract ",
                "public ",
                "protected ",
                "override ",
                "readonly ",
                "virtual ",
            ]
            .iter()
            .find_map(|word| signature.strip_prefix(word))
            {
                signature = rest.trim_start();
            }
            let probe = Source {
                name: member.name.clone(),
                owner: None,
                kind: DeclKind::Method,
                signature: Some(signature.to_owned()),
                failures: Vec::new(),
                ..source.clone()
            };
            callable::callable(&probe)
        });
        let fails = pipe.as_ref().is_some_and(|pipe| !pipe.drops.is_empty());
        let gives = pipe.and_then(|pipe| pipe.gives);
        let owes = by_rule(member);
        let slot = Slot {
            name: member.name.clone(),
            gives,
            fails,
            optional: owes == Owes::Optional,
            doc: member.summary.clone(),
            link: member.link.clone(),
        };
        match owes {
            Owes::Required | Owes::Optional => contract.write.push(slot),
            Owes::Provided => contract.get.push(slot),
            Owes::Unknown => contract.other.push(slot),
        }
    }
    Some(contract)
}
