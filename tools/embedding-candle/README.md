# Bounded Candle embedding provider

This executable implements the local service's verified BEM2 embedding protocol with Candle's BERT implementation. Its BERT layers follow Candle 0.11.0's upstream implementation; its LayerNorm uses the same formula as Candle's CPU path expressed as tensor operations so it also runs on Candle Metal. On macOS it uses Candle Metal when `BACKEND_EMBEDDING_DEVICE=metal`; `cpu` selects Candle's CPU device for reference checks. Other platforms build the CPU provider. Device selection is part of the local embedding runtime identity, so a CPU vector cannot be reused under a Metal identity or the reverse.

The helper is pinned to the Apache-2.0 `sentence-transformers/all-MiniLM-L6-v2` files at Hugging Face revision `1110a243fdf4706b3f48f1d95db1a4f5529b4d41`:

| File | Size | BLAKE3 identity required by the service |
| --- | ---: | --- |
| `model.safetensors` | 90,868,376 bytes | `8087e9bf97c265f8435ed268733ecf3791825ad24850fd5d84d89e32ee3a589a` |
| `tokenizer.json` | 466,247 bytes | `82483bb4f0bdb81779f295ecc5a93285d2156834e994a2169f9800e4c8f250c1` |

The model produces 384 coordinates. The helper clears the tokenizer JSON's fixed export padding and pads dynamically within each bounded microbatch. It truncates to 256 wordpieces, applies the tokenizer's normalizer and special-token template, sends attention masks into BERT, mean-pools all attended tokens (including `[CLS]` and `[SEP]`), and L2-normalizes. It validates all artifacts and frame identities before inference, processes at most 16 inputs per tensor microbatch and 256 per BEM2 invocation, and emits a response only after all vectors pass shape, finite-value, and norm checks. There is no generated or hash-based vector fallback.

Download only those two files from the pinned revision, then build the helper for the current host:

```sh
curl --fail --location \
  'https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/1110a243fdf4706b3f48f1d95db1a4f5529b4d41/model.safetensors?download=true' \
  --output model.safetensors
curl --fail --location \
  'https://huggingface.co/sentence-transformers/all-MiniLM-L6-v2/resolve/1110a243fdf4706b3f48f1d95db1a4f5529b4d41/tokenizer.json?download=true' \
  --output tokenizer.json
cargo build --release -p backend-embedding-candle
```

Configure the service's existing external-producer settings to point `BACKEND_EMBEDDING_PROGRAM` at the resulting `backend-embedding-candle` executable, `BACKEND_EMBEDDING_MODEL_FILE` at `model.safetensors`, and `BACKEND_EMBEDDING_TOKENIZER_FILE` at `tokenizer.json`. Set `BACKEND_EMBEDDING_PROTOCOL=bem2`, `BACKEND_EMBEDDING_DIMENSIONS=384`, `BACKEND_EMBEDDING_MODEL` and `BACKEND_EMBEDDING_TOKENIZER` to the corresponding `blake3:<identity>` values in the table, and retain the configured query/document treatment values. On macOS, `BACKEND_EMBEDDING_DEVICE` defaults to `metal`; set it to `cpu` to run the reference path. The service also verifies the executable and artifact bytes before and after each bounded invocation.

The model and tokenizer are external artifacts and are not committed to this repository. The helper has strict artifact byte limits (200 MiB model, 4 MiB tokenizer), bounded request and response sizes, and no network access during inference.
