# Docs real-source regression inputs

These exact source files were read from the retained Docs source capture at commit
`6e5dee38e927fac86855d17dfb614fbb64b172b3`, tree
`6febbe1d4a7cf4bbc04c0b6997c406e45e718e02`, during actual Docs acceptance of
backend source `50121275599bb0358a0bcb771827d303a23c328c`.

- `api.js`: `src/loadtest/k6/lib/api.js`, 3,455 bytes, Git blob
  `fb2d97deb47f53df0fcbd50a1b92e4d02f8bedb5`. Actual lowering rejected the
  JavaScript rest parameter with `lowering_projection_child_flags_forbidden`.
- `DocGridContentList.tsx`: source path under
  `src/frontend/apps/impress/src/features/docs/docs-grid/components/`,
  9,106 bytes, Git blob `fba889ed159d5a14a35c44076f6793961325684e`.
  Actual compact-fragment preparation rejected duplicate declaration identity.

The fixture tests pin SHA-256 and byte length, run the real in-process TSZ project,
and lower the entire source through normal canonical image admission. They do not
claim that this isolated source test reproduces the original package environment.
Actual package acceptance remains a separate gate. Native execution is pending.
