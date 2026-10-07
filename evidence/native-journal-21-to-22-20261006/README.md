# Genuine native journal21 to journal22 compatibility

Native historical codec source `9b0001b3af1b590b8f1e9fc81ac2273782bbb629`
emitted literal container-v3 snapshot and compact-event bytes, raw grammar
versions `[21,21]`. Its single native emitter test passed and independently
decoded its output. The checked-in immutable fixture is 21,790 bytes, SHA256
`464c5ad98c4988c0dcd4feb8e235fbead3d846f8918cf058f8de1ea344d32025`.
No version or proof bytes were rewritten. This is native codec evidence, not
application compiler IR evidence.

Strict22 source `22da01e7e4e7c5bd3d9d19fef9fa0f31996ba4eb`: full native
`builtin::view_journal::tests::` module **19 passed, 0 failed**, 0.84 seconds
execution. This includes literal historical snapshot/event proof admission,
append of a checked v22 event, exact retained `[21,21,22]` history, two
independent cold reopens, unchanged old prefix, strict live21 refusal and
unknown-version/field/proof/basis/descriptor negatives. Existing corruption,
torn-tail, retry, bounded compaction and capture-generation tests also passed.

Emitter permit 67648/launcher67649: 23:47:55.895404 UTC, fresh fleet age 5.890338,
terminal0 at 23:51:39.113815. Strict22 permit85272/launcher85274:
23:54:40.513422 UTC, fresh fleet age5.519332, terminal0 at23:58:30.740964.
One exclusively owned remote worktree/compiler group, managed lifetime lock,
explicit `-j4`. Initial23:53 admission refused because local available memory
was below8GiB; that unsuccessful sample is retained separately, and launched no
compiler. Only the later successful fresh census authorized the actual launch.

The archive includes full test names/stdout/stderr, exact launch/terminal,
fresh fleet samples, wrappers, fixture producer proof and artifact hashes.
`native-journal-21-to-22-receipts.tar.gz`: 81,441 bytes, SHA256
`dcd2399e24dbb30bab2a0ac17f81cf35f20e25a53ab318b208aba3ead81836b1`.
No executable image, HOME or application state is included.

The source also includes the separately native302-tested marker-case followup.
Actual public d54 CLI/MCP HTTPie identity evidence remains a distinct immutable
binary cohort; the fixture test addition changes no production persistence code.
