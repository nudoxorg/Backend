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

`migration.spec.ts` was subsequently captured from the same Docs commit during
actual backend `549d1998` acceptance: `src/yhub-server/__tests__/migration.spec.ts`,
21,322 bytes, Git blob `6230dbc4edb2cafbc1842525649f4c7e6532c2df`, SHA-256
`d6ea04f08213179eab0e4b1eb6c208c55c10e7b7f193a8e08be76c2177d7d532`.
The actual profile `[1,0]` preparation rejected it with
`build_duplicate_declaration_identity`; the reported native content identity
`0ea9b985a87ff8fdeaf3c6f22724551a78f5c48a1aa4189580c59deb3fb17ac8`
is distinct from the plain file SHA-256. The entire authored source, including
its anonymous mock classes, is retained without a synthetic dependency stub.
The isolated TSZ test uses no standard libraries and unresolved imports, so
it does not establish acceptance of the original complete Docs environment.

The repair admits unnamed class owners only from TSZ's real class symbol,
single declaration/value declaration, and matching stable file/range. It keeps
that site proof separate from the family: native enclosing names and expression
roles (including the real object property value role) identify the owner route.
Same-route classes share a family and retain distinct native source-order
instance ordinals in member variants. Inserting another indistinguishable
same-route class can change those ambiguous instance ordinals; this does not
claim exact instance lineage through arbitrary ambiguous sibling insertion.
Unrelated same-file prefix/trailing edits preserve the tested class member and
constructor parameter families. No named class row is invented, and no field,
constructor, parameter, or parameter-property role is removed.

The embedded native-authority digest domain and frontend producer/contract are
advanced. The digest already includes the lowering source and feeds the native
toolchain, runtime configuration, host witness and direct-program identity, so
the repaired ownership producer cannot reuse buggy IR under its old identity.
