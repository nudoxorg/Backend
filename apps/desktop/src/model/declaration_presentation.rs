//! One closed projection from producer declaration kinds to GUI vocabulary.
//!
//! Page and Graph retain their existing coarser type/callable families. Their
//! classification is explicit here, alongside the exact search/outline mark,
//! so a value cannot become a constant in one surface and a variable in another.
//! This projection owns no source capability or declaration identity.

use backend_library::DeclarationKind;
use facet::anatomy::symbol::view::Kind as PageKind;
use facet::graph::Kind as GraphKind;
use facet::icons::Kind as IconKind;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct DeclarationPresentation {
    icon: IconKind,
    page: PageKind,
    graph: GraphKind,
}

impl DeclarationPresentation {
    pub(crate) const fn of(kind: Option<DeclarationKind>) -> Self {
        use DeclarationKind as D;
        use GraphKind as G;
        use IconKind as I;
        use PageKind as P;
        match kind {
            Some(D::Module) => Self::new(I::Module, P::Module, G::Other),
            Some(D::Class) => Self::new(I::Class, P::Struct, G::Struct),
            Some(D::Function) => Self::new(I::Function, P::Function, G::Function),
            Some(D::Method) => Self::new(I::Method, P::Method, G::Method),
            Some(D::Interface) => Self::new(I::Interface, P::Trait, G::Trait),
            Some(D::Type) => Self::new(I::Type, P::Alias, G::Type),
            Some(D::Macro) => Self::new(I::Macro, P::Other, G::Macro),
            Some(D::Constant) => Self::new(I::Constant, P::Constant, G::Constant),
            Some(D::Field) => Self::new(I::Field, P::Other, G::Field),
            Some(D::Property) => Self::new(I::Property, P::Other, G::Method),
            Some(D::Constructor) => Self::new(I::Constructor, P::Method, G::Function),
            Some(D::Enum) => Self::new(I::Enum, P::Enum, G::Enum),
            Some(D::Struct) => Self::new(I::Struct, P::Struct, G::Struct),
            Some(D::Trait) => Self::new(I::Trait, P::Trait, G::Trait),
            Some(D::Union) => Self::new(I::Union, P::Struct, G::Union),
            Some(D::Variable) => Self::new(I::Variable, P::Variable, G::Variable),
            Some(D::Import) => Self::new(I::Import, P::Other, G::Other),
            Some(D::Variant) => Self::new(I::Variant, P::Other, G::Variant),
            Some(D::Unknown) | None => Self::new(I::Unknown, P::Other, G::Other),
        }
    }

    const fn new(icon: IconKind, page: PageKind, graph: GraphKind) -> Self {
        Self { icon, page, graph }
    }

    pub(crate) const fn icon(self) -> IconKind {
        self.icon
    }
    pub(crate) const fn page(self) -> PageKind {
        self.page
    }
    pub(crate) const fn graph(self) -> GraphKind {
        self.graph
    }

    /// A graph definition's coarse kind. Module/import/unknown roles remain
    /// separate facts on the graph node; this never resolves an indexed address.
    pub(crate) const fn graph_definition(kind: GraphKind) -> DeclarationKind {
        match kind {
            GraphKind::Struct => DeclarationKind::Struct,
            GraphKind::Enum => DeclarationKind::Enum,
            GraphKind::Union => DeclarationKind::Union,
            GraphKind::Trait => DeclarationKind::Trait,
            GraphKind::Type => DeclarationKind::Type,
            GraphKind::Function => DeclarationKind::Function,
            GraphKind::Method => DeclarationKind::Method,
            GraphKind::Macro => DeclarationKind::Macro,
            GraphKind::Constant => DeclarationKind::Constant,
            GraphKind::Field => DeclarationKind::Field,
            GraphKind::Variant => DeclarationKind::Variant,
            GraphKind::Other => DeclarationKind::Unknown,
            GraphKind::Variable => DeclarationKind::Variable,
        }
    }

    pub(crate) const fn graph_icon(kind: GraphKind) -> IconKind {
        facet::anatomy::icon_kind(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_producer_vocabulary_has_one_explicit_presentation() {
        let kinds = (0..=u8::MAX)
            .filter_map(DeclarationKind::from_wire_tag)
            .collect::<Vec<_>>();
        assert_eq!(kinds.len(), 19);
        for kind in kinds {
            assert_eq!(
                DeclarationPresentation::of(Some(kind)).icon().name(),
                kind.name(),
                "the exact mark is shared by search, outline and peeks: {kind:?}"
            );
        }
        assert_eq!(
            DeclarationPresentation::of(None),
            DeclarationPresentation::of(Some(DeclarationKind::Unknown))
        );
    }

    #[test]
    fn variable_and_constant_keep_distinct_words_marks_and_graph_selection() {
        let variable = DeclarationPresentation::of(Some(DeclarationKind::Variable));
        let constant = DeclarationPresentation::of(Some(DeclarationKind::Constant));
        assert_eq!(variable.page().word(), "variable");
        assert_eq!(variable.graph().text(), "variable");
        assert_eq!(variable.icon().name(), "variable");
        assert_eq!(constant.page().word(), "constant");
        assert_eq!(constant.graph().text(), "constant");
        assert!(!variable.page().callable());
        assert!(!variable.graph().is_type_like());
        assert_eq!(
            DeclarationPresentation::graph_definition(variable.graph()),
            DeclarationKind::Variable
        );
        assert_eq!(
            DeclarationPresentation::graph_icon(variable.graph()),
            variable.icon()
        );
        assert_eq!(
            crate::shell::kit::world_kind(variable.graph()),
            variable.icon()
        );
        assert_ne!(
            crate::shell::kit::world_kind(variable.graph()),
            constant.icon()
        );
        assert_eq!(GraphKind::parse("variable"), variable.graph());
        assert_eq!(
            facet::graph::layout::core(variable.graph()),
            facet::graph::layout::core(constant.graph())
        );
    }

    #[test]
    fn desktop_world_marks_share_the_component_projection_for_every_producer_kind() {
        for kind in (0..=u8::MAX).filter_map(DeclarationKind::from_wire_tag) {
            let graph = DeclarationPresentation::of(Some(kind)).graph();
            let mark = facet::anatomy::icon_kind(graph);
            assert_eq!(DeclarationPresentation::graph_icon(graph), mark, "{kind:?}");
            assert_eq!(crate::shell::kit::world_kind(graph), mark, "{kind:?}");
        }
    }
}
