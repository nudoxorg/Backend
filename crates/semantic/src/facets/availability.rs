use super::*;

/// Lexical field kind.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LexicalKind {
    /// Identifier or text token.
    Token,
    /// Language keyword.
    Keyword,
    /// Literal token.
    Literal,
    /// Comment token.
    Comment,
}

pub(crate) fn lexical_kind_tag(value: LexicalKind) -> u8 {
    match value {
        LexicalKind::Token => 1,
        LexicalKind::Keyword => 2,
        LexicalKind::Literal => 3,
        LexicalKind::Comment => 4,
    }
}

/// Lexical fact kept as a compact compatibility value.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct LexicalFact {
    /// Entity owning the text.
    pub entity: EntityId,
    /// Lexical field.
    pub kind: LexicalKind,
    /// Canonical token text.
    pub text: String,
}

/// Visibility state retaining unknown/unavailable distinct from private.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum VisibilityState {
    /// Public/exported.
    Public,
    /// Private/non-exported.
    Private,
    /// Visibility not represented by the authority.
    Unavailable,
}

/// Visibility compatibility fact.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct VisibilityFact {
    /// Entity whose visibility is observed.
    pub entity: EntityId,
    /// Whether the entity is public.
    pub public: bool,
}
