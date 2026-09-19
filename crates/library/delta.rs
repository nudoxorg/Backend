//! User intents and portable derived transition vocabulary.
//!
//! Intent identities are schema-marked object versions derived from the
//! complete logical intent payload. Relation transition identities remain the
//! checked `backend_version::ViewDeltaId<ViewRelation>` values produced by view
//! preparation; the two identity classes are never interchanged.

use crate::canonical::{
    ActorKey, IntentId, ObjectSchema, PackageKey, SemanticObject, ViewRecipeId, ViewRecipeSchema,
    admit_key_value, admit_version_value, intent_id,
};
use std::collections::BTreeMap;

/// Mutable user intent. Semantic objects and derived views are never
/// last-writer-wins merged here.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Intent {
    /// Request one package coordinate.
    RequestPackage {
        /// Logical package identity.
        id: PackageKey,
        /// Stable durable intent/idempotency identity.
        request: IntentId,
    },
    /// Remove one package coordinate.
    RemovePackage {
        /// Logical package identity.
        id: PackageKey,
        /// Stable durable intent/idempotency identity.
        request: IntentId,
    },
    /// Set one user preference.
    SetPreference {
        /// Actor owning this mutable intent.
        actor: ActorKey,
        /// Preference key.
        key: String,
        /// Preference value.
        value: String,
        /// Stable durable intent/idempotency identity.
        request: IntentId,
    },
}

impl Intent {
    /// Returns this intent's durable idempotency identity.
    #[must_use]
    pub const fn id(&self) -> IntentId {
        match self {
            Self::RequestPackage { request, .. }
            | Self::RemovePackage { request, .. }
            | Self::SetPreference { request, .. } => *request,
        }
    }

    /// Constructs an add-package intent with an identity derived from the
    /// complete logical payload.
    #[must_use]
    pub fn request_package(id: PackageKey) -> Self {
        let mut payload = Vec::new();
        payload.extend_from_slice(id.as_bytes());
        Self::RequestPackage {
            id,
            request: intent_id("request_package", &payload),
        }
    }

    /// Constructs a remove-package intent with an identity derived from the
    /// complete logical payload.
    #[must_use]
    pub fn remove_package(id: PackageKey) -> Self {
        let mut payload = Vec::new();
        payload.extend_from_slice(id.as_bytes());
        Self::RemovePackage {
            id,
            request: intent_id("remove_package", &payload),
        }
    }

    /// Constructs a preference intent with an identity derived from the
    /// complete actor/key/value payload.
    #[must_use]
    pub fn set_preference(
        actor: ActorKey,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        let key = key.into();
        let value = value.into();
        let mut payload = Vec::new();
        payload.extend_from_slice(actor.as_bytes());
        append_text(&mut payload, &key);
        append_text(&mut payload, &value);
        Self::SetPreference {
            actor,
            key,
            value,
            request: intent_id("set_preference", &payload),
        }
    }
}

fn append_text(out: &mut Vec<u8>, text: &str) {
    out.extend_from_slice(&(text.len() as u64).to_be_bytes());
    out.extend_from_slice(text.as_bytes());
}

/// Mutable settings whose merge policy belongs to the user-intent layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    /// Actor owning the setting.
    pub actor: ActorKey,
    /// Setting key.
    pub key: String,
    /// Setting value.
    pub value: String,
}

/// An append result that distinguishes replay from a new durable intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntentReceipt {
    /// Idempotency identity that was looked up.
    pub id: IntentId,
    /// Whether the log acquired a new entry.
    pub inserted: bool,
}

/// Failure while appending a durable intent.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntentError {
    /// The same idempotency key was presented with a different payload.
    IdempotencyConflict {
        /// Conflicting durable identity.
        id: IntentId,
    },
}

impl core::fmt::Display for IntentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::IdempotencyConflict { .. } => {
                f.write_str("intent idempotency key conflicts with its original payload")
            }
        }
    }
}

impl std::error::Error for IntentError {}

/// Portable idempotent intent log. An engine can persist its entries; no I/O
/// occurs here.
#[derive(Clone, Debug, Default)]
pub struct IntentLog {
    entries: BTreeMap<IntentId, Intent>,
}

impl IntentLog {
    /// Creates an empty intent log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one idempotent intent.
    ///
    /// # Errors
    ///
    /// Returns [`IntentError::IdempotencyConflict`] when an existing key is
    /// paired with a different logical payload.
    pub fn append(&mut self, intent: Intent) -> Result<IntentReceipt, IntentError> {
        let id = intent.id();
        if let Some(existing) = self.entries.get(&id) {
            if existing != &intent {
                return Err(IntentError::IdempotencyConflict { id });
            }
            return Ok(IntentReceipt {
                id,
                inserted: false,
            });
        }
        self.entries.insert(id, intent);
        Ok(IntentReceipt { id, inserted: true })
    }

    /// Returns the number of distinct accepted intents.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns whether this log contains no accepted intents.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Looks up one accepted intent by idempotency identity.
    #[must_use]
    pub fn get(&self, id: IntentId) -> Option<&Intent> {
        self.entries.get(&id)
    }

    /// Iterates accepted intents in canonical identity order.
    pub fn iter(&self) -> impl Iterator<Item = (&IntentId, &Intent)> {
        self.entries.iter()
    }
}

