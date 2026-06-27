# nudox-pipeline

Converts raw `PipelineInput` records into complete `BlobInfo` artifacts with embeddings. This crate is responsible for extraction; resolution of global symbol identity is left to the orchestrator.

## Pipeline

`Pipeline` takes a list of `Box<dyn Embedder>` and a `PipelineConfig`. For each input it runs every embedder against the code chunk and, if `embed_docstrings` is enabled and a docstring is present, against the docstring chunk as well. The result is one `EmbeddingRecord` per (embedder, purpose) pair.

```rust
use nudox_embed::PlaceholderEmbedder;
use nudox_pipeline::{Pipeline, PipelineConfig};

let pipeline = Pipeline::new(
    vec![Box::new(PlaceholderEmbedder::new("placeholder-v1", 256))],
    PipelineConfig::default(),
);
let blob_info = pipeline.process(input).await?;
```

`resolved_global_id` is always `None` on the output `BlobInfo`. Resolution is the orchestrator's job.

## Two-embedder setup

```rust
use nudox_embed::{PlaceholderEmbedder, MockEmbedder};
use nudox_pipeline::{Pipeline, PipelineConfig};

let pipeline = Pipeline::new(
    vec![
        Box::new(PlaceholderEmbedder::new("placeholder-v1", 256)),
        Box::new(MockEmbedder::new(256)),
    ],
    PipelineConfig::default(),
);
```

With two embedders and a docstring, `process` produces four `EmbeddingRecord`s: (placeholder, Code), (placeholder, Docstring), (mock, Code), (mock, Docstring).

## PipelineInput

| Field | Type | Description |
|---|---|---|
| `raw_code` | `String` | Raw source code of the symbol occurrence |
| `treesitter_repr` | `TreesitterRepr` | Opaque tree-sitter bytes; format decided by caller |
| `symbol_name` | `String` | Name of the symbol as it appeared at the use site |
| `symbol_span` | `ByteSpan` | Byte span of the symbol within `raw_code` |
| `symbol_origin` | `SymbolOrigin` | Repo-local or external library |
| `metadata` | `ChunkMetadata` | File path, repo id, language, parse timestamp, schema version |
| `docstring` | `Option<String>` | Pre-extracted docstring or comment text |

## PipelineConfig

| Field | Default | Description |
|---|---|---|
| `max_context_lines` | `80` | Maximum surrounding context lines when the enclosing function is too large |
| `embed_docstrings` | `true` | Whether to generate a separate embedding for the docstring if present |

## Note on tree-sitter

Tree-sitter node walking is deferred pending upstream parser work. Currently the full `raw_code` string is used as the embedding chunk verbatim.
