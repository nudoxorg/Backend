//! Bounded SQL extension codec for the reserved v14 owner intent. This module
//! does not authorize a transition or replace the existing owner admission.
use super::sql_admission::{SqlAnalysisBasis, SqlAnalysisChange};
use backend_engine::builtin::{MAX_SQL_ROW_BYTES, SqlAnalysisRecord, SqlAnalysisRelation};
use backend_version::{CanonicalRelation, Relation};

/// Reserved combined owner intent version; integration must preserve v3..v13.
pub(super) const SQL_INTENT_VERSION: u8 = 14;
/// Reserved combined owner intent magic (v13 NSE uses BPID).
pub(super) const SQL_INTENT_MAGIC: &[u8; 4] = b"BPIE";
const EXTENSION_MAGIC: &[u8; 5] = b"SQLX\x01";
const MAX_CHANGES: usize = 262_144;
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Unauthenticated serialized before/after claims for exact owner recomputation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct SqlIntentExtension {
    pub(super) basis: SqlAnalysisBasis,
    pub(super) changes: Vec<SqlAnalysisChange>,
}
impl SqlIntentExtension {
    fn validate(&self) -> Result<(), String> {
        if self.basis.workspace_root == [0; 32]
            || self.basis.closure == [0; 32]
            || self.basis.sequence == u64::MAX
            || self.basis.analysis_root == Some([0; 32])
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
            return Err("invalid SQL intent change ordering or extent".into());
        }
        for change in &self.changes {
            for row in [change.expected.as_ref(), change.after.as_ref()]
                .into_iter()
                .flatten()
            {
                row.validate()?;
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
        append(&mut out, &self.basis.closure)?;
        match self.basis.analysis_root {
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
                    SqlAnalysisRelation::encode_value(row, &mut encoded);
                    if encoded.len() > MAX_SQL_ROW_BYTES {
                        return Err("SQL intent row limit".into());
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
            return Err("SQL intent byte limit".into());
        }
        let mut input = Input { bytes, position: 0 };
        if input.take(5)? != EXTENSION_MAGIC {
            return Err("SQL intent magic".into());
        }
        let workspace_root = input.array()?;
        let sequence = u64::from_be_bytes(input.array()?);
        let closure = input.array()?;
        let analysis_root = match input.take(1)?[0] {
            0 => None,
            1 => Some(input.array()?),
            _ => return Err("SQL intent root tag".into()),
        };
        let count = u32::from_be_bytes(input.array()?) as usize;
        if count == 0
            || count > MAX_CHANGES
            || count > input.bytes.len().saturating_sub(input.position) / 40
        {
            return Err("SQL intent change count".into());
        }
        let mut changes = Vec::new();
        changes
            .try_reserve_exact(count)
            .map_err(|_| "SQL intent allocation")?;
        for _ in 0..count {
            let key = input.array()?;
            let mut row = || -> Result<Option<SqlAnalysisRecord>, String> {
                let length = u32::from_be_bytes(input.array()?) as usize;
                if length == 0 {
                    return Ok(None);
                }
                if length > MAX_SQL_ROW_BYTES {
                    return Err("SQL intent row limit".into());
                }
                SqlAnalysisRelation::decode_value(input.take(length)?)
                    .map(Some)
                    .map_err(|_| "invalid SQL intent row".into())
            };
            let expected = row()?;
            let after = row()?;
            changes.push(SqlAnalysisChange {
                key,
                expected,
                after,
            });
        }
        if input.position != bytes.len() {
            return Err("trailing SQL intent bytes".into());
        }
        let value = Self {
            basis: SqlAnalysisBasis {
                workspace_root,
                sequence,
                closure,
                analysis_root,
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
        .ok_or("SQL intent overflow")?;
    if size > MAX_BYTES {
        return Err("SQL intent byte limit".into());
    }
    out.try_reserve(bytes.len())
        .map_err(|_| "SQL intent allocation")?;
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
            .ok_or("SQL intent offset")?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or("truncated SQL intent")?;
        self.position = end;
        Ok(value)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N], String> {
        self.take(N)?
            .try_into()
            .map_err(|_| "invalid SQL intent field".into())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_extension_roundtrip_rejects_truncation_and_trailing_data() {
        use backend_engine::builtin::{SqlAttemptOutcome, SqlUnavailable, sql_attempt_key};
        let project = [1; 32];
        let row = SqlAnalysisRecord::Attempt {
            project,
            outcome: SqlAttemptOutcome::Unavailable(SqlUnavailable::EngineNotAdmitted),
            served_selection: None,
        };
        let extension = SqlIntentExtension {
            basis: SqlAnalysisBasis {
                workspace_root: [2; 32],
                sequence: 3,
                closure: [4; 32],
                analysis_root: None,
            },
            changes: vec![SqlAnalysisChange {
                key: sql_attempt_key(project),
                expected: None,
                after: Some(row),
            }],
        };
        let bytes = extension.encode().unwrap();
        assert_eq!(SqlIntentExtension::decode(&bytes).unwrap(), extension);
        let mut invalid_basis = extension.clone();
        invalid_basis.basis.closure = [0; 32];
        assert!(invalid_basis.encode().is_err());
        let mut forged = bytes.clone();
        // Extension magic (5), workspace root (32), and sequence (8).
        forged[45..77].fill(0);
        assert!(SqlIntentExtension::decode(&forged).is_err());
        assert!(SqlIntentExtension::decode(&bytes[..bytes.len() - 1]).is_err());
        let mut trailing = bytes;
        trailing.push(0);
        assert!(SqlIntentExtension::decode(&trailing).is_err());
    }
}