/// Cross-layer immutable publication/derivation/view delta vocabulary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delta {
    /// Publish one immutable object result from another object.
    Publish {
        /// Source object version.
        source: SemanticObject,
        /// Result object version.
        result: SemanticObject,
    },
    /// Derive one view recipe identity from an immutable object source.
    Derive {
        /// Source object version.
        source: SemanticObject,
        /// Stable view recipe identity.
        view: ViewRecipeId,
    },
    /// Publish one view transition for a stable recipe identity.
    View {
        /// Stable view recipe identity.
        view: ViewRecipeId,
    },
}

impl Delta {
    /// Encodes one fixed-width, domain-separated transition.
    #[must_use]
    pub fn encode(self) -> [u8; 65] {
        let mut bytes = [0; 65];
        match self {
            Self::Publish { source, result } => {
                bytes[0] = 1;
                bytes[1..33].copy_from_slice(source.as_bytes());
                bytes[33..65].copy_from_slice(result.as_bytes());
            }
            Self::Derive { source, view } => {
                bytes[0] = 2;
                bytes[1..33].copy_from_slice(source.as_bytes());
                bytes[33..65].copy_from_slice(view.as_bytes());
            }
            Self::View { view } => {
                bytes[0] = 3;
                bytes[33..65].copy_from_slice(view.as_bytes());
            }
        }
        bytes
    }

    /// Decodes a fixed-width transition when its logical value preimages are
    /// supplied by the producer.
    ///
    /// A fixed-width digest alone cannot prove an object or recipe version.
    /// Callers that only have the bytes emitted by [`Self::encode`] must use
    /// [`Self::decode_with_values`] with the exact canonical values.
    #[must_use]
    pub fn decode(bytes: [u8; 65]) -> Option<Self> {
        Self::decode_with_values(bytes, None, None)
    }

    /// Decodes a transition and verifies each identity against its logical
    /// canonical value. `object_values` supplies source/result preimages for a
    /// publish or the source preimage for a derive; `recipe_value` supplies a
    /// view recipe preimage for a derive/view transition.
    #[must_use]
    pub fn decode_with_values(
        bytes: [u8; 65],
        object_values: Option<(&[u8], Option<&[u8]>)>,
        recipe_value: Option<&[u8]>,
    ) -> Option<Self> {
        let object = |offset: usize| {
            let value = object_values.and_then(|(source, result)| {
                if offset == 1 {
                    Some(source)
                } else if offset == 33 {
                    result
                } else {
                    None
                }
            })?;
            let text = crate::canonical::encode_id(bytes[offset..offset + 32].try_into().ok()?);
            admit_version_value::<ObjectSchema>(&text, value).ok()
        };
        let view = || {
            let value = recipe_value?;
            let text = crate::canonical::encode_id(bytes[33..65].try_into().ok()?);
            admit_key_value::<ViewRecipeSchema>(&text, value).ok()
        };
        match bytes[0] {
            1 => Some(Self::Publish {
                source: object(1)?,
                result: object(33)?,
            }),
            2 => Some(Self::Derive {
                source: object(1)?,
                view: view()?,
            }),
            3 => Some(Self::View { view: view()? }),
            _ => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::canonical::{intent_id, object_version, package_key, view_key};

    #[test]
    fn every_delta_round_trips_through_admission() {
        let values = [
            Delta::Publish {
                source: object_version(b"source"),
                result: object_version(b"result"),
            },
            Delta::Derive {
                source: object_version(b"source"),
                view: view_key(b"view"),
            },
            Delta::View {
                view: view_key(b"view-only"),
            },
        ];
        for value in values {
            let decoded = match value {
                Delta::Publish { .. } => Delta::decode_with_values(
                    value.encode(),
                    Some((b"source", Some(b"result"))),
                    None,
                ),
                Delta::Derive { .. } => Delta::decode_with_values(
                    value.encode(),
                    Some((b"source", None)),
                    Some(b"view"),
                ),
                Delta::View { .. } => {
                    Delta::decode_with_values(value.encode(), None, Some(b"view-only"))
                }
            };
            assert_eq!(decoded, Some(value));
        }
    }

    #[test]
    fn duplicate_intents_are_idempotent() {
        let mut log = IntentLog::new();
        let intent = Intent::RequestPackage {
            id: package_key("package"),
            request: intent_id("request_package", b"package"),
        };
        assert!(log.append(intent.clone()).expect("append").inserted);
        assert!(!log.append(intent).expect("replay").inserted);
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn conflicting_payload_with_same_id_is_rejected_and_original_retained() {
        let mut log = IntentLog::new();
        let request = intent_id("manual", b"one");
        let first = Intent::RequestPackage {
            id: package_key("one"),
            request,
        };
        let second = Intent::RequestPackage {
            id: package_key("two"),
            request,
        };
        assert!(log.append(first.clone()).expect("first").inserted);
        assert_eq!(
            log.append(second),
            Err(IntentError::IdempotencyConflict { id: request })
        );
        assert_eq!(log.get(request), Some(&first));
    }

    #[test]
    fn intent_constructors_bind_operation_and_complete_payload() {
        let actor = crate::actor_key("miles");
        let first = Intent::set_preference(actor, "theme", "dark");
        let same = Intent::set_preference(actor, "theme", "dark");
        let changed_key = Intent::set_preference(actor, "font", "dark");
        let changed_value = Intent::set_preference(actor, "theme", "light");
        assert_eq!(first, same);
        assert_ne!(first.id(), changed_key.id());
        assert_ne!(first.id(), changed_value.id());
        assert_ne!(Intent::request_package(package_key("pkg")).id(), first.id());
    }

    #[test]
    fn wire_admission_rejects_wrong_length() {
        assert!(crate::wire_version::<ObjectSchema>("00").is_err());
    }
}
