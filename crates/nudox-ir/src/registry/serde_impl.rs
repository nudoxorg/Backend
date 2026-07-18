use super::{EntryIdx, ErasedUniqueId, Registry, RegistryResolver, RegistryState, UniqueId};

impl<R: RegistryResolver> Registry<R> {
    pub fn serialize<T, S>(&self, serializer: S, it: &T) -> Result<S::Ok, S::Error>
    where
        T: serde::Serialize,
        S: serde::Serializer,
    {
        serde_context::serialize_with_context(it, serializer, &self.state)
    }

    pub fn deserialize<'de, T, D>(&self, deserializer: D) -> Result<T, D::Error>
    where
        T: serde::Deserialize<'de>,
        D: serde::Deserializer<'de>,
    {
        serde_context::deserialize_with_context(
            deserializer,
            (&self.state, &self.resolver as &dyn DeserProvider),
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
            cx.get::<RegistryState>()
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
            let resolver = cx.get::<dyn DeserProvider>().map_err(D::Error::custom)?;

            let state = cx.get::<RegistryState>().map_err(D::Error::custom)?;

            let id = resolver
                .deser_erased_id(&mut <dyn erased_serde::Deserializer>::erase(deserializer))
                .map_err(D::Error::custom)?;

            Ok(state.unique_id_to_entry_idx(id).cast())
        })
    }
}

trait DeserProvider: 'static {
    fn deser_erased_id(
        &self,
        deserializer: &mut dyn erased_serde::Deserializer,
    ) -> erased_serde::Result<ErasedUniqueId>;
}

impl<R: RegistryResolver> DeserProvider for R {
    fn deser_erased_id(
        &self,
        deserializer: &mut dyn erased_serde::Deserializer,
    ) -> erased_serde::Result<ErasedUniqueId> {
        erased_serde::deserialize::<UniqueId<R::EntryId>>(deserializer).map(UniqueId::upcast)
    }
}
