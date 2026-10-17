# TypeScript class declaration extent alignment

The closed current-5012 Angular H6 courier (index
`68a2b6fe8c3cfc7d9b45489cf9100bed09d8d2e1cbaaf87aaccd00b57ac3c84c`)
contains full class document, CLI show, complete member resolve, and admitted
source captures in `evidence/typescript-angular/nongating-outline-classes`.
This proves two specific same-authored-definition duplicate groups; it does
not classify every repeated outline coordinate.

* `ArticleListComponent.currentPage`, field at line 75: selected IDs
  `0032c2d51f6b0c6948c367b2da521f45c1174848265407ff1dbac1bd1ae569cf` and
  `fe0e90632274537dd27eda155cfff8f9d526650477a0e8aca5b7875ae18df758`.
  Both are direct members of the returned class. The admitted 3,982-byte
  source, SHA-256 `2f2a88381da6e2acae2aac9f4a930218f2962191bac20b9d01cecbcf9d157436`,
  has one `@Input() currentPage = 1;` declaration.
* `IfAuthenticatedDirective.ifAuthenticated`, method at line 35: selected IDs
  `422841698fd0f6c351393af51415a352a22e3f985c5a5b1da6929ac03efdb0cc` and
  `c36cdb7a1f1eecda4787213913a8c08aeba8dc3aef28623254296183047a8e98`.
  Both are direct members of the returned class. The admitted 1,257-byte
  source, SHA-256 `825fe0749a80f8e71f0975aef322e65ac43cb291dc76dcb3d17857bfd1be615c`,
  has one `@Input() set ifAuthenticated(condition: boolean)` declaration.

The mixed projection joins syntax and native declarations only after exact
source identity authentication and singular name/byte-extent agreement.
The TypeScript tree-sitter grammar excludes the class-body semicolon from
`public_field_definition` and owns method decorators as class-body siblings.
OXC native declaration spans include those tokens. The old syntax capture
therefore could not supply the exact-site proof, leaving both rows intact.

The repair aligns only the syntax identity extent through those grammar edges:
class public fields include their following semicolon, and methods include
their preceding decorators. Comments between these grammar siblings are
allowed. No name/location deduplication, ranking change, native authority
change, or containment/excerpt widening is introduced. Both TypeScript syntax
producer identities advance so cached old captures cannot be reused.

The grammar test checks comments, ASI, and method trailing-semicolon exclusion.
The engine test uses actual native TSZ project output to compare syntax captures
with independently lowered declaration spans, retaining same-name methods in
different classes and distinct constructor parameter/field roles. The small
fixture retains the two implicated declaration forms from the captured source.
Existing exact-site tests continue to cover ambiguous/legacy/stale evidence and
overload preservation. Native compilation and test execution are delegated to
the root's managed validation; only source review, rustfmt parsing of changed
functions, and diff whitespace checks were performed for this patch.
