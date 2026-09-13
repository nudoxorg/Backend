# Backend v2 architecture

The v2 workspace is one versioned object and relation engine. The canonical package DAG is enforced by `backend-workspace`. Historical implementations are available only in external reference checkouts and cannot enter the build, runtime, or ownership scopes.

The governing documents are:

- [Versioned engine](versioned-engine.md)
- [Data structures and layout](layout.md)
- [Local and remote execution](local-remote.md)
- [Semantic ledger](../schemas/semantic-ledger.md)
- [Migration sequence](../operations/migration.md)
- [Agent execution and cutover](../operations/agent-cutover.md)

Implementation evidence belongs in machine-readable receipts under the control-plane ledger. These documents define contracts and gates; passing code and independent law suites determine completion.
