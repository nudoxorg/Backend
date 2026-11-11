//! Bounded C# extension codec for the reserved v16 owner intent. This module
//! does not authorize a transition or replace the existing owner admission.
use super::csharp_admission::{CSharpReceiptBasis, CSharpReceiptChange};
use backend_engine::builtin::{MAX_CSHARP_RECEIPT_ROW_BYTES, CSharpReceiptRecord, CSharpReceiptRelation};
use backend_version::{CanonicalRelation, Relation};

/// Reserved combined owner intent version; integration must preserve v3..v14.
pub(super) const CSHARP_INTENT_VERSION: u8 = 16;
/// Reserved combined owner intent magic (v13 NSE uses BPID).
pub(super) const CSHARP_INTENT_MAGIC: &[u8; 4] = b"BPIG";
const EXTENSION_MAGIC: &[u8; 5] = b"CSUX\x01";
const MAX_CHANGES: usize = 2 * (8192 + 1);
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Unauthenticated serialized before/after claims for exact owner recomputation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct CSharpReceiptIntentExtension {
    pub(super) basis: CSharpReceiptBasis,
    pub(super) changes: Vec<CSharpReceiptChange>,
}
impl CSharpReceiptIntentExtension {
    fn validate(&self) -> Result<(), String> {
        if self.basis.workspace_root == [0; 32]
            || self.basis.closure == [0; 32]
            || self.basis.sequence == u64::MAX
            || self.basis.relation_root == Some([0; 32])
            || self.changes.is_empty()
            || self.changes.len() > MAX_CHANGES
            || self
                .changes
                .windows(2)
                .any(|pair| pair[0].key >= pair[1].key)
            || self
                .changes
                .iter()
                .any(|change| change.expected == change.after)
        {
            return Err("invalid C# intent change ordering or extent".into());
        }
        let mut owner = None;
        for change in &self.changes {
            for row in [change.expected.as_ref(), change.after.as_ref()]
                .into_iter()
                .flatten()
            {
                row.validate_shape().map_err(str::to_owned)?;
                backend_engine::builtin::admit_csharp_receipt_row(&change.key, row).map_err(str::to_owned)?;
                let (project, unit) = match row {
                    CSharpReceiptRecord::Unit { project, unit, .. }
                    | CSharpReceiptRecord::Member { project, unit, .. } => (project, unit),
                };
                if owner.is_some_and(|prior| prior != (project, unit)) {
                    return Err("C# intent spans multiple units".into());
                }
                owner = Some((project, unit));
            }
        }
        Ok(())
    }
    /// Canonical bounded extension; no live proof is encoded.
    pub(super) fn encode(&self) -> Result<Vec<u8>, String> {
        self.validate()?;
        let mut out = Vec::new();
        append(&mut out, EXTENSION_MAGIC)?;
        append(&mut out, &self.basis.workspace_root)?;
        append(&mut out, &self.basis.sequence.to_be_bytes())?;
        append(&mut out, &self.basis.owner_epoch.to_be_bytes())?;
        append(&mut out, &self.basis.closure)?;
        match self.basis.relation_root {
            Some(root) => {
                append(&mut out, &[1])?;
                append(&mut out, &root)?;
            }
            None => append(&mut out, &[0])?,
        }
        append(&mut out, &(self.changes.len() as u32).to_be_bytes())?;
        for change in &self.changes {
            append(&mut out, &change.key)?;
            for row in [&change.expected, &change.after] {
                if let Some(row) = row {
                    let mut encoded = Vec::new();
                    CSharpReceiptRelation::encode_value(row, &mut encoded);
                    if encoded.len() > MAX_CSHARP_RECEIPT_ROW_BYTES {
                        return Err("C# intent row limit".into());
                    }
                    append(&mut out, &(encoded.len() as u32).to_be_bytes())?;
                    append(&mut out, &encoded)?;
                } else {
                    append(&mut out, &0u32.to_be_bytes())?;
                }
            }
        }
        Ok(out)
    }
    /// Strictly decode claims; this grants no publication or source authority.
    pub(super) fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_BYTES {
            return Err("C# intent byte limit".into());
        }
        let mut input = Input { bytes, position: 0 };
        if input.take(5)? != EXTENSION_MAGIC {
            return Err("C# intent magic".into());
        }
        let workspace_root = input.array()?;
        let sequence = u64::from_be_bytes(input.array()?);
        let owner_epoch = u64::from_be_bytes(input.array()?);
        let closure = input.array()?;
        let relation_root = match input.take(1)?[0] {
            0 => None,
            1 => Some(input.array()?),
            _ => return Err("C# intent root tag".into()),
        };
        let count = u32::from_be_bytes(input.array()?) as usize;
        if count == 0
            || count > MAX_CHANGES
            || count > input.bytes.len().saturating_sub(input.position) / 40
        {
            return Err("C# intent change count".into());
        }
        let mut changes = Vec::new();
        changes
            .try_reserve_exact(count)
            .map_err(|_| "C# intent allocation")?;
        for _ in 0..count {
            let key = input.array()?;
            let mut row = || -> Result<Option<CSharpReceiptRecord>, String> {
                let length = u32::from_be_bytes(input.array()?) as usize;
                if length == 0 {
                    return Ok(None);
                }
                if length > MAX_CSHARP_RECEIPT_ROW_BYTES {
                    return Err("C# intent row limit".into());
                }
                CSharpReceiptRelation::decode_value(input.take(length)?)
                    .map(Some)
                    .map_err(|_| "invalid C# intent row".into())
            };
            let expected = row()?;
            let after = row()?;
            changes.push(CSharpReceiptChange {
                key,
                expected,
                after,
            });
        }
        if input.position != bytes.len() {
            return Err("trailing C# intent bytes".into());
        }
        let value = Self {
            basis: CSharpReceiptBasis {
                workspace_root,
                sequence,
                owner_epoch,
                closure,
                relation_root,
            },
            changes,
        };
        value.validate()?;
        Ok(value)
    }
}
fn append(out: &mut Vec<u8>, bytes: &[u8]) -> Result<(), String> {
    let size = out
        .len()
        .checked_add(bytes.len())
        .ok_or("C# intent overflow")?;
    if size > MAX_BYTES {
        return Err("C# intent byte limit".into());
    }
    out.try_reserve(bytes.len())
        .map_err(|_| "C# intent allocation")?;
    out.extend_from_slice(bytes);
    Ok(())
}
struct Input<'a> {
    bytes: &'a [u8],
    position: usize,
}
impl<'a> Input<'a> {
    fn take(&mut self, length: usize) -> Result<&'a [u8], String> {
        let end = self
            .position
            .checked_add(length)
            .ok_or("C# intent offset")?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or("truncated C# intent")?;
        self.position = end;
        Ok(value)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)?
            .try_into()
            .map_err(|_| "invalid C# intent field".into())
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use backend_engine::builtin::{CSharpMemberOutcome, CSharpReceiptUnit, csharp_receipt_key};

    pub(super) fn example(project: [u8; 32]) -> CSharpReceiptIntentExtension {
        let row = CSharpReceiptRecord::Member {
            project, unit: CSharpReceiptUnit::Project { path: "App.csproj".into() },
            path: "Program.cs".into(), source_identity: [1; 32], input_content: [2; 32],
            content_digest: [3; 32], byte_length: 7, outcome: CSharpMemberOutcome::Unknown,
        };
        CSharpReceiptIntentExtension {
            basis: CSharpReceiptBasis { workspace_root: [4; 32], sequence: 5,
                owner_epoch: 6, closure: [7; 32], relation_root: None },
            changes: vec![CSharpReceiptChange { key: csharp_receipt_key(&row).unwrap(), expected: None, after: Some(row) }],
        }
    }

    #[test]
    fn exact_claims_roundtrip_and_reject_forged_framing() {
        let extension = example([8; 32]);
        let bytes = extension.encode().unwrap();
        assert_eq!(CSharpReceiptIntentExtension::decode(&bytes).unwrap(), extension);
        assert!(CSharpReceiptIntentExtension::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut trailing = bytes.clone(); trailing.push(0);
        assert!(CSharpReceiptIntentExtension::decode(&trailing).is_err());
        let mut bad_key = extension.clone(); bad_key.changes[0].key = [0; 32];
        assert!(bad_key.encode().is_err());
        let mut mixed = extension.clone();
        mixed.changes.extend(example([9; 32]).changes);
        mixed.changes.sort_by_key(|change| change.key);
        assert!(mixed.encode().is_err());
    }
}
