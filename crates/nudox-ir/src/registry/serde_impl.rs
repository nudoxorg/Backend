use std::marker::PhantomData;

use super::{
    EntryIdx, ErasedRegistryState, ErasedUniqueId, Registry, RegistryResolver, RegistryState,
    UniqueId,
};

impl<R: RegistryResolver> Registry<R> {
    pub fn serialize<T, S>(&self, serializer: S, it: &T) -> Result<S::Ok, S::Error>
    where
        T: serde::Serialize,
        S: serde::Serializer,
    {
        self.state.serialize::<T, S>(serializer, it)
    }

    pub fn deserialize<'de, T, D>(&self, deserializer: D) -> Result<T, D::Error>
    where
        T: serde::Deserialize<'de>,
        D: serde::Deserializer<'de>,
    {
        self.state.deserialize::<T, D>(deserializer)
    }
}

impl<R> RegistryState<R> {
    pub fn serialize<T, S>(&self, serializer: S, it: &T) -> Result<S::Ok, S::Error>
    where
        T: serde::Serialize,
        S: serde::Serializer,
    {
        serde_context::serialize_with_context(it, serializer, self.erased())
    }
}

impl<R> RegistryState<R>
where
    R: RegistryResolver,
{
    pub fn deserialize<'de, T, D>(&self, deserializer: D) -> Result<T, D::Error>
    where
        T: serde::Deserialize<'de>,
        D: serde::Deserializer<'de>,
    {
        let provider = Provider::<R>::new();

        serde_context::deserialize_with_context(
            deserializer,
            (self.erased(), &provider as &dyn DeserProvider),
        )
    }
}

impl<T> serde::Serialize for EntryIdx<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::Error;

        serde_context::context_scope(|cx| {
            cx.get::<ErasedRegistryState>()
                .map_err(S::Error::custom)?
                .entry_idx_to_unique_id(self.raw())
                .serialize(serializer)
        })
    }
}

impl<'de, T> serde::Deserialize<'de> for EntryIdx<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;

        serde_context::context_scope(|cx| {
            let provider = cx.get::<dyn DeserProvider>().map_err(D::Error::custom)?;
            let state = cx.get::<ErasedRegistryState>().map_err(D::Error::custom)?;

            let deserializer = &mut <dyn erased_serde::Deserializer>::erase(deserializer);

            let id = provider
                .deser_erased_id(deserializer)
                .map_err(D::Error::custom)?;

            Ok(state.unique_id_to_entry_idx(id).cast())
        })
    }
}

struct Provider<R>(PhantomData<R>);

impl<R> Provider<R> {
    fn new() -> Self {
        Self(PhantomData)
    }
}

trait DeserProvider: 'static {
    fn deser_erased_id(
        &self,
        deserializer: &mut dyn erased_serde::Deserializer,
    ) -> erased_serde::Result<ErasedUniqueId>;
}

impl<R> DeserProvider for Provider<R>
where
    R: RegistryResolver,
{
    fn deser_erased_id(
        &self,
        deserializer: &mut dyn erased_serde::Deserializer,
    ) -> erased_serde::Result<ErasedUniqueId> {
        erased_serde::deserialize::<UniqueId<R::EntryId>>(deserializer).map(UniqueId::upcast)
    }
}
