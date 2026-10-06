//! Per-program semantic policies carried by the native query database.
//!
//! These options affect type construction and relation queries. They are
//! immutable for one TypeScript program and must travel with its
//! `TypeDatabase`; process environment variables are deliberately not consulted.

/// Identity assigned to a user-declared type parameter.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum TypeParamOriginMode {
    /// Preserve the historical structural identity for equal type parameters.
    #[default]
    Structural,
    /// Include the declaring source file and name-node identity.
    DeclarationScoped,
}

/// Type-construction and relation policies for one semantic program.
///
/// Declaration-scoped identity is kept independent from the two follow-up
/// relation experiments. Turning on identity alone never enables either
/// reduction or higher-kinded return-context relaxation.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub struct ProjectSemanticOptions {
    type_param_origin: TypeParamOriginMode,
    declaration_origin_reduction: bool,
    hkt_application_unknown_drop: bool,
}

impl ProjectSemanticOptions {
    /// Uses the legacy structural identity and disables both follow-up relation policies.
    #[must_use]
    pub const fn structural() -> Self {
        Self {
            type_param_origin: TypeParamOriginMode::Structural,
            declaration_origin_reduction: false,
            hkt_application_unknown_drop: false,
        }
    }

    /// Uses declaration-scoped identities while keeping follow-up relation policies disabled.
    #[must_use]
    pub const fn declaration_scoped() -> Self {
        Self {
            type_param_origin: TypeParamOriginMode::DeclarationScoped,
            declaration_origin_reduction: false,
            hkt_application_unknown_drop: false,
        }
    }

    /// Returns a copy with the separately evaluated declaration-origin reduction enabled or
    /// disabled. This policy has no effect unless origin mode is declaration-scoped.
    #[must_use]
    pub const fn with_declaration_origin_reduction(mut self, enabled: bool) -> Self {
        self.declaration_origin_reduction = enabled;
        self
    }

    /// Returns a copy with the separately evaluated HKT application return-context recovery
    /// enabled or disabled. This policy has no effect unless origin mode is declaration-scoped.
    #[must_use]
    pub const fn with_hkt_application_unknown_drop(mut self, enabled: bool) -> Self {
        self.hkt_application_unknown_drop = enabled;
        self
    }

    /// Returns the type-parameter identity policy.
    #[must_use]
    pub const fn type_param_origin(self) -> TypeParamOriginMode {
        self.type_param_origin
    }

    /// Whether the independently selected declaration-origin reduction is enabled.
    #[must_use]
    pub const fn declaration_origin_reduction(self) -> bool {
        self.type_param_origin == TypeParamOriginMode::DeclarationScoped
            && self.declaration_origin_reduction
    }

    /// Whether the independently selected HKT return-context recovery is enabled.
    #[must_use]
    pub const fn hkt_application_unknown_drop(self) -> bool {
        self.type_param_origin == TypeParamOriginMode::DeclarationScoped
            && self.hkt_application_unknown_drop
    }
}

#[cfg(test)]
mod tests {
    use super::{ProjectSemanticOptions, TypeParamOriginMode};

    #[test]
    fn declaration_identity_does_not_enable_relation_experiments() {
        let options = ProjectSemanticOptions::declaration_scoped();
        assert_eq!(
            options.type_param_origin(),
            TypeParamOriginMode::DeclarationScoped
        );
        assert!(!options.declaration_origin_reduction());
        assert!(!options.hkt_application_unknown_drop());
    }

    #[test]
    fn relation_experiments_require_declaration_scoped_identity() {
        let options = ProjectSemanticOptions::structural()
            .with_declaration_origin_reduction(true)
            .with_hkt_application_unknown_drop(true);
        assert_eq!(options.type_param_origin(), TypeParamOriginMode::Structural);
        assert!(!options.declaration_origin_reduction());
        assert!(!options.hkt_application_unknown_drop());
    }
}
