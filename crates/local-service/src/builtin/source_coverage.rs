//! Bounded residence of immutable authenticated coverage catalogs.
//!
//! Admission/restart can decode once; page reads borrow an indexed row. Keys
//! include the entire selected physical/semantic authority tuple. Eviction
//! loses reuse only and can never authorize a different source frontier.
use super::BuiltinModelError;
use backend_extension_turso::SelectedGeneration;
use backend_semantic::ir::SourceCoverageObservation;
use std::sync::{Arc, Mutex};

const MAX_ENTRIES: usize = 16;
const MAX_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(super) struct CoverageSelection {
    namespace: [u8; 16],
    closure: [u8; 32],
    candidate: [u8; 32],
    target: [u8; 32],
    generation: u64,
    binding: [u8; 32],
    input: [u8; 32],
}
impl CoverageSelection {
    pub(super) fn new(selected: &SelectedGeneration, binding: [u8; 32]) -> Self {
        Self {
            namespace: selected.namespace().namespace_id(),
            closure: *selected.closure_id(),
            candidate: *selected.candidate_id(),
            target: *selected.target_root(),
            generation: selected.generation(),
            binding,
            input: *selected.input_digest(),
        }
    }
}
struct Resident {
    value: Option<Arc<SourceCoverageObservation>>,
    bytes: usize,
}
struct State {
    entries: hashlink::LruCache<CoverageSelection, Resident>,
    bytes: usize,
}
impl Default for State {
    fn default() -> Self {
        Self {
            entries: hashlink::LruCache::new(MAX_ENTRIES),
            bytes: 1024,
        }
    }
}

