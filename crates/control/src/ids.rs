//! Typed control-plane identities built on the shared version kernel.
//!
//! The control plane deliberately has no private hash implementation. Every
//! identity below is a `backend_version::ObjectVersion<T>` with a distinct
//! schema marker. The small [`Identity`] wrapper retains the canonical value
//! beside its version so durable recovery can re-admit a wire claim without
//! ever constructing a typed digest from raw bytes.

use backend_version::{IdContext, ObjectVersion, Schema, UntrustedId};

use crate::ControlError;

/// Maximum bytes accepted for one label or root preimage in a wire adapter.
pub const MAX_ID_PREIMAGE_BYTES: usize = 4096;

/// Canonical owned bytes used as the value of small versioned identity
/// objects. The schema encoder adds one length field, so two distinct
/// identities cannot collide through concatenation ambiguity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdentityBytes(Box<[u8]>);

impl IdentityBytes {
    /// Creates one bounded canonical byte value.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] when the value exceeds the canonical
    /// preimage limit.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, ControlError> {
        let bytes = bytes.into();
        if bytes.len() > MAX_ID_PREIMAGE_BYTES {
            return Err(ControlError::Bounds);
        }
        Ok(Self(bytes.into_boxed_slice()))
    }

    /// Returns the exact canonical bytes represented by this identity value.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

fn encode_identity(value: &IdentityBytes, output: &mut Vec<u8>) {
    output.extend_from_slice(&(value.as_bytes().len() as u64).to_be_bytes());
    output.extend_from_slice(value.as_bytes());
}

macro_rules! identity_schema {
    ($name:ident, $ty:expr) => {
        #[doc = "Schema marker for one domain-separated control identity."]
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct $name;

        impl Schema for $name {
            const DOMAIN: u8 = 0x40;
            const TYPE: u16 = $ty;
            type Value = IdentityBytes;

            fn encode(value: &Self::Value, output: &mut Vec<u8>) {
                encode_identity(value, output);
            }
        }
    };
}

identity_schema!(CellSchema, 0x0001);
identity_schema!(RoleSchema, 0x0002);
identity_schema!(ModelSchema, 0x0003);
identity_schema!(ToolchainSchema, 0x0004);
identity_schema!(InputSchema, 0x0005);
identity_schema!(DependencySchema, 0x0006);
identity_schema!(OwnerSchema, 0x0007);
identity_schema!(OutputSchema, 0x0008);
identity_schema!(EvidenceSchema, 0x0009);
identity_schema!(ContextSchema, 0x000a);
identity_schema!(ReceiptSchema, 0x000b);
identity_schema!(FenceSchema, 0x000c);
identity_schema!(EvaluationSchema, 0x000d);
identity_schema!(WorkKeySchema, 0x000e);
identity_schema!(EvaluationReceiptSchema, 0x0011);
identity_schema!(ReviewReceiptSchema, 0x0012);
identity_schema!(DecisionReceiptSchema, 0x0013);

/// Schema marker for the compact scheduler projection summary retained next
/// to a control relation root.  The summary is a versioned value object, so a
/// process restart can recover capacity counters without walking work rows.
pub struct ControlSummarySchema;

impl Schema for ControlSummarySchema {
    const DOMAIN: u8 = 0x40;
    const TYPE: u16 = 0x0010;
    type Value = ControlSummary;

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.active.to_be_bytes());
        output.extend_from_slice(&value.ready.to_be_bytes());
    }
}

/// O(1) scheduler counters carried by the selected durable workspace root.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ControlSummary {
    active: u64,
    ready: u64,
}

impl ControlSummary {
    /// Constructs a checked summary from bounded counters.
    #[must_use]
    pub const fn new(active: u64, ready: u64) -> Self {
        Self { active, ready }
    }

    /// Returns the number of running or frozen attempts.
    #[must_use]
    pub const fn active(self) -> u64 {
        self.active
    }

    /// Returns the number of ready or queued rows.
    #[must_use]
    pub const fn ready(self) -> u64 {
        self.ready
    }

    /// Decodes the exact canonical summary value bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Corrupt`] when the byte slice is not the
    /// fixed-width canonical summary encoding.
    pub fn decode(bytes: &[u8]) -> Result<Self, ControlError> {
        let bytes: [u8; 16] = bytes.try_into().map_err(|_| ControlError::Corrupt)?;
        let active = u64::from_be_bytes(bytes[..8].try_into().map_err(|_| ControlError::Corrupt)?);
        let ready = u64::from_be_bytes(bytes[8..].try_into().map_err(|_| ControlError::Corrupt)?);
        Ok(Self { active, ready })
    }
}

