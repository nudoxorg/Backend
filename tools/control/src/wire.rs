//! Bounded JSON/Nu transport adapters.
//!
//! These types deliberately contain strings and byte vectors only at the
//! boundary. Parsing a request immediately constructs a checked [`WorkSpec`];
//! callers inside the control plane never receive an untyped digest map.

use backend_version::{IdContext, ObjectVersion, UntrustedId};
use serde::{Deserialize, Serialize};

use crate::ControlError;
use crate::ids::{
    AgentWorkKey, Identity, IdentityBytes, InputSchema, WorkKeySchema, decode_hex, hex_encode,
};
use crate::spec::{Effort, WorkDependency, WorkSpec};

/// Wire request accepted by `.config` and the command adapter.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct WireWorkSpec {
    /// Stable cell label.
    pub cell: String,
    /// Stable role label.
    pub role: String,
    /// Stable model family/class label.
    #[serde(alias = "model")]
    pub model_class: String,
    /// Effort budget spelling.
    pub effort: String,
    /// Hex or stable-label toolchain root.
    pub toolchain_root: String,
    /// Hex or stable-label source/input digest.
    pub input_digest: String,
    /// Canonical dependency key materials, encoded as hex.
    #[serde(default)]
    pub dependencies: Vec<String>,
    /// Optional claimed work key. It is checked against the generated key and
    /// never admitted solely from its context metadata.
    #[serde(default)]
    pub work_key: Option<String>,
}

impl WireWorkSpec {
    /// Admits a wire request into the typed immutable specification.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Wire`] for malformed JSON values and a typed
    /// identity or canonical-size error for invalid fields.
    pub fn admit(self) -> Result<WorkSpec, ControlError> {
        let effort = Effort::parse(&self.effort)?;
        let toolchain = decode_root(&self.toolchain_root)?;
        let input = decode_root(&self.input_digest)?;
        let dependencies = self
            .dependencies
            .into_iter()
            .map(|material| WorkDependency::from_material(decode_hex(&material)?))
            .collect::<Result<Vec<_>, ControlError>>()?;
        let spec = WorkSpec::from_labels(
            &self.cell,
            &self.role,
            &self.model_class,
            effort,
            &toolchain,
            &input,
            dependencies,
        )?;
        if let Some(claim) = self.work_key {
            admit_work_key(&claim, &spec)?;
        }
        Ok(spec)
    }

    /// Decodes one bounded JSON request and admits it immediately.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Wire`] for malformed JSON or any rejected
    /// field in the request.
    pub fn from_json(json: &str) -> Result<WorkSpec, ControlError> {
        let request = serde_json::from_str::<Self>(json)
            .map_err(|error| ControlError::Wire(error.to_string()))?;
        request.admit()
    }
}

fn decode_root(value: &str) -> Result<Vec<u8>, ControlError> {
    match decode_hex(value) {
        Ok(bytes) => Ok(bytes),
        Err(ControlError::InvalidDigest | ControlError::InvalidDigestLength) => {
            let identity = Identity::<InputSchema>::from_label(value)?;
            Ok(identity.value().as_bytes().to_vec())
        }
        Err(error) => Err(error),
    }
}

/// Stable response view for shell callers that need to persist a key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WireWorkKey {
    /// Schema-qualified hexadecimal key bytes.
    pub work_key: String,
}

impl WireWorkKey {
    /// Converts a checked typed key to its wire representation.
    #[must_use]
    pub fn from_key(key: AgentWorkKey) -> Self {
        Self {
            work_key: hex_encode(key.as_bytes()),
        }
    }

    /// Admits a hexadecimal key claim against an existing checked row.
    ///
    /// # Errors
    ///
    /// Returns a wire, unknown-work, or corruption error when the claim is
    /// not a key in the checked relation.
    pub fn admit(
        value: &str,
        plane: &crate::VersionedControlPlane,
    ) -> Result<AgentWorkKey, ControlError> {
        plane.lookup_work_key(&decode_hex(value)?)
    }
}

fn admit_work_key(claim_hex: &str, spec: &WorkSpec) -> Result<(), ControlError> {
    let bytes = decode_hex(claim_hex)?;
    let claim =
        UntrustedId::<WorkKeySchema>::from_wire(&bytes, IdContext::schema::<WorkKeySchema>())
            .map_err(|_| ControlError::InvalidDigestLength)?;
    let material = IdentityBytes::new(spec.key_material()?)?;
    let admitted = ObjectVersion::<WorkKeySchema>::admit_value(claim, &material)
        .map_err(|_| ControlError::Corrupt)?;
    if admitted != spec.key() {
        return Err(ControlError::Corrupt);
    }
    Ok(())
}
