# T0.1a local leased terminal

Canonical card for baseline `cf9cc215ba8425a45623562a0ffc455b55c082aa`.

## Capability

`nudox-operation` adds one local source over exactly two caller-owned, uniquely keyed leases. It
preflights the checked byte total before moving either owner, emits data in lower-key order, lends
the original bytes synchronously, then emits one complete or cancelled terminal and fuses. Pending,
wakers, remote completion, failure terminals, retry, reusable admission, source errors, and probes
are later children.

## Scope

Write only `workspace2/crates/nudox-operation/Cargo.toml`, `workspace2/crates/nudox-operation/src/lib.rs`, `workspace2/crates/nudox-operation/src/leased_range.rs`, and
`workspace2/crates/nudox-operation/tests/leased_range.rs` in `/private/tmp/nudox-t0-builder`, whose repository root must be proven
before edit. Do not modify `Cargo.lock` or any other path. No new dependency, unsafe, dynamic
dispatch, box, runtime, or task.

Baseline ledger: `workspace2/crates/nudox-operation/Cargo.toml` 17 lines SHA-256
`3426daf14c27d03f5b12baa744af42e4fc8a14ea7cdb08de72990fe5061e39c5`; `workspace2/crates/nudox-operation/src/lib.rs` 12 lines
`f2c42a8d20ffb48202537d60b9dada3a712b9d1b47d7b8d7e7e10bf06ac587a9`; new source/test absent.

## Literal ABI

```rust
#[repr(transparent)] pub struct RangeKey(pub u64);
pub struct Lease<Buffer> { key: RangeKey, buffer: Buffer }
pub enum LeaseEvent<Buffer> { Data(Lease<Buffer>), Terminal(LeaseTerminal) }
pub enum LeaseTerminal { Complete { items: u8, bytes: u64 }, Cancelled { items: u8, bytes: u64 } }
pub enum StartError<Buffer> {
    Budget { requested: u64, capacity: u64, leases: [Lease<Buffer>; 2] },
    Duplicate { key: RangeKey, leases: [Lease<Buffer>; 2] },
}
pub struct LocalLeaseSource<Buffer> { /* private */ }
impl<Buffer: AsRef<[u8]>> Lease<Buffer> {
    pub fn new(key: RangeKey, buffer: Buffer) -> Self;
    pub fn key(&self) -> RangeKey;
    pub fn with_validated<Output>(&self, validate: impl for<'lease> FnOnce(&'lease [u8]) -> Output) -> Output;
}
impl<Buffer: AsRef<[u8]>> LocalLeaseSource<Buffer> {
    pub fn new(capacity: u64, leases: [Lease<Buffer>; 2]) -> Result<Self, StartError<Buffer>>;
    pub fn poll_next(&mut self) -> Option<LeaseEvent<Buffer>>;
    pub fn cancel(&mut self);
    pub fn credits(&self) -> (u8, u64);
}
```

No type implements `Clone` for a lease. Duplicate keys and over-budget totals reject with both
owners unchanged; two valid safe slice lengths cannot overflow `u64`, so no overflow variant exists.
`credits` names source-held count/bytes only. `cancel` drops all source-held owners and queues
cancelled with the count/bytes already delivered unless complete has already queued; a queued terminal never changes. Dropping the source
drops only source-held leases. Caller-held data is ownership-transferred.

## Evidence and limits

| law | falsifier | cap/stop |
| --- | --- | --- |
| exact preflight | zero/exact/+1, duplicate, full-pair recovery | move before check stops |
| order/borrow | keys 2 then 1 deliver 1 then 2; pointer equality callback | copy/redecode stops |
| terminal/drop | complete and pre/after-first cancel fuse once; source drop preserves held lease | second terminal/owner loss stops |
| ABI | ordinary integration imports all additions and legacy exports | unplanned public item stops |

One production module is forecast 110 formatted lines; production cap 150 with 40 unused reserve.
One integration test is forecast 130; test cap 170 with 40 unused reserve. Run with `RUSTC_WRAPPER`
unset from `/private/tmp/nudox-t0-builder/workspace2`: format check, `cargo test -p nudox-operation --all-targets`, and clippy `-D warnings`.

Next decision: one committed local-terminal implementation; only then T0.1b may add the remote
physical adapter and deterministic wake proof.
