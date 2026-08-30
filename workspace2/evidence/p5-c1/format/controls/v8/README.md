# v8 detached control

This evidence-only control preserves the canonical 23-byte empty two-lane header as replayable hex.
Decode `detached-control-source.hex` with `xxd -r -p` and verify the result is exactly 23 bytes.
The test source intentionally has exactly one named terminal and exactly one actual-rlib helper
signature. It contains no production implementation or owned collection.
