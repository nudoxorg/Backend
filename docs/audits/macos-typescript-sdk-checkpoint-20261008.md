# macOS TypeScript SDK packaging checkpoint — 2026-10-08

The macOS package builder can assemble the application with a genuine Node and
complete TypeScript SDK, including its compiler API, declarations, package
metadata, launchers, and license notices. The SDK-only path no longer requires
unrelated language helper receipts. Original SDK receipts remain intact;
relocated and signed payloads receive separately bound receipts.

The signing pipeline now binds selected external package, application-build,
and SDK receipts before signing. It admits finite file inventories, exact
executable and helper modes, and bounded aggregate/package bytes. Bundle roots
must be real directories; special files and unexpected pre-seal signature
sidecars are refused. Native probes run against relocated inputs and must
finish without changing those inputs before refreshed receipts are published.

Signed Mach-O identity uses the selected Apple codesign tool on a private copy.
After signature removal it normalizes only the extent-derived terminal
`__LINKEDIT.vmsize` field for the admitted CPU. Every remaining unsigned byte
is retained in the identity. A valid replacement signature therefore cannot
authorize replacement executable code. Outer application signing and
notarization follow the refreshed inner-image and SDK evidence.

Root read the complete production and test changes. An independent Luna review
found and drove repairs for executable-mode preservation, finite preflight
traversal, and symlinked bundle roots. Root compared every tracked tree entry
against canonical `114332427dfb4aa66eec5277995fbf3209c98f8a`: the ten changed
package paths exactly match the reviewed `f1a8cd7009` source; 17,933 unrelated
entries remain exact, including the newly integrated Rust request authority.

Root ran 84 ordinary Python fixture/source contracts on the composed source
`064c5ea762d358649debdb047d48c9a2e005c8a8`, tree
`872872e9f2aa02dc81071aaf45c9c23c1948f3a5`: all passed. The adjacent JSON names
the exact command, source, and output hashes. These tests validate packaging
and signing contracts with explicit fixtures. They do not prove a native
application build, real signing/notarization, installed macOS behavior, or
successful indexing of a real application.

Remaining work is a coherent current GUI/CLI/MCP/daemon release build, native
SDK probes, signing/notarization where available, and fresh-home installation
with actual TypeScript, Python, Go, and Rust project flows. The separately
built `c0016d4f` Linux package has its own identity and validation ledger; its
installation checks must not be attributed to this macOS checkpoint.
