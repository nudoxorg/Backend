# Recovery integration and release sequence

This updates Backend PR #22 after the release/tooling PRs merged. It remains a
draft until the joined product passes acceptance. MachineConfigurations #45 can
be merged and deployed independently to activate the already-merged Auth/Web
routing; it does not distribute a new desktop application.

## Integrated source

The branch includes canonical `6c98128cc5c09be49e011482495e3a845a94f659`,
the recovered integration checkpoint
`3e4d2c208f0bbca840e4cb3f495048affccaea55`, and the existing PR #22 history
through `02970471c7afd8b510704420ccae753d9307a145`. The merge preserves both
histories instead of replacing the PR or replaying old release changes.

The joined source includes project-local TypeScript discovery and config/import/
library admission, paged full-source facts and verified lazy reads, the final
identified compact-row overflow repair, typed compiler refusals through shared
CLI/MCP presentation, and recovered native publication, focus, Graph and Reader
repairs. The launcher admits project toolchains instead of imposing the builder's
TypeScript paths. These are implementation changes; source reachability alone
does not establish installed acceptance.

## Checks on this joined branch

- The package Python suite passes: 32 tests.
- The five-package Rust all-target check is being run with the pinned compiler
  shell. Its result must be recorded before treating compilation as accepted.
- Installed native application, complete Plural semantic indexing, and joined
  GUI acceptance have not passed. Historical results in the
  [recovery ledger](../architecture/briefs/checkpoint-2026-10-05/MEETING-BLOCKERS-RECOVERY.md)
  remain historical results unless reproduced for this branch.

Reproduce the source checks from this checkout:

```sh
python3 -m unittest discover -s tools/package -p 'test_*.py' -v
nix develop .#compiler --command cargo check --locked --offline --jobs 2 \
  -p backend-engine -p backend-library -p backend-local-service \
  -p backend-present -p backend-desktop --all-targets
```

## Merge and operator sequence

1. Auth #6 and Web #10 are already merged. Merge MachineConfigurations #45
   after `concourse/nixos-pr` succeeds on its updated head. Deploy production
   using its `docs/backend-release.md` instructions. Verify the seeded release
   catalog, all four legacy redirects, and the deployed Web version labels.
2. Keep Backend #22 in draft while validating the integrated fixes. Run focused
   TypeScript admission, large-file cold capture, typed-refusal, and native
   publication/focus/Reader/Graph regressions against the joined tree. Repair
   remaining failures rather than carrying earlier passing receipts forward.
3. Test matched installed CLI/MCP/GUI binaries outside the Nix shell and source
   checkout. A normal project-local TypeScript installation must work. The full
   Plural reproduction needs its exact checkout, successful semantic publication,
   meaningful queries/source reads, edit/reindex, interrupted-operation recovery
   and cold restart. Native captures and keyboard interactions must pass review.
4. Merge Backend #22 only after current-head CI and that product acceptance
   pass. Select the resulting exact clean canonical SHA/tree for packaging;
   merging Backend does not require a second Colmena apply for the desktop.
5. Follow [the Mac release runbook](macos-release.md): supply receipted native
   helpers, a Developer ID identity and notarization profile; build/sign the
   candidate; accept the exact final archive; then stage and promote those same
   bytes. Use the manifests' exact hashes, not the old ad-hoc preview's receipts.

The release pipeline stays disabled until routing, dedicated publisher
credentials and an accepted signed/notarized candidate are ready. Do not enable
it as part of the initial MachineConfigurations production apply. Canonical CI
is currently paused; obtaining the release verifier's required
`concourse/backend-fast` success for the chosen canonical SHA is a separate
operator step in the MachineConfigurations runbook.
