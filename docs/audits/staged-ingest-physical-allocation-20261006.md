# Physical CAS allocation through staged publication

The fixed staging policy now checks actual Unix allocated blocks for all
immutable object, pack, closure descriptor, and durable closure-index node
files. Selected and abandoned files remain charged until normal store GC
reclaims them. Scans use descriptor-relative no-symlink reads, single-link
regular-file checks, a fixed file-count bound, and constant scan memory.

Stage admission and immutable page writes check this allocation policy, and
the completed staged index is checked before entering the command queue.
Stored membership carries the policy through control rebinds. The durable
publication validates it again under the store process lock after the
pre-HEAD callback and before selecting HEAD. This closes the page-only
accounting gap in the earlier fcc876ec checkpoint. Immutable CAS allocation
is bounded to the existing four 64 MiB admissions (256 MiB); the command queue
remains 4 MiB. Per-intent canonical/raw-source payload remains bounded to
64 MiB. No owned committed CAS reclamation was introduced.

The budget charges immutable CAS and index allocation, not the workspace's
append-only journal or unrelated product metadata. Directory allocation for
the three CAS directories is included. Platforms without a physical block
adapter refuse stored staging rather than substituting logical bytes.

A source test measures actual allocation before and after durable index
composition, checks the exact quota boundary and file-count refusal through
an independently opened cold store, and checks that writing the same immutable
object again does not allocate a second payload. No passing execution is
claimed here; fresh full-fleet admission is still required for the next job.
