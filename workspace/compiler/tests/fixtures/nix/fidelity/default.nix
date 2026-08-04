# Fixture: fidelity/default.nix
#
# Covers the max-fidelity IR contracts exercised by nix_ir_fidelity.rs:
#  - typed scalar constant (defaultWidth = 80)
#  - nested attrset → Module hierarchy (strings / strings.trim)
#  - args@{ a, ... } ellipsis + args_bind preservation
#  - inherit (from) simple select resolution
{
  /**
    A simple integer constant.
  */
  defaultWidth = 80;

  /**
    String utilities namespace.
  */
  strings = {
    /**
      Strip whitespace.

      # Type
      ```
      trim :: String -> String
      ```
    */
    trim = s: s;

    /**
      Pad on the right.

      # Type
      ```
      pad :: Int -> String -> String
      ```
    */
    pad = width: s: s;
  };

  # inherit (from) simple select → resolve lambda of strings.trim
  utils = {
    inherit (strings) trim;
  };

  /**
    Open record pattern with args@ bind and ellipsis.

    # Type
    ```
    withArgs :: { a :: Int; ... } -> Int
    ```
  */
  withArgs = args@{ a, ... }: a + 1;

  /**
    Ellipsis without args@ bind.
  */
  openOnly = { name, ... }: name;
}
