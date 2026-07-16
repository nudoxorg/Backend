use std::{any::Any, hash::Hash};

use super::{RawEntryIdx, RegistryState};

pub(super) use self::private::DynRegistryResolver;

pub trait EntryId = serde::Serialize + serde::de::DeserializeOwned + Hash + Any;

pub trait RegistryResolver: DynRegistryResolver {
    /// A type that can be used to uniquely identify an Entry between different
    /// packages within a registry. Should be constructable based on information
    /// available within the IR of a package that is consuming an external
    /// package's entry as the target.
    type EntryId: EntryId;

    /// resolves an `EntryId` to an actual `EntryIdx` that points to the given
    /// Entry.
    ///
    /// This can also load other package IRs if needed, through the provided
    /// `RegistryState`
    // TODO: determine error handling for bad usage: is a panic OK?
    fn entry_id_to_idx(&self, id: Self::EntryId, state: &RegistryState) -> RawEntryIdx;

    /// resolves an EntryIdx to it's unique internal `EntryId`
    fn idx_to_entry_id(&self, idx: RawEntryIdx, state: &RegistryState) -> Self::EntryId;
}

impl<T: RegistryResolver> DynRegistryResolver for T {
    fn __idx_to_entry_id(
        &self,
        idx: RawEntryIdx,
        state: &RegistryState,
    ) -> Box<dyn erased_serde::Serialize> {
        Box::new(self.idx_to_entry_id(idx, state))
    }

    fn __deser_entry_id_to_idx(
        &self,
        deserializer: &mut dyn erased_serde::Deserializer,
        state: &RegistryState,
    ) -> erased_serde::Result<RawEntryIdx> {
        erased_serde::deserialize(deserializer).map(|id| self.entry_id_to_idx(id, state))
    }
}

mod private {
    use super::{RawEntryIdx, RegistryState};

    /// An internal-only auto-implemented subtrait of `Registry` that's used to
    /// do type-erased shenanigans to allow (de)serializing `EntryIdx`s when
    /// using our context helpers
    ///
    /// essentially, it's the backing behind the mapping between `EntryIdx` that
    /// exists in-memory and the `EntryId` that's actually (de)serialized.
    pub trait DynRegistryResolver: 'static {
        /// gets the `EntryId` for the corresponding `RawEntryIdx` and converts
        /// it to a type-erased serializable type
        fn __idx_to_entry_id(
            &self,
            idx: RawEntryIdx,
            state: &RegistryState,
        ) -> Box<dyn erased_serde::Serialize>;

        fn __deser_entry_id_to_idx(
            &self,
            deserializer: &mut dyn erased_serde::Deserializer,
            state: &RegistryState,
        ) -> erased_serde::Result<RawEntryIdx>;
    }
}
