# Registry Discovery DTO v15

## Purpose: preserve the source's alias coverage state.

## Compatibility: use matching DTO versions.

DTO v15 distinguishes `Known`, `Absent`, and `Unknown` advisory aliases. This is a wire-shape change; clients and services that exchange registry discovery replies must use matching DTO versions.

The view journal rejects a v14 snapshot when opened by a v15 build. A valid v14 journal is left unchanged by that refusal and can still be opened by a matching v14 build. No automatic migration is performed.
