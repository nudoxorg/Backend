# Stored membership reachability leases

The stored membership handle now registers one fixed 32-byte closure-root
lease while its verified affine admission and local shared GC barriers are
still held. Every cloned handle, selected transition, and retained older
workspace snapshot shares the held OS lease. Registration then releases the
global admission barriers owned by that membership handle.

All FileStore collection entry points resolve these leases under the exclusive
global barrier before freezing their root set. A locked lease adds the exact
closure as an ordinary GC root. An unlocked stale record is removed as lease
metadata; normal store collection decides whether to reclaim its immutable
objects. Independently opened store handles use the same descriptor-relative,
no-symlink lease directory. Admission and root scans are bounded to 4,096 lease
records and refuse unsafe paths, non-regular files, multiple links, and corrupt
live records. The current platform implementation is Unix; unsupported
platforms refuse stored membership rather than silently dropping protection.

New store tests retain an older cloned membership through independent-handle
collection, require unrelated interrupted objects to be reclaimed, and require
the retained objects to become reclaimable after the last clone drops. A second
test interrupts streamed membership validation after its first member.

These are authored source tests, not passing execution evidence. The full
local-service check of the prior queue checkpoint fcc876ec stopped on existing
Python frontend type/name errors before reaching local-service. The fleet
collector's configured ILO Python path had been removed; a complete fresh
sample used its documented override to the existing pinned Python 3.14.6
environment before that managed Mac check. No incomplete sample or stale
sample is credited as admission.
