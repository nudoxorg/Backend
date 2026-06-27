# nudox-embed

Implementations of the `Embedder` trait from `nudox-core`. This crate is deliberately kept free of provider SDK dependencies. OpenAI, Cohere, or other third-party embedders should be implemented as separate `Embedder` impls in their own crate and passed into the pipeline as `Box<dyn Embedder>`.

## Implementations

**`MockEmbedder`** — returns a constant vector of `0.1_f32` values for every input. Useful only in unit tests where exact vector values do not matter. Constructor: `MockEmbedder::new(dim)`.

**`PlaceholderEmbedder`** — deterministic, content-derived vectors with no external dependencies. Hashes the chunk text and `EmbeddingPurpose` through a seeded LCG to produce values in `[-1.0, 1.0]`. Same input always yields the same vector; different inputs almost certainly differ. Suitable for development and CI. Constructor: `PlaceholderEmbedder::new(model_id, dim)`.

**`InProcessEmbedder`** — stub for future candle or ort in-process model inference. Currently returns deterministic placeholder vectors seeded from the model id, chunk, and purpose. Constructor: `InProcessEmbedder::load(&Path)`, which derives the model id from the path's final component.

**`RemoteEmbedder`** — stub for a future self-hosted HTTP embedding service. Currently returns deterministic placeholder vectors seeded from the endpoint URL, model id, chunk, and purpose. Constructor: `RemoteEmbedder::new(endpoint_url, model_id)`.

## Usage example

```rust
use nudox_embed::PlaceholderEmbedder;
use nudox_pipeline::{Pipeline, PipelineConfig};

let pipeline = Pipeline::new(
    vec![
        Box::new(PlaceholderEmbedder::new("placeholder-v1", 256)),
    ],
    PipelineConfig::default(),
);
```

To run multiple embedders simultaneously, pass them all in the `Vec`. The pipeline produces one `EmbeddingRecord` per (embedder, purpose) pair.

```rust
use nudox_embed::{PlaceholderEmbedder, MockEmbedder};

let pipeline = Pipeline::new(
    vec![
        Box::new(PlaceholderEmbedder::new("placeholder-v1", 256)),
        Box::new(MockEmbedder::new(256)),
    ],
    PipelineConfig::default(),
);
```

## Provider integration

To add a real embedding provider, implement `nudox_core::Embedder` in a new crate (e.g. `nudox-embed-openai`) and pass it in as `Box<dyn Embedder>`. Do not add provider SDK dependencies to this crate.
