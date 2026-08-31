# Exercises the public Nix-built tooling surface from a repository journey.
# Proves role generation and self-lint behavior without compiling product crates.
# Leaves every generated artifact beneath the ignored `.local` boundary.

use std/assert

let doctor = (^backend doctor | complete)
assert equal $doctor.exit_code 0

let roles = (^backend agents verify | complete)
assert equal $roles.exit_code 0

let generation = (^backend agents generate | complete)
assert equal $generation.exit_code 0
