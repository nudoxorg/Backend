Historical ripgrep semantic artifacts

These bytes were retained from the installed c001 product run against ripgrep
commit `3fce3b5bb0236da2df6d99672afb8a719642eca7`, tree
`856ed9162d23416ee7dc5f4389975b54ca062f60`. See `provenance.json` for the exact
image/manifest/source identities, SHA-256 values and capture limits. Upstream
source: https://github.com/BurntSushi/ripgrep/tree/3fce3b5bb0236da2df6d99672afb8a719642eca7.
The pinned COPYING, LICENSE-MIT and UNLICENSE files accompany this fixture.

The real selected manifest contains 107 artifacts for 110 captured sources.
This fixture retains only main.rs and flags/parse.rs plus that manifest and the
three missing source witnesses. Tests use the existing semantic-image and
compilation-manifest codecs to validate the retained bytes and their exact
artifact/source/recipe membership. A test activation with two retained images
and five source witnesses is explicitly a finite fixture, not a reconstruction
or acceptance of the original 107-image generation closure.

The main.rs:45 call is Compiler evidence targeting a foreign Universe(cargo)
`flags::parse` endpoint with VariantUnavailable. It has no exact parse.rs
coordinate. Both source images being available must not manufacture that
reference. Positive availability controls are separate codec-built fixtures.

This is historical actual-artifact evidence. It is neither a current native
compiler result nor a current CLI/MCP end-to-end pass. No original State/HOME
is opened by the tests. Native test execution must be reported separately.
