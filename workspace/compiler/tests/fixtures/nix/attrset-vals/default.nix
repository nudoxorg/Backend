# Fixture: attrset-vals/default.nix
#
# Exercises attrset-valued bindings (nested attrs) and inherit aliases.
# These produce Module + Function/Constant entries in the IR, and the
# inherit alias creates a second Function entry with the same formals as
# the canonical binding.
#
# Snapshot targets:
#  - The nested `strings` namespace module is present as a path prefix
#  - `strings.trim` function with typed sig
#  - `strings.pad` function with typed sig
#  - `utils` namespace re-exports `trim` via `inherit`
#  - `builtins.*` synthetic functions from the synthesize() layer
{
  /**
    String utility functions.
  */
  strings = {
    /**
      Strip leading and trailing whitespace.

      # Type
      ```
      trim :: String -> String
      ```
    */
    trim = s: builtins.replaceStrings [" "] [""] s;

    /**
      Pad a string to at least `width` characters on the right.

      # Type
      ```
      pad :: Int -> String -> String
      ```
    */
    pad = width: s:
      if builtins.stringLength s >= width
      then s
      else s + builtins.substring 0 (width - builtins.stringLength s) "                ";
  };

  # Re-export via inherit so utils.trim is an alias of strings.trim.
  utils = {
    inherit (strings) trim;
  };

  /**
    A simple constant (not a function) — exercises the Constant IR path.
  */
  defaultWidth = 80;
}
