# Worker-level facts. These cases do not claim SQL name binding or an admitted
# database catalog. Offsets below are half-open byte offsets in authored UTF-8.
let
  read = path: builtins.readFile path;
  eq = pointer: equals: { inherit pointer equals; };
  cssConfig = {
    source_kind = "StandaloneUtf8";
    base_url = "https://example.invalid/main.css";
    author_origin = true;
    no_quirks = true;
    allow_imports = false;
    limits = { input_bytes = 100000; rules = 100; depth = 16; records = 100; diagnostics = 100; text_bytes = 100000; };
  };
  sqlInput = { version = 4; expected_basis = null; settings = "ProcessDefaults"; context = null; };
  css = id: source: config: json: {
    inherit id source config;
    kind = "process"; binary = "backend-stylo-worker"; protocol = "stylo";
    expect = { exit = 0; inherit json; };
  };
  sql = id: source: input: json: {
    inherit id source input;
    kind = "process"; binary = "backend-sqlite-worker"; protocol = "json";
    expect = { exit = 0; inherit json; };
  };
in {
  schema = 1;
  cases = [
    {
      id = "css-source-observer"; language = "css"; node = "frontends.stylo"; daemon = false;
      steps = [
        (css "unicode-escapes-and-duplicates" (read ./fixtures/css/unicode.css) cssConfig [
          (eq "/result/Ok/utf8_bom_removed" true)
          (eq "/result/Ok/source_coverage" "Partial")
          (eq "/result/Ok/occurrences/0/role" "ClassName")
          (eq "/result/Ok/occurrences/0/raw" "foo\\:bar")
          (eq "/result/Ok/occurrences/0/decoded" "foo:bar")
          (eq "/result/Ok/occurrences/0/start" 18)
          (eq "/result/Ok/occurrences/0/end" 26)
          (eq "/result/Ok/occurrences/1/raw" "--x")
          (eq "/result/Ok/occurrences/1/start" 29)
          (eq "/result/Ok/occurrences/2/raw" "--x")
          (eq "/result/Ok/occurrences/2/start" 39)
          (eq "/result/Ok/occurrences/3/raw" "--x\\31")
          (eq "/result/Ok/occurrences/3/decoded" "--x1")
          (eq "/result/Ok/occurrences/3/start" 49)
          (eq "/result/Ok/occurrences/3/end" 55)
        ])
        (css "invalid-value-is-partial" ".bad { color: definitely-not-a-color; } .ok { color: red; }" cssConfig [
          (eq "/result/Ok/coverage" "Partial")
          (eq "/result/Ok/diagnostics/0/location/approximate" true)
        ])
        (css "record-limit" ".x {}" (cssConfig // { limits = cssConfig.limits // { records = 0; }; }) [
          (eq "/result/Err" "Limit")
        ])
        (css "unsupported-quirks" "" (cssConfig // { no_quirks = false; }) [
          (eq "/result/Err" "InvalidConfig")
        ])
      ];
    }
    {
      id = "sqlite-syntactic-outline"; language = "sql"; node = "frontends.sql"; daemon = false;
      steps = [
        (sql "unicode-definitions-and-reads" (read ./fixtures/sql/unicode.sql) sqlInput [
          (eq "/version" 4)
          (eq "/dialect" "Sqlite")
          (eq "/binding" "MissingContext")
          (eq "/syntax/Parsed/statements" 2)
          (eq "/syntax/Parsed/identifier_coverage" "Unavailable")
          (eq "/outline/coverage" "Complete")
          (eq "/outline/statements/0/kind" "CreateTable")
          (eq "/outline/statements/0/definitions/0/kind" "Table")
          (eq "/outline/statements/0/definitions/0/name" "cafÉ")
          (eq "/outline/statements/0/definitions/0/name_extent" { start = 27; end = 32; })
          (eq "/outline/statements/0/definitions/2/kind" "Column")
          (eq "/outline/statements/0/definitions/2/name" "名前")
          (eq "/outline/statements/0/definitions/2/name_extent" { start = 58; end = 66; })
          (eq "/outline/statements/1/kind" "Select")
          (eq "/outline/statements/1/references/0/kind" "Column")
          (eq "/outline/statements/1/references/0/role" "Read")
          (eq "/outline/statements/1/references/0/name" "名前")
          (eq "/outline/statements/1/references/0/extent" { start = 82; end = 90; })
        ])
        (sql "unmodeled-pragma" "PRAGMA table_info(users);" sqlInput [
          (eq "/outline/coverage" "Partial")
          (eq "/outline/statements/0/support/Partial" "UnmodeledConstruct")
          (eq "/binding" "MissingContext")
        ])
        (sql "syntax-error" "SELECT FROM WHERE" sqlInput [
          (eq "/syntax" "NativeError")
          (eq "/binding" "NotAttempted")
          (eq "/outline" null)
        ])
        (sql "wrong-protocol-version" "SELECT 1" (sqlInput // { version = 3; }) [
          (eq "/version" 4)
          (eq "/syntax" "InitializationUnavailable")
          (eq "/binding" "NotAttempted")
          (eq "/outline" null)
        ])
      ];
    }
  ];
}
