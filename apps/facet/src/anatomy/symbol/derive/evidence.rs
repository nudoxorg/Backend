//! Evidence comes from the same typed values the page paints, never a
//! package-wide assumption about Python, JavaScript, or another language.

use super::super::view::{Call, Generic, Origin, Shape, TypeEvidence};

pub(super) fn types(call: Option<&Call>, generics: &[Generic], shape: Option<&Shape>) -> TypeEvidence {
    let mut evidence = TypeEvidence::NotShown;
    let mut record = |origin: Origin| {
        let next = match origin {
            Origin::Declared => TypeEvidence::Declared,
            Origin::Docs => TypeEvidence::Docs,
            Origin::Code => TypeEvidence::Code,
        };
        evidence = match evidence {
            TypeEvidence::NotShown => next,
            current if current == next => current,
            _ => TypeEvidence::Mixed,
        };
    };
    if let Some(call) = call {
        for port in &call.ports {
            record(port.ty.origin);
            for option in &port.options { record(option.ty.origin); }
        }
        if let Some(ty) = &call.gives.ty { record(ty.origin); }
        if let Some(failure) = &call.fails { record(failure.ty.origin); }
    }
    for generic in generics { record(generic.origin); }
    match shape {
        Some(Shape::OneOf(cases)) => {
            for case in cases { for ty in &case.holds { record(ty.origin); } }
        }
        Some(Shape::Holds { fields, .. }) => {
            for field in fields { record(field.ty.origin); }
        }
        Some(Shape::Write { rows, .. }) => {
            for row in rows { record(row.takes.origin); }
        }
        None => {}
    }
    evidence
}

#[cfg(test)]
mod tests {
    use crate::anatomy::symbol::{Facts, compile};
    use super::super::super::view::{Kind, Lang, Origin, TypeEvidence};

    #[test]
    fn absent_declared_docs_code_and_mixed_types_keep_distinct_origin_evidence() {
        use super::super::super::view::{Generic, Role};
        assert_eq!(super::types(None, &[], None), TypeEvidence::NotShown);
        assert!(TypeEvidence::NotShown.explanation().is_none());
        let generic = |origin| Generic {
            name: "T".into(), role: Role::Needs, says: String::new(), bounds: vec![], origin,
        };
        for (origin, evidence) in [
            (Origin::Declared, TypeEvidence::Declared),
            (Origin::Docs, TypeEvidence::Docs),
            (Origin::Code, TypeEvidence::Code),
        ] {
            assert_eq!(super::types(None, &[generic(origin)], None), evidence);
            assert_eq!(evidence.explanation().is_some(), origin.dotted());
        }
        assert_eq!(super::types(None, &[generic(Origin::Docs), generic(Origin::Code)], None), TypeEvidence::Mixed);
        assert_eq!(super::types(None, &[generic(Origin::Declared), generic(Origin::Docs)], None), TypeEvidence::Mixed);
        assert_eq!(super::types(None, &[generic(Origin::Code), generic(Origin::Declared)], None), TypeEvidence::Mixed);
    }

    #[test]
    fn real_fastapi_annotations_remain_declared_in_inputs_output_and_explanation() {
        // Pinned FastAPI e99fbaeb: items.py14 and security.py22. These are
        // exactly the signatures whose actual db02 pages claimed no types.
        for (name, signature, inputs) in [
            ("read_items", "def read_items(session: SessionDep, current_user: CurrentUser, skip: int = 0, limit: int = 100) -> Any:", 4),
            ("create_access_token", "def create_access_token(subject: str | Any, expires_delta: timedelta) -> str:", 2),
        ] {
            let mut facts = Facts::new(name, Kind::Function, Lang::Python, "fastapi-full-stack");
            facts.signature = Some(signature.into());
            let view = compile(&facts);
            let call = view.call.expect("actual annotated Python call");
            assert_eq!(call.ports.len(), inputs);
            assert!(call.ports.iter().all(|port| port.ty.origin == Origin::Declared));
            assert_eq!(call.gives.ty.expect("written return annotation").origin, Origin::Declared);
            assert_eq!(view.rail.type_evidence, TypeEvidence::Declared);
            assert!(view.rail.type_evidence.explanation().is_none());
        }
    }

    #[test]
    fn a_mixed_annotation_and_default_keeps_each_origin_instead_of_a_language_claim() {
        let mut facts = Facts::new("mixed", Kind::Function, Lang::Python, "fastapi-full-stack");
        facts.signature = Some("def mixed(session: SessionDep, limit=100) -> str:".into());
        let view = compile(&facts);
        let call = view.call.expect("mixed call");
        assert_eq!(call.ports[0].ty.origin, Origin::Declared);
        assert_eq!(call.ports[1].ty.origin, Origin::Code);
        assert_eq!(view.rail.type_evidence, TypeEvidence::Mixed);
        assert!(view.rail.type_evidence.explanation().is_some_and(|note| note.contains("Written annotations remain declared")));
    }
}
