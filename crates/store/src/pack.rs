use super::{
    BTreeMap, Hash, OrderedMap, StoreError, StoredValue, checked_key_len, checked_wire_len, digest,
    put_u32, record_wire_len,
};

/// Physical materialization identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LayoutId(Hash);

impl LayoutId {
    /// Derives a physical layout identity from its canonical descriptor.
    #[must_use]
    pub fn derive(descriptor: &[u8]) -> Self {
        Self(digest(b"layout", descriptor))
    }

    /// Creates a layout identity from a checked fixed-width representation.
    pub(crate) const fn from_bytes(bytes: Hash) -> Self {
        Self(bytes)
    }

    /// Returns the fixed-width layout identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &Hash {
        &self.0
    }
}

/// Physical immutable pack identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PackId(Hash);

impl PackId {
    fn from_digest(bytes: Hash) -> Self {
        Self(bytes)
    }

    pub(crate) fn from_wire(bytes: Hash) -> Self {
        Self::from_digest(bytes)
    }

    /// Returns the fixed-width pack identity bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &Hash {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
/// Validated immutable pack and its key location directory.
pub struct Pack {
    id: PackId,
    layout: LayoutId,
    bytes: Vec<u8>,
    locations: BTreeMap<Vec<u8>, (u32, u32)>,
}

impl Pack {
    /// Returns the content identity derived from the validated bytes.
    #[must_use]
    pub const fn id(&self) -> PackId {
        self.id
    }

    /// Returns the physical layout identity.
    #[must_use]
    pub const fn layout(&self) -> LayoutId {
        self.layout
    }

    /// Borrows the validated encoded records.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Borrows the validated key location directory.
    #[must_use]
    pub fn locations(&self) -> &BTreeMap<Vec<u8>, (u32, u32)> {
        &self.locations
    }

    /// Exposes a copy as an untrusted wire value for transport or testing.
    #[must_use]
    pub fn into_wire(self) -> WirePack {
        WirePack {
            id: *self.id.as_bytes(),
            layout: self.layout,
            bytes: self.bytes,
            locations: self.locations,
        }
    }

    /// Copies this trusted pack into an untrusted wire value.
    #[must_use]
    pub fn to_wire(&self) -> WirePack {
        self.clone().into_wire()
    }
}

/// Untrusted serialized pack received from a file, peer, or caller.
///
/// Its public fields are deliberately raw. Call [`admit_pack`] before a
/// value becomes a trusted [`Pack`] or enters [`crate::Residency`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WirePack {
    /// Claimed content identity; admission derives the trusted [`PackId`].
    pub id: Hash,
    /// Claimed physical layout identity.
    pub layout: LayoutId,
    /// Encoded key/value records.
    pub bytes: Vec<u8>,
    /// Claimed key to byte offset and length directory.
    pub locations: BTreeMap<Vec<u8>, (u32, u32)>,
}

impl WirePack {
    /// Validates and admits this untrusted wire value.
    ///
    /// # Errors
    ///
    /// Returns an error when the digest, record lengths, location directory,
    /// or byte budget is invalid.
    pub fn admit(self, max_bytes: usize) -> Result<Pack, StoreError> {
        admit_pack(self, max_bytes)
    }
}
/// Encodes all map entries into one bounded immutable pack.
///
/// # Errors
///
/// Returns [`StoreError::Bounds`] when a record or the complete pack cannot
/// be represented by the bounded wire format.
pub fn encode_pack(
    map: &OrderedMap,
    layout: LayoutId,
    max_bytes: usize,
) -> Result<Pack, StoreError> {
    let mut bytes = Vec::new();
    let mut locations = BTreeMap::new();
    for (k, v) in map {
        let start = bytes.len();
        let key_len = checked_key_len(k.len())?;
        let record_len = record_wire_len(k, v)?;
        let next = start.checked_add(record_len).ok_or(StoreError::Bounds)?;
        checked_wire_len(next)?;
        if next > max_bytes {
            return Err(StoreError::Bounds);
        }
        put_u32(&mut bytes, key_len);
        bytes.extend_from_slice(k);
        v.encode(&mut bytes)?;
        debug_assert_eq!(bytes.len(), next);
        let offset = u32::try_from(start).map_err(|_| StoreError::Bounds)?;
        let length = u32::try_from(record_len).map_err(|_| StoreError::Bounds)?;
        locations.insert(k.clone(), (offset, length));
    }
    let id = PackId::from_digest(digest(b"pack", &bytes));
    Ok(Pack {
        id,
        layout,
        bytes,
        locations,
    })
}
/// Validates and admits an untrusted bounded pack.
///
/// # Errors
///
/// Returns [`StoreError::Corrupt`] or [`StoreError::Bounds`] when the pack
/// digest, records, locations, or duplicate-key constraints are invalid.
pub fn admit_pack(wire: WirePack, max_bytes: usize) -> Result<Pack, StoreError> {
    if wire.bytes.len() > max_bytes {
        return Err(StoreError::Bounds);
    }
    if digest(b"pack", &wire.bytes) != wire.id {
        return Err(StoreError::Corrupt);
    }
    validate_pack_records(&wire.bytes, &wire.locations)?;
    // Admission proves that the records reconstruct one canonical logical
    // map.  A syntactically valid pack with an oversized key or node must not
    // become trusted merely because its byte directory is well formed.
    let _ = decode_pack_values(&wire.bytes)?;
    Ok(Pack {
        id: PackId::from_digest(wire.id),
        layout: wire.layout,
        bytes: wire.bytes,
        locations: wire.locations,
    })
}

