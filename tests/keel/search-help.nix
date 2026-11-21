# Public process check for the search command's documented page-bound range.
let
in
{
  schema = 1;
  cases = [
    {
      id = "cli-search-help-page-range";
      language = "protocol";
      daemon = false;
      steps = [
        {
          id = "invoke-search-help";
          kind = "process";
          binary = "backend-cli";
          args = [
            "search"
            "--help"
          ];
          timeout_seconds = 15;
          expect = {
            exit = 0;
            stdout_contains = [
              "nudox search"
              "Range: 1 to 200; default 25."
            ];
            stderr_contains = [ ];
          };
        }
      ];
    }
  ];
}
