{
  /**
    Map a function over every value of an attribute set, returning a new
    attribute set with the same keys.

    # Type
    ```
    mapAttrs :: (String -> a -> b) -> AttrSet a -> AttrSet b
    ```

    # Arguments

    f
    : The function applied to each `name` / `value` pair.

    set
    : The attribute set to map over.
  */
  mapAttrs = f: set:
    builtins.listToAttrs (
      map (name: { inherit name; value = f name set.${name}; })
        (builtins.attrNames set)
    );

  /* Concatenate a list of strings, inserting a separator between each
     element.

     Type: concatStringsSep :: String -> [String] -> String

     Example:
       concatStringsSep "-" [ "a" "b" "c" ]
       => "a-b-c"
  */
  concatStringsSep = sep: strings:
    builtins.concatStringsSep sep strings;

  /**
    Build a wrapper derivation around an existing program.

    # Type
    ```
    makeWrapper :: { name :: String; version :: String; ... } -> Derivation
    ```
  */
  makeWrapper = { name, version ? "0.0.0", ... }:
    derivation {
      inherit name;
      builder = "/bin/sh";
      system = "x86_64-linux";
    };

  # The canonical nixpkgs `lib.attrsets` namespace re-exports `mapAttrs`
  # as an inherit alias so both `mapAttrs` and `attrsets.mapAttrs` resolve.
  attrsets = {
    inherit mapAttrs;
  };
}