/// Reopens a trusted pack, checking its content hash and every length field.
///
/// # Errors
///
/// Returns [`StoreError::Corrupt`] or [`StoreError::Bounds`] when the pack
/// digest, records, locations, or duplicate-key constraints are invalid.
pub fn decode_pack(pack: &Pack) -> Result<OrderedMap, StoreError> {
    if PackId::from_digest(digest(b"pack", &pack.bytes)) != pack.id {
        return Err(StoreError::Corrupt);
    }
    validate_pack_records(&pack.bytes, &pack.locations)?;
    decode_pack_values(&pack.bytes)
}

/// Admits and decodes an untrusted wire pack in one checked operation.
///
/// # Errors
///
/// Returns an error when admission or canonical map reconstruction fails.
pub fn decode_wire_pack(wire: WirePack, max_bytes: usize) -> Result<OrderedMap, StoreError> {
    let pack = admit_pack(wire, max_bytes)?;
    decode_pack(&pack)
}

fn validate_pack_records(
    bytes: &[u8],
    locations: &BTreeMap<Vec<u8>, (u32, u32)>,
) -> Result<(), StoreError> {
    let mut at = 0usize;
    let mut seen = BTreeMap::new();
    let mut last_key: Option<Vec<u8>> = None;
    while at < bytes.len() {
        let start = at;
        let key_len = read_u32(bytes, &mut at)? as usize;
        let end = at.checked_add(key_len).ok_or(StoreError::Bounds)?;
        let key = bytes.get(at..end).ok_or(StoreError::Bounds)?.to_vec();
        at = end;
        if last_key.as_ref().is_some_and(|last| *last >= key) {
            return Err(StoreError::Corrupt);
        }
        last_key = Some(key.clone());
        bytes.get(at).ok_or(StoreError::Bounds)?;
        at = at.checked_add(1).ok_or(StoreError::Bounds)?;
        let value_len = read_u32(bytes, &mut at)? as usize;
        let end = at.checked_add(value_len).ok_or(StoreError::Bounds)?;
        bytes.get(at..end).ok_or(StoreError::Bounds)?;
        at = end;
        let refs = read_u32(bytes, &mut at)? as usize;
        let byte_len = refs.checked_mul(32).ok_or(StoreError::Bounds)?;
        let end = at.checked_add(byte_len).ok_or(StoreError::Bounds)?;
        bytes.get(at..end).ok_or(StoreError::Bounds)?;
        at = end;
        let length = at.checked_sub(start).ok_or(StoreError::Bounds)?;
        let offset = u32::try_from(start).map_err(|_| StoreError::Bounds)?;
        let length = u32::try_from(length).map_err(|_| StoreError::Bounds)?;
        if locations.get(&key) != Some(&(offset, length)) {
            return Err(StoreError::Corrupt);
        }
        if seen.insert(key, (offset, length)).is_some() {
            return Err(StoreError::Corrupt);
        }
    }
    if seen != *locations {
        return Err(StoreError::Corrupt);
    }
    Ok(())
}

fn decode_pack_values(bytes: &[u8]) -> Result<OrderedMap, StoreError> {
    let mut at = 0usize;
    let mut values = BTreeMap::new();
    while at < bytes.len() {
        let key_len = read_u32(bytes, &mut at)? as usize;
        let end = at.checked_add(key_len).ok_or(StoreError::Bounds)?;
        let key = bytes.get(at..end).ok_or(StoreError::Bounds)?.to_vec();
        at = end;
        let availability = *bytes.get(at).ok_or(StoreError::Bounds)?;
        at = at.checked_add(1).ok_or(StoreError::Bounds)?;
        let value_len = read_u32(bytes, &mut at)? as usize;
        let end = at.checked_add(value_len).ok_or(StoreError::Bounds)?;
        let value = bytes.get(at..end).ok_or(StoreError::Bounds)?.to_vec();
        at = end;
        let refs = read_u32(bytes, &mut at)? as usize;
        let byte_len = refs.checked_mul(32).ok_or(StoreError::Bounds)?;
        let end = at.checked_add(byte_len).ok_or(StoreError::Bounds)?;
        let raw = bytes.get(at..end).ok_or(StoreError::Bounds)?;
        at = end;
        let references = raw
            .chunks_exact(32)
            .map(|chunk| {
                let mut reference = [0; 32];
                reference.copy_from_slice(chunk);
                reference
            })
            .collect();
        if values
            .insert(
                key,
                StoredValue {
                    value,
                    availability,
                    references,
                },
            )
            .is_some()
        {
            return Err(StoreError::Corrupt);
        }
    }
    OrderedMap::try_from_iter(values)
}
fn read_u32(bytes: &[u8], at: &mut usize) -> Result<u32, StoreError> {
    let end = at.checked_add(4).ok_or(StoreError::Bounds)?;
    let raw = bytes.get(*at..end).ok_or(StoreError::Bounds)?;
    *at = end;
    Ok(u32::from_le_bytes(
        raw.try_into().map_err(|_| StoreError::Corrupt)?,
    ))
}
