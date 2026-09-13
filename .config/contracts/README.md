# Versioned control-plane contracts

`schema.json` is the single canonical, versioned description of the bootstrap
control plane. The files under `schemas/` are deliberately boring JSON Schema
projections for tools that cannot read the source. They must carry the same
`$id` and `x-backend-schema-version`; a schema change is a new version.

`backend-control` owns the typed dispatch relation. It admits
versioned relation, leases, exact-base deltas, receipts, and selected durable
head through `backend-version` and `backend-store`. Nu is only the shell
journey adapter: `main cutover control ...` invokes the bounded Rust binary and
returns its JSON response without interpreting lifecycle state.

The Rust executable exposes the complete typed custody path:
`candidate -> evaluation -> review -> decision`. Each command consumes the
exact fence or preceding receipt selected in the versioned relation; review
and decision infer the current receipt when the caller omits its ID. The
three authority receipts have different Rust schema types, and the evaluator,
reviewer, and Sol actor must all differ from the candidate owner and from one
another. Only a Sol-accepted decision changes the row to reusable `completed`.
Receipt constructors are crate-private, so downstream code cannot self-mint
controller, evaluator, reviewer, or Sol evidence.

`main cutover candidate|evaluation|receipt|decide|context ...` remains the Git
promotion and audit adapter. It validates the real Git tree and ancestry and
advances only an approved protected ref with expected-old-value compare-and-swap.
Its prepared and committed events make a crash after `update-ref` reconcilable;
they do not authorize or replace the Rust dispatch lifecycle. Nix remains the
`PolicyRoot` compiler and supplies role-specific tools. The append-only format
is the Git-effect import/export oracle, while the selected versioned relation is
the canonical live agent-work ledger.
