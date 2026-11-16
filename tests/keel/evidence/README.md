# Native process migration receipt

`native-linux-20261116.json` records an actual run on ilo-prod of the two native
worker cases in `native.nix`: all 17 process steps passed in 186 ms. The report
binds the exact evaluated manifest (SHA-256
`635c4ae2534efc9109cb779f7902d91fbc05c91d208c4f5c45ea69e769f02446`),
worker executable hashes and raw response hashes. The driver was built with the
workspace lockfile at harness revision `b3d8e706da`; the native workers came from
the independently built c5f063 staging worktree. Raw replies and the manifest
remain at `/root/nudox-keel-native-expanded-evidence-20261116` on that host.

This receipt permitted replacing seven direct in-crate Stylo example functions
with worker-process contract rows. The requirements are retained: media and
selector/declaration/dependency facts; duplicate and escaped source occurrences;
comment-skipping; rejected-rule rollback; BOM and native UTF-16 locations; and
permitted, disabled and late imports. Callback cancellation and support-fingerprint
helper coverage remain pending their own observable replacements. The CSS/SQL
owner, CLI and MCP companion history is a separate required case; this worker
receipt does not establish owner admission or end-to-end readiness.
