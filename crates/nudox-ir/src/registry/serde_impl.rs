use super::{EntryIdx, ErasedUniqueId, Registry, RegistryResolver, RegistryState, UniqueId};

impl<R: RegistryResolver> Registry<R> {
    pub fn serialize<T, S>(&self, serializer: S, value: &T) -> Result<S::Ok, S::Error>
    where
        T: serde::Serialize,
        S: serde::Serializer,
    {
        serde_context::serialize_with_context(value, serializer, &self.state)
    }

    pub fn deserialize<'de, T, D>(&self, deserializer: D) -> Result<T, D::Error>
    where
        T: serde::Deserialize<'de>,
        D: serde::Deserializer<'de>,
    {
        DeserContext {
            state: &self.state,
            deser: |d| erased_serde::deserialize::<UniqueId<R::EntryId>>(d).map(UniqueId::upcast),
        }
        .deserialize(deserializer)
    }
}

pub struct DeserContext<'a> {
    state: &'a RegistryState,
    deser: DeserFn,
}

struct DeserFnCx {
    deser: DeserFn,
}

type DeserFn = fn(&mut dyn erased_serde::Deserializer) -> erased_serde::Result<ErasedUniqueId>;

impl DeserContext<'_> {
    pub fn deserialize<'de, T, D>(&self, deserializer: D) -> Result<T, D::Error>
    where
        T: serde::Deserialize<'de>,
        D: serde::Deserializer<'de>,
    {
        serde_context::deserialize_with_context(
            deserializer,
            (self.state, &DeserFnCx { deser: self.deser }),
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
            let state = cx.get::<RegistryState>().map_err(D::Error::custom)?;

            let DeserFnCx { deser } = cx.get::<DeserFnCx>().map_err(D::Error::custom)?;

            let deserializer = &mut <dyn erased_serde::Deserializer>::erase(deserializer);

            let id = deser(deserializer).map_err(D::Error::custom)?;

            Ok(state.unique_id_to_entry_idx(id).cast())
        })
    }
}
