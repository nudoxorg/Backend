//! Query, read, and graph source-facing contracts.

use crate::{Binding, Error, Limits, QueryVersion, ReadManifest};

/// One declared semantic field read.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Read {
    /// Declared semantic contract/facet family.
    pub contract: String,
    /// Declared field or selector.
    pub field: String,
}

impl Read {
    /// Creates a declared read after the adapter validates its contents.
    #[must_use]
    pub fn new(contract: String, field: String) -> Self {
        Self { contract, field }
    }
}

pub(crate) fn canonical_reads(
    mut reads: Vec<Read>,
    limits: Limits,
) -> Result<(Vec<Read>, ReadManifest, QueryVersion), Error> {
    let limits = limits.validate()?;
    if reads.len() > limits.max_reads {
        return Err(Error::SizeLimit);
    }
    if reads
        .iter()
        .any(|read| read.contract.is_empty() || read.field.is_empty())
    {
        return Err(Error::UndeclaredRead);
    }
    if reads.iter().any(|read| {
        read.contract.len() > limits.max_field_bytes || read.field.len() > limits.max_field_bytes
    }) {
        return Err(Error::SizeLimit);
    }
    reads.sort();
    if reads.windows(2).any(|window| window[0] == window[1]) {
        return Err(Error::MalformedInput);
    }
    let mut bytes = Vec::new();
    for read in &reads {
        let read_bytes = read
            .contract
            .len()
            .checked_add(read.field.len())
            .and_then(|size| size.checked_add(16))
            .ok_or(Error::SizeLimit)?;
        if bytes
            .len()
            .checked_add(read_bytes)
            .ok_or(Error::SizeLimit)?
            > limits.max_total_bytes
        {
            return Err(Error::SizeLimit);
        }
        bytes.extend_from_slice(&(read.contract.len() as u64).to_be_bytes());
        bytes.extend_from_slice(read.contract.as_bytes());
        bytes.extend_from_slice(&(read.field.len() as u64).to_be_bytes());
        bytes.extend_from_slice(read.field.as_bytes());
    }
    let manifest = ReadManifest::from_value(&bytes);
    let query = QueryVersion::from_value(&bytes);
    Ok((reads, manifest, query))
}

/// Exact query descriptor with canonical read-manifest identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Query {
    /// Canonically ordered declared reads.
    pub reads: Vec<Read>,
    /// Exact read-manifest version.
    pub read_manifest: ReadManifest,
    /// Query/read-set identity used by cursors.
    pub version: QueryVersion,
}

impl Query {
    /// Creates a canonical query descriptor.
    ///
    /// # Errors
    ///
    /// Returns a typed error for undeclared/duplicate reads or size violations.
    pub fn new(reads: Vec<Read>, limits: Limits) -> Result<Self, Error> {
        let (reads, read_manifest, version) = canonical_reads(reads, limits)?;
        Ok(Self {
            reads,
            read_manifest,
            version,
        })
    }

    pub(crate) fn validate(&self, limits: Limits) -> Result<(), Error> {
        if Self::new(self.reads.clone(), limits)? != *self {
            return Err(Error::MalformedInput);
        }
        Ok(())
    }
}

/// Bounded cursor bound to exact graph inputs and read set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    binding: Binding,
    query: QueryVersion,
    offset: usize,
}

impl Cursor {
    /// Creates a cursor at an offset. Adapter admission checks the offset and
    /// all identity fields before use.
    #[must_use]
    pub const fn new(binding: Binding, query: QueryVersion, offset: usize) -> Self {
        Self {
            binding,
            query,
            offset,
        }
    }

    /// Returns the exact source binding.
    #[must_use]
    pub(crate) const fn binding(self) -> Binding {
        self.binding
    }

    /// Returns the exact query identity.
    #[must_use]
    pub(crate) const fn query(self) -> QueryVersion {
        self.query
    }

    /// Returns the result offset.
    #[must_use]
    pub(crate) const fn offset(self) -> usize {
        self.offset
    }
}

/// One canonical graph row keyed by logical semantic identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GraphRow {
    /// Stable logical row key.
    pub key: u64,
    /// Ordered scalar projection values.
    pub values: Vec<String>,
}

/// One logical graph relation insertion/replacement/deletion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GraphChange {
    /// Inserts or replaces one graph row.
    Upsert {
        /// Stable logical row key.
        key: u64,
        /// Complete row projection.
        values: Vec<String>,
    },
    /// Deletes one existing graph row.
    Delete {
        /// Stable logical row key.
        key: u64,
    },
}

impl GraphChange {
    pub(crate) fn key(&self) -> u64 {
        match self {
            Self::Upsert { key, .. } | Self::Delete { key } => *key,
        }
    }
}
