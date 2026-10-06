# Portable query continuation checkpoint

Base: `039c360d286962a6cf19488fb86caabd1d8c93de` (DTO20).

The client exports a bounded `pc2-` representation of the existing next `CommandDto` and its canonical predecessor certificate. Import uses the existing strict command decoder, then obtains an authenticated current Revision before sending the exact next Search or Name query. Imported claims do not mint a capability. Query family, text, limit, read manifest, selected root, producer scope/context/evidence, branch, log, schema, and cursor frontier remain bound. A no-op sequence advance is allowed; a changed root or authority is refused. Resume performs one Revision RPC and one query RPC, with no prior-page replay.

The issuing scope is compared with the query response even on terminal pages. Export measures the existing DTO serializer against the 32 KiB token budget before allocating the encoded body; overflow is a typed size refusal. CLI JSON emits `nextCursor`, and `--cursor` routes through the shared presentation driver. MCP uses the same inner query proof but retains its existing process/session signer boundary.

At this checkpoint the initial five focused tests passed. The combined suite reached CLI 36/36 and client 95/96. The failing budget test incorrectly assumed that a 200-row page must produce an oversized predecessor proof; it was replaced with a legitimately long query/label fixture and a separate compact full-credit test. Those replacement tests still require execution. This checkpoint is not a final green receipt or actual fresh-process CLI acceptance.

The first combined run also exposed pre-existing test fixtures using arbitrary query projection recipes. The CLI fixture now uses the actual Library producer, and the authenticated remote fixture uses the shared QueryPageRecipe; admission was not weakened.

Further local compilation was paused at the shared disk threshold (15.60 GiB available). `probe-real-portable-cli.py` is prepared for the preserved Mac039 37-coordinate fixture, separate CLI processes per page, cold owner reopen, query and proof tamper negatives, and CLI/persistent-MCP stream parity. It preserves the original project/state and only uses a private state copy. No actual result from that observer is claimed yet.
