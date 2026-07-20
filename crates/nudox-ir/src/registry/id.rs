use std::{
    any::Any,
    fmt::{self, Debug},
    hash::{Hash, Hasher},
};

use erased_serde::Serialize;

use crate::package::PackageId;

use super::RegistryResolver;

const INVALID_ENTRY_ID_DOWNCAST_MESSAGE: &str = ""; // TODO

#[repr(C)]
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct UniqueId<T: ?Sized> {
    package: PackageId,
    entry: Option<Box<T>>,
}

impl<T: ?Sized> UniqueId<T> {
    pub fn package(&self) -> PackageId {
        PackageId::clone(&self.package)
    }

    pub fn entry(&self) -> Option<&T> {
        self.entry.as_deref()
    }

    pub fn root(package: PackageId) -> Self {
        UniqueId {
            package,
            entry: None,
        }
    }
}

impl<T> UniqueId<T> {
    pub fn new(package: PackageId, entry: T) -> Self {
        UniqueId {
            package,
            entry: Some(Box::new(entry)),
        }
    }
}

pub(super) type ErasedUniqueId = UniqueId<ErasedEntryId>;

impl<T: EntryId> UniqueId<T> {
    pub(super) fn upcast(self) -> ErasedUniqueId {
        UniqueId::<dyn DynEntryId> {
            package: self.package,
            entry: self.entry.map(|it| it as Box<dyn DynEntryId>),
        }
        .erase()
    }
}

impl ErasedUniqueId {
    #[expect(unused)]
    pub(super) fn downcast<R: RegistryResolver>(self) -> UniqueId<R::EntryId> {
        let UniqueId { package, entry } = self.restore();

        let entry =
            entry.map(|it| Box::<dyn Any>::downcast(it).expect(INVALID_ENTRY_ID_DOWNCAST_MESSAGE));

        UniqueId { package, entry }
    }

    pub(super) fn downcast_ref<R: RegistryResolver>(&self) -> &UniqueId<R::EntryId> {
        if self
            .entry
            .as_ref()
            .is_none_or(|e| e.inner.as_any().is::<R::EntryId>())
        {
            // Safety: TODO
            unsafe { &*std::ptr::from_ref(self).cast() }
        } else {
            panic!(
                "{} (target = {})",
                INVALID_ENTRY_ID_DOWNCAST_MESSAGE,
                std::any::type_name::<R::EntryId>()
            )
        }
    }
}

impl UniqueId<dyn DynEntryId> {
    fn erase(self) -> UniqueId<ErasedEntryId> {
        // Safety: TODO
        unsafe { std::mem::transmute::<Self, UniqueId<ErasedEntryId>>(self) }
    }
}

impl ErasedUniqueId {
    fn restore(self) -> UniqueId<dyn DynEntryId> {
        // Safety: TODO
        unsafe { std::mem::transmute::<Self, UniqueId<dyn DynEntryId>>(self) }
    }
}

pub trait EntryId = serde::Serialize + serde::de::DeserializeOwned + Eq + Hash + Debug + Any;

#[repr(transparent)]
pub(super) struct ErasedEntryId {
    inner: dyn DynEntryId,
}

impl PartialEq for ErasedEntryId {
    fn eq(&self, other: &Self) -> bool {
        self.inner.eq(&other.inner)
    }
}

impl Eq for ErasedEntryId {}

impl Hash for ErasedEntryId {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.inner.hash(state);
    }
}

impl Debug for ErasedEntryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.inner.fmt(f)
    }
}

impl serde::Serialize for ErasedEntryId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        erased_serde::serialize(&self.inner, serializer)
    }
}

trait DynEntryId: Serialize + Debug + Any {
    fn hash(&self, state: &mut dyn Hasher);
    fn eq(&self, other: &dyn DynEntryId) -> bool;
    fn as_any(&self) -> &dyn Any;
}

impl<T: EntryId> DynEntryId for T {
    fn hash(&self, mut state: &mut dyn Hasher) {
        self.hash(&mut state);
    }

    fn eq(&self, other: &dyn DynEntryId) -> bool {
        other
            .as_any()
            .downcast_ref::<Self>()
            .is_some_and(|other| self == other)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}
