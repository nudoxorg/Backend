# Payload token evidence

`payload-budgets.json` records measurements of the exact UTF-8 payloads emitted
by `backend-present`'s typed encoder. The real counts use `tiktoken==0.11.0`,
encoding `cl100k_base`, with the pinned artifact hash recorded in the fixture:

```text
8613f818e5af318d868379772ce5234e3a3e8519b6c611d94cf708fdfc580fbc
```

The counts were regenerated with Python 3 and the vendored pinned wheel. The
artifact digest is the SHA-256 of compact, sorted JSON containing the encoding
pattern, special-token map, and mergeable-rank map with byte keys rendered as
hex:

```sh
PYTHONPATH=.local/tiktoken-pinned /opt/homebrew/bin/python3 - <<'PY'
import tiktoken
import hashlib, json
from pathlib import Path

encoding = tiktoken.get_encoding("cl100k_base")
artifact = {
    "mergeable-ranks": {k.hex(): v for k, v in sorted(encoding._mergeable_ranks.items())},
    "special-tokens": dict(sorted(encoding._special_tokens.items())),
    "pattern": encoding._pat_str,
}
print("artifact", hashlib.sha256(json.dumps(artifact, sort_keys=True, separators=(",", ":")).encode()).hexdigest())
for path in (
    Path("crates/present/fixtures/common-empty-records-summary.json"),
    Path("crates/present/fixtures/worst-200-full-records.json"),
):
    payload = path.read_bytes()
    print(path, len(payload), len(encoding.encode(payload.decode("utf-8"))))
PY
```

The Rust fixture test regenerates the common and 200-row payloads through the
typed path and asserts their byte counts (`334` and `100025`). The two exact
serialized payloads are checked in beside this README so the tokenizer run is
reproducible. The checked-in real-token counts are `87` and `23460`; the `4`
bytes/token values in the fixture are estimates only and are not used as
tokenizer claims or admission decisions.
