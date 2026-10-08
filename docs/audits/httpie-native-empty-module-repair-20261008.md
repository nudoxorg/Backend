# HTTPie native module-coordinate repair

The selected Python authority could refuse the entire HTTPie project when
native navigation reached an empty package initializer. Ruff's synthetic module
container has no written identifier and has a 0..0 extent for an empty file;
that is not a valid named-declaration source coordinate.

The producer now excludes synthetic module containers when joining native
navigation to named declarations. Module occurrences remain unresolved. Real
class, constructor and function coordinates keep their strict source and program
identities; no file is skipped and no coordinate admission is relaxed.

## Executed validation

The actual native source was `b97584982d77f51b1bb182426f4be77b2396b481`,
including production/test atom `0247d28a30c3f9e8ee7cf7932b49a33fbed33865`.
On h16001mac, all nine Python project tests passed with zero failures or ignored
tests. A freshly compiled native report example then admitted all 133 Python
modules in two fresh states of unmodified HTTPie commit
`5b604c37c6c67e18e7c3e9aee6c88a8c22b98345`.

The primary agent independently replayed the source/program identity audit over
the exact 265-file input archive, checked all 5,240 emitted target/callee
coordinates and the known `tests/test_cli.py:242` class/constructor call, checked
2,067 source inputs and fresh Cargo artifacts, and rehashed the sealed remote
example image. The baseline's native refusal and all raw phase receipts remain
preserved alongside the successful candidate.

The canonical integration matches 2,066 of those input files byte for byte. Its
only input difference is an existing `allocation-counter` dependency line in
Tantivy's benchmark-related lock entry; the selected Python dependency graph is
unchanged. This is correspondence evidence, not a fresh whole-workspace build.

## Remaining acceptance

This proves the native Python repair. It does not prove installed CLI/MCP
publication, package dependency projection, public graph continuations, semantic
search, or cold daemon startup. Those have separate open gates. The broader
typed ProjectReport diagnostic redesign is intentionally absent from this fix.

Evidence and hashes: [manifest](../operations/evidence/python-httpie-native-20261008/manifest.json),
[causal comparison](../operations/evidence/python-httpie-native-20261008/causal-comparison.json),
[independent audit](../operations/evidence/python-httpie-native-20261008/root-independent-audit.json),
and [raw native receipts](../operations/evidence/python-httpie-native-20261008/raw-proof-20261008.tar.gz).
