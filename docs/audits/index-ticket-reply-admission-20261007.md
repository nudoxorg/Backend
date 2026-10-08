# Exact caller ticket admission

Source base: `aabf9dbcdfae47761dda1d13f5d6d6d2c6b9fd1c`, tree
`ca57e55933a3affbaf16a40ef1e0e3dda8d8bdb0`, unreleased joined protocol 24.

The public body decoder preserves the ticket's package, owner epoch and nonzero
counter. Surface self-admission checks internal page/terminal/cancellation
consistency, but the shared client admission previously compared only command
IDs. A self-consistent foreign ticket could therefore satisfy Await, Progress or
Cancel. Durable operation status also shares the Progress command ID, allowing
an unrelated status family through that check.

The shared Surface admission now checks the exact reply family and full ticket
equality after existing self-admission. This covers pending pages, terminal
outcomes, restart/expired Unknown observations, Requested/Unknown cancellation
and cancellation terminal receipts. Current owner epoch in an honest Unknown
observation may differ from the requested ticket's epoch. Generic Error/Failed
responses remain failures and do not claim a ticket or publication.

No DTO fields, versions, SDK admission, producer publication, GUI state or budgets
change. Complete and partial publication checks remain strict. The controls use
explicitly incomplete view fixtures for structural partial receipts; they do not
construct a runtime coverage authority.

Four controls serialize complete ReplyDto envelopes, use the public strict body
decoder, then call shared admit_reply. Exact tickets pass all six terminal
outcomes (including typed compiler refusal and partial publication), pending,
unknown and cancellation states. Independently valid foreign packages, owner
epochs and counters fail for every ticket-bearing state. A durable Unknown
operation reply with the same Progress command ID fails its family check.
Identity-free failures still pass as failures.

Validation at source freeze: new test module rustfmt; production/test module
Rust syntax parsing; git diff --check. Native execution remains pending Root
source review and a fresh remote capacity admission. No local builds, active
target mutation or source merges were performed.
