# ADR 0001: One versioned engine

Status: accepted.

The backend uses one canonical version crate, one persistent store, one retained flow engine, and one engine composition root. Product views, compiler facts, indexes, replication, work dispatch, and agent evidence identify state through the same domain-separated version machinery.

The Cargo graph mechanically rejects upward or cross-owner core imports. Physical pack, layout, checkpoint, attempt, and Git identities remain separate from logical `StateRoot`, `WorkspaceRoot`, `CommitId`, and `DeltaId` types.

The replaced packages have been removed from the v2 tree. Compatibility tests consume explicit fixtures and external reference checkouts; no legacy package can become a writer, dependency, or in-tree authority again.
