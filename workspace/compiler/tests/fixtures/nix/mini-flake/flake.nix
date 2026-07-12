{
  description = "A self-contained mini flake with no inputs";

  outputs = { self, ... }: {
    lib = {
      /**
        Double an integer.

        # Type
        ```
        double :: Int -> Int
        ```
      */
      double = x: x * 2;
    };

    packages.x86_64-linux.hello = derivation {
      name = "hello";
      builder = "/bin/sh";
      system = "x86_64-linux";
    };

    nixosModules.default = { config, lib, pkgs, ... }: {
      options = { };
      config = { };
    };
  };
}
