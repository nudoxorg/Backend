//! Shared fixtures for the runtime integration tests: a deterministic,
//! offline embedder (so vector-space behavior is testable without a model or a
//! qdrant), symbol builders, and a self-cleaning temporary directory.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use heart::{Guid, Language, Name, PackageId, Symbol, SymbolId, SymbolKind};
use runtime::error::EmbedError;
use runtime::vector::{
    EmbedRole, Embedder, Embedding, EmbeddingPurpose,
    model::{E5Small, EmbeddingModel, ModelId},
};

/// A deterministic pseudo-embedding: an FNV-1a seed over `(purpose, text)`
/// drives an xorshift stream, one value per dimension. Same input -> same
/// vector; any byte difference (including the purpose tag) decorrelates the
/// stream completely.
pub fn deterministic_embedding(text: &str, purpose: EmbeddingPurpose) -> Embedding<E5Small> {
    let tag = match purpose {
        EmbeddingPurpose::Code => b'c',
        EmbeddingPurpose::Documentation => b'd',
    };
    let mut seed: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in std::iter::once(tag).chain(text.bytes()) {
        seed = (seed ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut state = seed.max(1);
    let values: Vec<f32> = (0..E5Small::DIMENSIONS)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            // Uniform-ish in [-1, 1): 24 high bits scaled into [0, 1), shifted.
            (state >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        })
        .collect();
    Embedding::from_vec(values).expect("generated exactly E5Small::DIMENSIONS values")
}

/// An offline [`Embedder`] built on [`deterministic_embedding`], counting every
/// embed call so tests can assert *when* embedding work happens.
pub struct DeterministicEmbedder {
    model: ModelId,
    calls: AtomicUsize,
}

impl DeterministicEmbedder {
    pub fn new() -> Self {
        Self {
            model: ModelId::try_new("test/deterministic-e5-small").expect("static id is non-empty"),
            calls: AtomicUsize::new(0),
        }
    }

    /// How many embed calls (single or batched texts) have run.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl Embedder for DeterministicEmbedder {
    type Model = E5Small;

    fn model(&self) -> &ModelId {
        &self.model
    }

    async fn embed(
        &self,
        text: &str,
        purpose: EmbeddingPurpose,
        _role: EmbedRole,
    ) -> Result<Embedding<E5Small>, EmbedError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(deterministic_embedding(text, purpose))
    }

    async fn embed_batch(
        &self,
        texts: &[&str],
        purpose: EmbeddingPurpose,
        _role: EmbedRole,
    ) -> Result<Vec<Embedding<E5Small>>, EmbedError> {
        self.calls.fetch_add(texts.len(), Ordering::SeqCst);
        Ok(texts.iter().map(|text| deterministic_embedding(text, purpose)).collect())
    }
}

/// A stable test symbol id from a small integer.
pub fn symbol_id(n: u64) -> SymbolId {
    SymbolId::from_uuid(Guid::from_u128(0x5313_0000 + u128::from(n)))
}

/// A stable test package id from a small integer.
pub fn package_id(n: u64) -> PackageId {
    PackageId::from_uuid(Guid::from_u128(0x9AC6_0000 + u128::from(n)))
}

/// A full symbol record for indexing tests.
pub fn symbol(n: u64, plain: &str, fully_qualified: &str) -> Symbol {
    Symbol {
        id: symbol_id(n),
        package: package_id(0),
        ecosystem: Language::Rust,
        name: Name { plain: plain.into(), fully_qualified: fully_qualified.into() },
        kind: SymbolKind::Type,
    }
}

/// A unique temporary directory removed on drop.
pub struct TempDir {
    path: PathBuf,
}

impl TempDir {
    pub fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("runtime-test-{label}-{}", Guid::new_v4()));
        std::fs::create_dir_all(&path).expect("temp dir under std::env::temp_dir is creatable");
        Self { path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}
