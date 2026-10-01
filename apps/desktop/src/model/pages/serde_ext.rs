//! Persisting page models (W-Open I2, the launch snapshot): serde adapters
//! for the few producer types the page models embed that carry no serde of
//! their own. Each goes through the type's own stable spelling (a wire tag
//! or a lowercase name), so an unknown spelling fails the decode (the
//! snapshot is then ignored), never becomes a different value.

use backend_library::{DeclarationKind, Obligation};
use backend_present::{Language, SectionKind};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::Known;

/// `Option<DeclarationKind>` as its canonical wire tag.
pub(crate) mod declaration_kind {
    use super::*;

    #[allow(
        clippy::trivially_copy_pass_by_ref,
        clippy::ref_option,
        reason = "serde's `with` passes the field by reference"
    )]
    pub(crate) fn serialize<S: Serializer>(
        kind: &Option<DeclarationKind>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        kind.map(DeclarationKind::wire_tag).serialize(to)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        from: D,
    ) -> Result<Option<DeclarationKind>, D::Error> {
        Option::<u8>::deserialize(from)?
            .map(|tag| {
                DeclarationKind::from_wire_tag(tag)
                    .ok_or_else(|| D::Error::custom(format!("declaration kind tag {tag}")))
            })
            .transpose()
    }
}

/// `Known<Option<Obligation>>` with each obligation as its canonical tag.
pub(crate) mod obligation {
    use super::*;

    pub(crate) fn serialize<S: Serializer>(
        value: &Known<Option<Obligation>>,
        to: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Known::Known(obligation) => {
                Known::Known(obligation.map(Obligation::wire_tag)).serialize(to)
            }
            Known::Unknown(gap) => Known::<Option<u8>>::Unknown(gap.clone()).serialize(to),
        }
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        from: D,
    ) -> Result<Known<Option<Obligation>>, D::Error> {
        Ok(match Known::<Option<u8>>::deserialize(from)? {
            Known::Known(tag) => Known::Known(
                tag.map(|tag| {
                    Obligation::from_wire_tag(tag)
                        .map_err(|_| D::Error::custom(format!("obligation tag {tag}")))
                })
                .transpose()?,
            ),
            Known::Unknown(gap) => Known::Unknown(gap),
        })
    }
}

/// `Language` as its stable name.
pub(crate) mod language {
    use super::*;

    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` passes the field by reference"
    )]
    pub(crate) fn serialize<S: Serializer>(language: &Language, to: S) -> Result<S::Ok, S::Error> {
        language.name().serialize(to)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<Language, D::Error> {
        let name = String::deserialize(from)?;
        if name == Language::Unknown.name() {
            return Ok(Language::Unknown);
        }
        Language::ALL
            .into_iter()
            .find(|language| language.name() == name)
            .ok_or_else(|| D::Error::custom(format!("language {name}")))
    }
}

/// `SectionKind` as its stable name.
pub(crate) mod section_kind {
    use super::*;

    const ALL: [SectionKind; 8] = [
        SectionKind::Errors,
        SectionKind::Panics,
        SectionKind::Safety,
        SectionKind::Examples,
        SectionKind::Returns,
        SectionKind::Parameters,
        SectionKind::Deprecated,
        SectionKind::Other,
    ];

    #[allow(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde's `with` passes the field by reference"
    )]
    pub(crate) fn serialize<S: Serializer>(kind: &SectionKind, to: S) -> Result<S::Ok, S::Error> {
        kind.name().serialize(to)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(from: D) -> Result<SectionKind, D::Error> {
        let name = String::deserialize(from)?;
        ALL.into_iter()
            .find(|kind| kind.name() == name)
            .ok_or_else(|| D::Error::custom(format!("section kind {name}")))
    }
}
