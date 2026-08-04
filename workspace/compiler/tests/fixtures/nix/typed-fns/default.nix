# Fixture: typed-fns/default.nix
#
# Exercises the full breadth of the RFC-145 / legacy `::` signature parser:
#
#  - Untyped function (no sig comment at all)
#  - Simple arrows: Int -> String -> Bool
#  - Slice / list parameter: [String]
#  - Record parameter (closed): { name :: String; age :: Int }
#  - Open record (rest `...`): { name :: String; ... }
#  - Type variables: a -> b
#  - Higher-order (curried HOF): (String -> a -> b) -> AttrSet a -> AttrSet b
#  - Nullable (optional) field: String?
#  - Multiple curried args: String -> [String] -> String
{
  /**
    Check whether a number is positive.

    # Type
    ```
    isPositive :: Int -> Bool
    ```
  */
  isPositive = n: n > 0;

  /* No type annotation at all — exercises the untyped path. */
  identity = x: x;

  /**
    Join a list of strings with a separator.

    # Type
    ```
    joinWith :: String -> [String] -> String
    ```
  */
  joinWith = sep: parts:
    builtins.concatStringsSep sep parts;

  /**
    Look up a key in an attribute set, returning null when absent.

    # Type
    ```
    getOr :: String -> AttrSet -> String?
    ```
  */
  getOr = key: set:
    set.${key} or null;

  /**
    Transform every value in an attribute set by applying f.

    # Type
    ```
    transformAttrs :: (String -> a -> b) -> AttrSet a -> AttrSet b
    ```
  */
  transformAttrs = f: attrs:
    builtins.listToAttrs (
      map (k: { name = k; value = f k attrs.${k}; })
        (builtins.attrNames attrs)
    );

  /**
    Build a package from a closed record of metadata.

    # Type
    ```
    mkPackage :: { name :: String; version :: String; src :: Path } -> Derivation
    ```
  */
  mkPackage = { name, version, src }:
    derivation {
      inherit name system;
      inherit src;
      builder  = "/bin/sh";
      system   = "x86_64-linux";
    };

  /**
    Build a package from an open record (unknown extra keys allowed).

    # Type
    ```
    mkPackageOpen :: { name :: String; version :: String; ... } -> Derivation
    ```
  */
  mkPackageOpen = { name, version ? "0.0.0", ... }:
    derivation {
      inherit name;
      builder = "/bin/sh";
      system  = "x86_64-linux";
    };

  /**
    Apply a polymorphic function to a value (generic type variables).

    # Type
    ```
    applyTo :: a -> (a -> b) -> b
    ```
  */
  applyTo = x: f: f x;
}