/// Version of the compact scheduler projection summary.
pub type SummaryVersion = ObjectVersion<ControlSummarySchema>;

/// Schema marker for the coordinator authority object used by workspace
/// closure publication.  The value is a stable authority generation rather
/// than an unverified digest copied from a wire request.
pub struct ControlAuthoritySchema;

impl Schema for ControlAuthoritySchema {
    const DOMAIN: u8 = 0x40;
    const TYPE: u16 = 0x000f;
    type Value = u64;

    fn encode(value: &Self::Value, output: &mut Vec<u8>) {
        output.extend_from_slice(&value.to_be_bytes());
    }
}

/// Complete version of the control-plane authority object.
pub type AuthorityVersion = ObjectVersion<ControlAuthoritySchema>;

/// Identity of a planned invariant cell.
pub type CellId = ObjectVersion<CellSchema>;
/// Identity of a role contract.
pub type RoleId = ObjectVersion<RoleSchema>;
/// Identity of a model family/class.
pub type ModelId = ObjectVersion<ModelSchema>;
/// Identity of an immutable toolchain root.
pub type ToolchainRoot = ObjectVersion<ToolchainSchema>;
/// Identity of the complete source/input root.
pub type InputRoot = ObjectVersion<InputSchema>;
/// Identity of the sorted dependency root.
pub type DependencyRoot = ObjectVersion<DependencySchema>;
/// Identity of an owner process or agent card.
pub type OwnerId = ObjectVersion<OwnerSchema>;
/// Identity of a candidate/output root.
pub type OutputRoot = ObjectVersion<OutputSchema>;
/// Identity of evidence attached to a run.
pub type EvidenceRoot = ObjectVersion<EvidenceSchema>;
/// Identity of a reviewer context root.
pub type ContextRoot = ObjectVersion<ContextSchema>;
/// Identity of an immutable receipt.
pub type ReceiptId = ObjectVersion<ReceiptSchema>;
/// Identity of an evaluator attestation over one exact candidate receipt.
pub type EvaluationReceiptId = ObjectVersion<EvaluationReceiptSchema>;
/// Identity of a reviewer attestation over one exact evaluation receipt.
pub type ReviewReceiptId = ObjectVersion<ReviewReceiptSchema>;
/// Identity of a Sol decision over one exact review receipt.
pub type DecisionReceiptId = ObjectVersion<DecisionReceiptSchema>;
/// Identity of a lease fence.
pub type FenceId = ObjectVersion<FenceSchema>;
/// Identity of an evaluator/holdout environment.
pub type EvaluationRoot = ObjectVersion<EvaluationSchema>;
/// Identity of a complete immutable unit of agent work.
pub type AgentWorkKey = ObjectVersion<WorkKeySchema>;

/// A typed identity together with the exact canonical value that produced it.
///
/// Keeping the preimage is useful only at durable recovery and wire-admission
/// boundaries. Callers normally use [`Self::id`] and never manipulate the
/// representation directly.
pub struct Identity<T: Schema> {
    id: ObjectVersion<T>,
    value: IdentityBytes,
}

impl<T: Schema<Value = IdentityBytes>> Identity<T> {
    /// Derives one typed identity from canonical bytes.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::Bounds`] when the preimage is oversized.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>) -> Result<Self, ControlError> {
        let value = IdentityBytes::new(bytes)?;
        Ok(Self {
            id: ObjectVersion::from_value(&value),
            value,
        })
    }

    /// Derives one typed identity from a stable human label.
    ///
    /// # Errors
    ///
    /// Returns [`ControlError::InvalidLabel`] for an empty or oversized label
    /// and [`ControlError::Bounds`] when its bytes cannot be admitted.
    pub fn from_label(label: &str) -> Result<Self, ControlError> {
        let normalized = label.trim();
        if normalized.is_empty() || normalized.len() > MAX_ID_PREIMAGE_BYTES {
            return Err(ControlError::InvalidLabel);
        }
        Self::from_bytes(normalized.as_bytes().to_vec())
    }

    /// Creates a checked identity whose canonical value is a bounded hex
    /// payload supplied by a transport adapter.
    ///
    /// # Errors
    ///
    /// Returns a wire-admission error for malformed or oversized hexadecimal
    /// input.
    pub fn from_hex(value: &str) -> Result<Self, ControlError> {
        Self::from_bytes(decode_hex(value)?)
    }

    /// Returns the checked object version.
    #[must_use]
    pub const fn id(&self) -> ObjectVersion<T> {
        self.id
    }

    /// Returns the canonical value preimage.
    #[must_use]
    pub fn value(&self) -> &IdentityBytes {
        &self.value
    }

    /// Re-admits a wire claim against the exact canonical value bytes.
    ///
    /// # Errors
    ///
    /// Returns a wire-admission error for a malformed claim and
    /// [`ControlError::Corrupt`] when the claim does not match `value`.
    pub fn from_wire(bytes: &[u8], value: impl Into<Vec<u8>>) -> Result<Self, ControlError> {
        let value = IdentityBytes::new(value)?;
        let claim = UntrustedId::<T>::from_wire(bytes, IdContext::schema::<T>())
            .map_err(|_| ControlError::InvalidDigestLength)?;
        let id = ObjectVersion::admit_value(claim, &value).map_err(|_| ControlError::Corrupt)?;
        Ok(Self { id, value })
    }
}

