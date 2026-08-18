#!/usr/bin/env nu

# Run the root workspace's hermetic nextest tier from the Nix check.
#
# The flake unpacks the repository source before invoking this script. The
# toolchain and nextest binary are supplied by Nix, and --locked prevents
# dependency resolution drift.

# The Nix check materializes two fixed sources as local paths (pyroscope and
# SmolVM), so their package source identities differ from the checkout lockfile.
# Re-resolve only that temporary build tree, then keep the test invocation
# locked so nextest cannot change dependencies while it runs.
^cargo generate-lockfile
# Keep the hermetic check reproducible on small CI builders and laptop-class
# Darwin hosts.  The default profile's `num-cpus` setting can schedule enough
# concurrent resource-heavy tests to starve the build sandbox before nextest
# can emit its summary; this caps test processes without changing selection or
# assertions.
^env RUSTC_BOOTSTRAP=1 cargo nextest run --locked -P default --workspace --test-threads 4
