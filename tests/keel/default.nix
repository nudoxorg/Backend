# Every shared query declares the same rendering detail on both transports.
let
  histories = import ./cases.nix;
  native = import ./native.nix;
  protocol = import ./protocol.nix;
  focus = import ./focus.nix;
  references = import ./references.nix;
  ownerNative = import ./owner-native.nix;
  tsExternal = import ./ts-external.nix;
  jsRetained = import ./javascript-retained.nix;
  cursorLifecycle = import ./cursor-lifecycle.nix;
  required = import ./required-cases.nix;
  assembled = histories.cases ++ native.cases ++ protocol.cases ++ focus.cases ++ references.cases ++ ownerNative.cases ++ tsExternal.cases ++ jsRetained.cases ++ cursorLifecycle.cases;
  cases = map (case: case // {
    steps = map (step: if step.kind == "query" then
      let
        detail = step.arguments.detail or "standard";
        searchBarrier = if step.tool == "backend.search" then {
          wait_for = true;
          expect = step.expect // {
            # Wait for full-search preparation, while preserving honest partial
            # semantic coverage (for example a JavaScript program without full
            # static type evidence). Every remaining field still compares.
            json = step.expect.json ++ [
              { pointer = "/full_search"; absent = true; }
              { pointer = "/search_scope"; absent = true; }
            ];
          };
        } else {};
      in step // {
        cli = step.cli ++ [ "--detail" detail ];
        arguments = step.arguments // { inherit detail; };
      } // searchBarrier
    else step) case.steps;
  }) assembled;
  actual = map (case: case.id) cases;
  sorted = builtins.sort builtins.lessThan;
in
if sorted actual != sorted required then
  throw "Keel release inventory differs from required-cases.nix; missing, duplicated or unreviewed case identities cannot satisfy the release gate"
else {
  schema = 1;
  inherit cases;
}