impl<T: Schema<Value = IdentityBytes>> Clone for Identity<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            value: self.value.clone(),
        }
    }
}

impl<T: Schema<Value = IdentityBytes>> core::fmt::Debug for Identity<T> {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Identity")
            .field("id", &self.id)
            .field("value", &self.value)
            .finish()
    }
}

impl<T: Schema<Value = IdentityBytes>> PartialEq for Identity<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.value == other.value
    }
}

impl<T: Schema<Value = IdentityBytes>> Eq for Identity<T> {}

/// Derives one typed identity from canonical bytes.
pub(crate) fn derive<T: Schema<Value = IdentityBytes>>(
    bytes: &[u8],
) -> Result<ObjectVersion<T>, ControlError> {
    Identity::<T>::from_bytes(bytes.to_vec()).map(|identity| identity.id())
}

/// Adds a schema-tagged typed object version to a canonical preimage.
pub(crate) fn append_version<T: Schema>(output: &mut Vec<u8>, version: ObjectVersion<T>) {
    output.push(T::DOMAIN);
    output.extend_from_slice(&T::TYPE.to_be_bytes());
    output.push(T::VERSION);
    output.extend_from_slice(version.as_bytes());
}

/// Adds one length-delimited field to a canonical transition preimage.
pub(crate) fn append_field(output: &mut Vec<u8>, bytes: &[u8]) -> Result<(), ControlError> {
    let length = u64::try_from(bytes.len()).map_err(|_| ControlError::Bounds)?;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
    Ok(())
}

/// Adds one checked identity, including its schema marker and value length.
pub(crate) fn append_identity<T: Schema<Value = IdentityBytes>>(
    output: &mut Vec<u8>,
    identity: &Identity<T>,
) -> Result<(), ControlError> {
    append_version::<T>(output, identity.id());
    append_field(output, identity.value().as_bytes())
}

/// Adds one already-admitted identity without repeating its bounded checks.
pub(crate) fn append_identity_unchecked<T: Schema<Value = IdentityBytes>>(
    output: &mut Vec<u8>,
    identity: &Identity<T>,
) {
    append_version::<T>(output, identity.id());
    append_field_unchecked(output, identity.value().as_bytes());
}

/// Encodes a bounded field for a canonical value whose admission was already
/// completed by its constructor. Keeping this primitive beside the checked
/// form prevents individual codecs from inventing a second framing grammar.
pub(crate) fn append_field_unchecked(output: &mut Vec<u8>, bytes: &[u8]) {
    debug_assert!(u64::try_from(bytes.len()).is_ok());
    #[allow(clippy::cast_possible_truncation)]
    let length = bytes.len() as u64;
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
}

/// Decodes a bounded hexadecimal wire value into its canonical byte value.
///
/// # Errors
///
/// Returns [`ControlError::InvalidDigestLength`] for odd or oversized input
/// and [`ControlError::InvalidDigest`] for a non-hexadecimal digit.
pub fn decode_hex(value: &str) -> Result<Vec<u8>, ControlError> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    if !value.len().is_multiple_of(2) || value.len() / 2 > MAX_ID_PREIMAGE_BYTES {
        return Err(ControlError::InvalidDigestLength);
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let mut digits = value.as_bytes().chunks_exact(2);
    for pair in &mut digits {
        let high = hex_digit(pair[0]).ok_or(ControlError::InvalidDigest)?;
        let low = hex_digit(pair[1]).ok_or(ControlError::InvalidDigest)?;
        bytes.push((high << 4) | low);
    }
    if !digits.remainder().is_empty() {
        return Err(ControlError::InvalidDigestLength);
    }
    Ok(bytes)
}

/// Encodes a fixed-width identity for a wire response.
#[must_use]
pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
