# TypeScript Query Core membership review

The reviewed fixture is `@tanstack/query-core@5.62.8`. The baseline in
`typescript-query-core-membership.json` records each row from the pinned
post-fix run, as printed by the T2 harness (at most 400 characters per row).
The harness compares that set on every run. The manifest also records, for
each external row, the selected compiler image and captured source identity
from the diagnostic fact audit. `pre_fix_removed` records the 214 rows in the
preceding CI view that are absent from the new view; it is comparison evidence,
not a rule that names alone establish an alias.

CI build 1035114 at `bbd5e8a03d523534c87a4f4977a071dfd74ae399` produced
view `58f52d01d891d0d5e3c82cd7d5aa6060a3e14d5b85bf1b64aa78b26d47b63d4f`.
A separate pinned-fixture diagnostic replay of the observed-alias change
produced exactly the same 3,173 printed rows and view root. Its fact audit
joined all 3,173 IDs: 2,531 compiler declarations, 641 captured TypeScript
compiler external targets, and one package. Both unmatched sets were empty.
The 641 entries in `external_facts` retain their exact image and source
identities. The verifier below checks membership, uniqueness, fact classes,
source/image identities, and the total join against those diagnostic logs:

```
python3 .config/ci/audit_typescript_membership.py REPLAY_LOG OWNER_LOG
```

The optional `--pre-owner PRE_OWNER_LOG` checks a separate pre-fix diagnostic:
all 214 rows missing query evidence there are selected-program aliases to
owned rows. Its view root was `7d39f5393f1fceee830c9ace7c86ac237ec691af814d6bb85519fb305703e844`
and its owner log SHA-256 was
`2b41153822278fbe1d79f9221257a8c02267b5f1fa910ba3061208af5e89e14a`.
Those external IDs differ from build 1033468, so that diagnostic establishes
the alias behavior on its own compiler basis rather than an ID-by-ID mapping
of the 214 CI exclusions.

Diagnostic replay SHA-256:
`5926d92af77bcf1e159c31b05f38cc235fef28188bbbbf8652d537730ebe9159`.
Diagnostic owner log SHA-256:
`2efcf61f8714bff969097b76deb7c580e01ff0d295699fdcbec623a145c47e36`.
Those logs use instrumentation that prints fact identities and provenance;
they are not themselves normal CI artifacts. The normal CI row dump matches
the replay's complete printed row set. Build 1033468 before the observed-alias
fix has the same 2,532 owned/package rows, 855 external rows, and no rows
added by the fix; 214 external rows are removed. The exact causal qualification
of those 214 rows requires matching selected-program evidence, not just this
set difference.

The historical snapshot is retained in Git at commit `64a525c85e08c991bfa58acf51eaeb4c71a48b6f`;
it recorded 3,382 rows, five fewer external rows than the 3,387-row pre-fix
view. Its source was `2237c6b6b9e56794536be0bcf99236a734a8273a`.
CI build 1034307 did not retain that exact source's artifact, and an exact
rebuild was blocked by its nested SQLite workspace, missing homepage and stale
lockfile. A regenerated lockfile would not reproduce the historical artifact.
Thus the five-row historical difference remains unattributed; the current
membership and compiler facts above are the basis for this new baseline.

The answer snapshot also reflects byte-array IDs serialized as equivalent hex,
the current cursor representation, and resulting response byte/token budgets.
The prior performance baseline and tolerances are unchanged.
