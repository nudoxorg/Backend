# Exact reviewed capture/document22 union native gate

Root-reviewed source `96b04b141affd653e3c6932dc8c325f351775f8f`, tree
`d15332203646b6d01b0decb14a1e3c6e03bc9a24`, clean owned remote worktree.
Cargo.lock SHA256 `b508bc8978b37e09e54887a76cdb7055a4188f1243770057784a08ad61560268`.
The independently read source manifest includes Git blob identities and SHA256
for all32 changed Rust/fixture files against the a659 predecessor.

All three native ARM64 gates passed:

- Document projection/selection:2 passed,0 failed,0.01s. Permit44259/launcher44262,
  start00:04:47.652233 UTC, fleet age5.724433s, terminal0 at00:09:09.065792.
- Full view-journal module:19 passed,0 failed,1.01s. Permit62721/launcher62724,
  start00:09:41.582727, fleet age5.396044s, terminal0 at00:09:49.217383.
- Full library:302 passed,0 failed,1 preexisting ignored,2.82s.
  Permit63949/launcher63956, start00:10:22.062286, fleet age5.746307s,
  terminal0 at00:11:48.620652.

Every Cargo launch separately required a new successful full-fleet census and
owned lifetime `compiler-index.lock`, explicit `-j4`, one exclusive worktree
and cache graph. Each predecessor exited before the next admission. All owned
compiler/runtime groups were retired after the gates. Native environment,
managed wrapper arguments, full stdout/test IDs/stderr and artifact hashes remain.

The remote origin fetch failed host-key verification. No trust check was relaxed:
the exact shared local Git branch was bundled, the bundle head was checked, then
transferred through the existing verified h16001mac SSH route. Remote commit,
tree, clean state, lock and file identities were checked independently before
launch. This source includes Root's capture/quality fixes beyond the original
document/journal cohort.

`document-union-96b-native-receipts.tar.gz`:113,962B, SHA256
`75822f29e97f6cfe339a1cdca51ef8db54cd62e691e012567927ea665227d0f5`.
No executable image, HOME or application state is included. No application trio
was rebuilt from96b for these receipts; the separately retained d54 public
HTTPie CLI/MCP identity and21-client refusal transcripts remain distinct evidence.
