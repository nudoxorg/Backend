#!/usr/bin/env bash
set -euo pipefail

no_defaults_tree="$(cargo tree --package backend-journeys --no-default-features --edges normal,build,dev,features)"
default_tree="$(cargo tree --package backend-journeys --edges normal,build,dev,features)"

for package in backend-desktop backend-gui-harness backend-worker; do
    if grep -Fq "$package" <<<"$no_defaults_tree"; then
        printf 'unexpected optional journey dependency in no-default-features tree: %s\n' "$package" >&2
        exit 1
    fi
    if ! grep -Fq "$package" <<<"$default_tree"; then
        printf 'default feature tree omitted expected dependency: %s\n' "$package" >&2
        exit 1
    fi
done

for package in backend-local-service backend-cli backend-mcp; do
    if ! grep -Fq "$package" <<<"$no_defaults_tree"; then
        printf 'no-default-features tree omitted index surface dependency: %s\n' "$package" >&2
        exit 1
    fi
done

printf 'backend-journeys index-only dependency isolation passed\n'
