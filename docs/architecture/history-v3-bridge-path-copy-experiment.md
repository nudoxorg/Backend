# V3 bridge path-copy experiment

This experiment compares a persistent V3 bridge-index model with the V2 flat
locator model. It is not production selection evidence: the V3 history model is
test-only, and its corrected focused test still needs a rerun before the model
results are treated as green.

The measured fixture has 2,048 bridge rows and applies twelve same-width
replacements. The flat V2 locator reference is 18.9 MB at 65,536 rows, or
590,625 bytes when scaled linearly to the fixture size. The Rust model measured
904,872 bytes of selected metadata writes for clustered changes and 1,526,244
bytes for scattered changes. At this fixture size, V3 is larger than the
same-row flat estimate in both cases; it is a conditional locality win, not a
universal replacement.

The locality difference is visible in the relation and closure work. Clustered
changes wrote 409,224 relation bytes and read 99,400 bytes across four relation
node reads. Scattered changes wrote 591,872 relation bytes and read 972,064
bytes across twenty-five reads. Closure-index writes were 495,447 bytes for
the clustered update and 934,171 bytes for the scattered update. Both cases
materialized 24 relation nodes. Cold reads were similar: 324,512 bridge bytes
across nine nodes, 973,752–977,832 closure-index bytes across 95 nodes, and
196,608 payload bytes across 2,048 objects.

The measured shape points to two costs. The current `LazyTree::prepare_update`
applies ordered keys one at a time through an in-memory overlay, so nearby
changes can repeatedly read or reconstruct the same authenticated ancestor
paths. Scattered edits touch more independent paths, which explains their
larger relation reads. The V3 commit also binds a complete immutable closure;
its closure index must be rewritten and verified as part of each candidate
generation. In this fixture, that closure-index write is comparable to or
larger than the relation write, so improving only tree path copying cannot
guarantee that total metadata falls below the flat locator estimate.

The source experiment now has an explicit bounded replacement API for existing
keys. For equal-width values whose canonical leaf boundaries stay fixed, it
descends shared paths once, merges edits in each affected leaf using the
existing persistent-update merge logic, and rebuilds each affected branch once.
Width changes and leaf-cut changes discard the provisional candidate and use
the established sequential/spill path; inserts, deletes, and missing keys are
rejected by the replacement-only contract. The path applies the existing
`LazyTreeUpdateBudget` preflight and charges candidate nodes, retained
frontier/overlay estimates, and root copies before keeping them.

The 2,048-row FileStore bridge model is wired to this API so its later focused
run can report clustered/scattered relation reads and writes, cold replay, and
post-GC replay. Unit regressions compare exact roots with the sequential oracle,
exercise both fallback triggers, and cold-reopen membership. These Rust checks
have not run yet; the source experiment is not integration evidence and should
not be selected until the hard scattered case and the bridge model pass.
