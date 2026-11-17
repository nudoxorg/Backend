# A fixed, independently authored subset of the release's Keel contracts.
# Keep dependency closure explicit so an omitted native worker cannot pass.
let
  suite = import ../../tests/keel/default.nix;
  ids = [
    "rust-fanout-and-diamond"
    "python-call-history"
    "javascript-file-history"
    "typescript-external-source-policy"
    "css-source-observer"
    "sqlite-syntactic-outline"
    "owner-css-sql-history"
    "rust-mcp-cursor-lifecycle"
  ];
  selected = builtins.filter (case: builtins.elem case.id ids) suite.cases;
in
assert builtins.length selected == builtins.length ids;
{
  schema = 1;
  cases = selected;
}
