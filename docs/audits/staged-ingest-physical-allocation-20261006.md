# Physical CAS allocation and pending admission accounting

Commits 0654a73f and 3e1c3807 introduced an optional store allocator that
measures actual Unix allocated blocks for immutable object, pack, closure
descriptor, and closure-index files. The initial production policy attached
a fixed 256 MiB limit to the entire workspace CAS. Root rejected that policy:
selected projects cannot remain charged against a pending ingest budget.
Commit ea70a748 removes this production attachment and the separate fixed
64 MiB per-intent limit. Neither rejected limit is a current production gate.

Production staged limits now derive from the existing validated
SourceAdmissionPolicy project source and record limits. Four affine live
admissions share the resulting pending budget. The command queue remains
4 MiB. The pending catalog charges actual allocated blocks for staged page
and manifest CAS files, including abandoned admissions after their live
reservations drop. An exact current HEAD membership proof transfers selected
IDs out of this catalog. Only accounting markers are removed; normal store
GC retains ownership of CAS reclamation, and per-closure filesystem leases
protect retained reader snapshots.

The pending catalog does not yet charge generated closure-index nodes and
changed control/relation frontier files. This is an explicit remaining gap,
so complete physical pending-storage quota acceptance is not claimed.

The optional PhysicalAllocationBudget allocator remains available for an
explicitly configured total-store policy. It is not attached to production
staged membership. Its descriptor-relative scans reject symlinks and
multiply-linked/nonregular files, include CAS directory blocks, and use
constant scan memory. They exclude the append-only journal and unrelated
product metadata. Platforms without an allocation adapter refuse this policy
rather than substituting logical bytes.

An authored allocator test measures physical allocation before and after
index composition, exact quota boundaries through a cold store, and unchanged
allocation when the same immutable object is written again. Execution results
will be recorded separately with the frozen source and admitted compiler job.