/// One bounded per-authority residence, shared with its detached immutable read snapshot.
#[derive(Default)]
pub(super) struct CoverageResidence {
    state: Mutex<State>,
}
impl CoverageResidence {
    pub(super) fn get_or_load(
        &self,
        key: CoverageSelection,
        load: impl FnOnce() -> Result<Option<SourceCoverageObservation>, BuiltinModelError>,
    ) -> Result<Option<Arc<SourceCoverageObservation>>, BuiltinModelError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| BuiltinModelError("compiler coverage residence poisoned".to_owned()))?;
        if let Some(resident) = state.entries.get(&key) {
            return Ok(resident.value.clone());
        }
        // Serialize cold admissions. No reader can observe an unverified value or
        // pay the full catalog decode/hash a second time while this key is resident.
        let value = load()?.map(Arc::new);
        // Include bounded LRU node/key/Arc/hash-table slack, not just payload paths.
        let bytes = value.as_ref().map_or(0, |value| value.retained_bytes())
            + std::mem::size_of::<CoverageSelection>()
            + std::mem::size_of::<Resident>()
            + 1024;
        if bytes.saturating_add(1024) > MAX_BYTES {
            return Err(BuiltinModelError(
                "compiler coverage exceeds immutable residence admission".to_owned(),
            ));
        }
        while state.entries.len() >= MAX_ENTRIES || state.bytes.saturating_add(bytes) > MAX_BYTES {
            let Some((_, old)) = state.entries.remove_lru() else {
                break;
            };
            state.bytes -= old.bytes;
        }
        state.bytes += bytes;
        state.entries.insert(
            key,
            Resident {
                value: value.clone(),
                bytes,
            },
        );
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::ir::{SourceCoverageGapCause, SourceCoverageManifest, SourceCoverageRow};
    use std::cell::Cell;
    fn key(profile: u8) -> CoverageSelection {
        CoverageSelection {
            namespace: [profile; 16],
            closure: [3; 32],
            candidate: [4; 32],
            target: [5; 32],
            generation: 1,
            binding: [6; 32],
            input: [7; 32],
        }
    }
    #[test]
    fn indexed_coverage_pages_admit_each_profile_once_and_do_not_revisit_prior_rows()
    -> Result<(), String> {
        use crate::test_support::TestValue as _;
        let residence = CoverageResidence::default();
        let loads = Cell::new(0);
        let source_checks = Cell::new(0);
        let catalog = || {
            loads.set(loads.get() + 1);
            let source =
                *backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                    b"spec",
                )
                .as_ref();
            SourceCoverageManifest::new(
                [7; 32],
                [1, 0],
                [7; 32],
                4097,
                1,
                (0..4096)
                    .map(|i| SourceCoverageRow::Gap {
                        path: format!("src/{i:05}.spec.ts"),
                        source,
                        byte_len: 4,
                        cause: SourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram,
                        diagnostic: None,
                    })
                    .collect(),
            )
            .and_then(SourceCoverageObservation::from_catalog)
            .map(Some)
            .map_err(|e| BuiltinModelError(e.into()))
        };
        // Discovery may load separate published profiles. The exact selector
        // subsequently touches only its selected immutable catalog and row.
        residence.get_or_load(key(1), catalog).test_value()?;
        residence.get_or_load(key(2), catalog).test_value()?;
        for offset in [0, 2048, 4095, 2048] {
            let value = residence
                .get_or_load(key(1), || {
                    panic!("warm page must not decode/hash any catalog again")
                })
                .test_value()?
                .test_value()?;
            let row = &value.catalog().test_value()?.rows()[offset];
            source_checks.set(source_checks.get() + 1);
            assert_eq!(row.path(), format!("src/{offset:05}.spec.ts"));
        }
        assert_eq!(loads.get(), 2, "one admission per exact profile tuple");
        assert_eq!(
            source_checks.get(),
            4,
            "one source check per requested row, independent of offset"
        );
        let mut changed = key(1);
        changed.closure[0] ^= 1;
        residence.get_or_load(changed, catalog).test_value()?;
        assert_eq!(loads.get(), 3, "changed authority cannot reuse old catalog");
        let state = residence.state.lock().test_value()?;
        assert!(state.bytes <= MAX_BYTES && state.entries.len() <= MAX_ENTRIES);
        Ok(())
    }
    #[test]
    fn immutable_coverage_residence_bounds_large_catalog_bytes_and_never_aliases_selected_authority()
    -> Result<(), String> {
        use crate::test_support::TestValue as _;
        let residence = CoverageResidence::default();
        let loads = Cell::new(0);
        let make = || {
            loads.set(loads.get() + 1);
            let source =
                *backend_version::ContentId::<backend_version::SourceFactDomain>::from_canonical_bytes(
                    b"spec",
                )
                .as_ref();
            SourceCoverageManifest::new(
                [7; 32],
                [1, 0],
                [7; 32],
                15361,
                1,
                (0..15360)
                    .map(|i| SourceCoverageRow::Gap {
                        path: format!("src/{i:05}{}.spec.ts", "x".repeat(400)),
                        source,
                        byte_len: 4,
                        cause: SourceCoverageGapCause::TypeScriptSourceExcludedFromConfiguredProgram,
                        diagnostic: None,
                    })
                    .collect(),
            )
            .and_then(SourceCoverageObservation::from_catalog)
            .map(Some)
            .map_err(|e| BuiltinModelError(e.into()))
        };
        for namespace in 1..9 {
            residence.get_or_load(key(namespace), make).test_value()?;
        }
        {
            let state = residence.state.lock().test_value()?;
            assert!(state.bytes <= MAX_BYTES);
            assert!(
                state.entries.len() < 8,
                "byte admission evicted large catalogs before the entry cap"
            );
        }
        let before = loads.get();
        residence.get_or_load(key(1), make).test_value()?;
        assert_eq!(loads.get(), before + 1, "an evicted key is reauthenticated");
        for field in 0..7 {
            let mut changed = key(1);
            match field {
                0 => changed.namespace[0] ^= 1,
                1 => changed.closure[0] ^= 1,
                2 => changed.candidate[0] ^= 1,
                3 => changed.target[0] ^= 1,
                4 => changed.generation += 1,
                5 => changed.binding[0] ^= 1,
                _ => changed.input[0] ^= 1,
            };
            let before = loads.get();
            residence.get_or_load(changed, make).test_value()?;
            assert_eq!(
                loads.get(),
                before + 1,
                "authority facet {field} must not hit another catalog"
            );
        }
        Ok(())
    }
}
