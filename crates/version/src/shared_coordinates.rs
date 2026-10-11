//! Immutable coordinate storage with an optional retained allocation owner.
//!
//! This value carries storage lifetime, not an executor or a memory-accounting policy.
//! Consumers can validate the concrete immutable slice once; no user-provided accessor
//! can change its contents after admission.

use std::{fmt::Debug, sync::Arc};

/// Closed immutable storage shared by coordinate producers and vector consumers.
#[derive(Clone, Debug)]
pub struct SharedCoordinates(Storage);

#[derive(Clone, Debug)]
enum Storage {
    Plain(Arc<[f32]>),
    Retained(Arc<RetainedCoordinates>),
}

#[derive(Debug)]
struct RetainedCoordinates {
    values: Arc<[f32]>,
    _owner: Box<dyn Debug + Send + Sync>,
}

impl SharedCoordinates {
    /// Retains an allocation owner for as long as any storage clone survives.
    /// The owner carries its own admission policy; this constructor makes no credit claim.
    #[must_use]
    pub fn retain<T: Debug + Send + Sync + 'static>(values: Arc<[f32]>, owner: T) -> Self {
        Self(Storage::Retained(Arc::new(RetainedCoordinates {
            values,
            _owner: Box::new(owner),
        })))
    }

    /// Compares storage without extracting an owner that could outlive its charge.
    #[must_use]
    pub fn shares_storage(&self, other: &Self) -> bool {
        Arc::ptr_eq(self.storage(), other.storage())
    }

    fn storage(&self) -> &Arc<[f32]> {
        match &self.0 {
            Storage::Plain(values) => values,
            Storage::Retained(retained) => &retained.values,
        }
    }
}

impl From<Arc<[f32]>> for SharedCoordinates {
    fn from(values: Arc<[f32]>) -> Self {
        Self(Storage::Plain(values))
    }
}
impl AsRef<[f32]> for SharedCoordinates {
    fn as_ref(&self) -> &[f32] {
        self.storage()
    }
}
impl std::ops::Deref for SharedCoordinates {
    type Target = [f32];
    fn deref(&self) -> &[f32] {
        self.as_ref()
    }
}
impl PartialEq for SharedCoordinates {
    fn eq(&self, other: &Self) -> bool {
        self.as_ref() == other.as_ref()
    }
}
