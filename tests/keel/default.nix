# All cases are retained in the inventory. Guidance may change order, never truth.
let
  histories = import ./cases.nix;
  native = import ./native.nix;
in {
  schema = 1;
  cases = histories.cases ++ native.cases;
}
