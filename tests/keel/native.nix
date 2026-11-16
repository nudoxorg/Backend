# Worker-level facts. These cases do not claim SQL name binding or an admitted
# database catalog. Offsets below are half-open byte offsets in authored UTF-8.
let
  read = path: builtins.readFile path;
  eq = pointer: equals: { inherit pointer equals; };
  has = pointer: contains: { inherit pointer contains; };
  selectSet = pointer: select: set_equals: { inherit pointer select set_equals; };
  occurrence = rule_path: selector_ordinal: role: start: end: raw: decoded: {
    inherit rule_path selector_ordinal role start end raw decoded;
    disposition = "Unknown";
  };
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
        (css "observer-root-and-duplicate-sites"
          "﻿/* π😀 */\r\n.foo\\:bar { --x: 1px; --x: 2px; --x\\31: 3px; } @media screen { .nested { --n: 1; } }"
          cssConfig [
            (eq "/result/Ok/utf8_bom_removed" true)
            (eq "/result/Ok/source_coverage" "Partial")
            (selectSet "/result/Ok/occurrences" "" [
              (occurrence [ 0 ] 0 "ClassName" 18 26 "foo\\:bar" "foo:bar")
              (occurrence [ 0 ] null "CustomPropertyDeclarationName" 29 32 "--x" "--x")
              (occurrence [ 0 ] null "CustomPropertyDeclarationName" 39 42 "--x" "--x")
              (occurrence [ 0 ] null "CustomPropertyDeclarationName" 49 55 "--x\\31" "--x1")
            ])
          ])
        (css "class-site-skips-comments"
          "﻿/*😀*/\r\n./**/foo\\:bar { --x: 1; }"
          cssConfig [
            (selectSet "/result/Ok/occurrences" "" [
              (occurrence [ 0 ] 0 "ClassName" 18 26 "foo\\:bar" "foo:bar")
              (occurrence [ 0 ] null "CustomPropertyDeclarationName" 29 32 "--x" "--x")
            ])
          ])
        (css "rejected-rule-does-not-shift-root-ordinals"
          ".bad. { --ignored: 0; } .one { --a: 1; } @media screen { .nested { --n: 2; } } .two { --b: 3; }"
          cssConfig [
            (eq "/result/Ok/source_coverage" "Partial")
            (eq "/result/Ok/rules/0/path" [ 0 ])
            (eq "/result/Ok/rules/1/path" [ 1 ])
            (eq "/result/Ok/rules/2/path" [ 1 0 ])
            (eq "/result/Ok/rules/3/path" [ 2 ])
            (selectSet "/result/Ok/occurrences" "" [
              (occurrence [ 0 ] 0 "ClassName" 25 28 "one" "one")
              (occurrence [ 0 ] null "CustomPropertyDeclarationName" 31 34 "--a" "--a")
              (occurrence [ 2 ] 0 "ClassName" 80 83 "two" "two")
              (occurrence [ 2 ] null "CustomPropertyDeclarationName" 86 89 "--b" "--b")
            ])
          ])
        (css "bom-is-decoded-before-native-location"
          "﻿.foo { color: red; }"
          cssConfig [
            (eq "/result/Ok/utf8_bom_removed" true)
            (eq "/result/Ok/selectors/0/canonical" ".foo")
            (eq "/result/Ok/selectors/0/specificity" 1024)
            (eq "/result/Ok/rules/0/location/column" 1)
          ])
        (css "native-location-is-utf16-column"
          ".first {}\r\n/*😀*/ .second {}"
          cssConfig [
            (eq "/result/Ok/rules/1/location/line" 1)
            (eq "/result/Ok/rules/1/location/column" 8)
            (eq "/result/Ok/rules/1/location/approximate" false)
          ])
        (css "native-media-selectors-declarations-dependencies"
          "@media (width > 60rem) { .hud-menu, .button { --x: 1px; --x: 2px; --chain: var(--x, var(--other)) var(--z, var(--end)); --empty: ; color: var(--chain); } }"
          cssConfig [
            (eq "/result/Ok/coverage" "Partial")
            (eq "/result/Ok/preference_stability" "Unknown")
            (eq "/result/Ok/rules/0/kind" "Media")
            (eq "/result/Ok/rules/1/path" [ 0 0 ])
            (selectSet "/result/Ok/selectors" "/canonical" [ ".hud-menu" ".button" ])
            (selectSet "/result/Ok/selectors/0/components" "/name" [ "hud-menu" ])
            (eq "/result/Ok/selectors/0/component_coverage" "Unknown")
            (selectSet "/result/Ok/declarations" "/property" [ "--x" "--chain" "--empty" "color" ])
            (eq "/result/Ok/declarations/2/value_available" true)
            (eq "/result/Ok/declarations/2/value" "")
            (selectSet "/result/Ok/dependencies" "/name" [ "x" "other" "z" "end" "chain" ])
            (eq "/result/Ok/dependencies/0/reference_path" [ 0 ])
            (eq "/result/Ok/dependencies/1/reference_path" [ 0 0 ])
            (eq "/result/Ok/dependencies/2/reference_path" [ 1 ])
            (eq "/result/Ok/dependencies/3/reference_path" [ 1 0 ])
          ])
        (css "imports-are-retained-with-qualifiers-and-never-loaded"
          "@import \"./base.css\" layer(theme) supports(display: grid) screen;\n@import url(other.css);\n.a { color: red; }"
          (cssConfig // { allow_imports = true; }) [
            (eq "/result/Ok/rules/0/native_kind" "Import")
            (eq "/result/Ok/rules/0/location/line" 0)
            (has "/result/Ok/rules/0/condition" "./base.css")
            (has "/result/Ok/rules/0/condition" "layer(theme)")
            (has "/result/Ok/rules/0/condition" "supports(")
            (has "/result/Ok/rules/0/condition" "screen")
            (eq "/result/Ok/rules/1/native_kind" "Import")
            (eq "/result/Ok/rules/1/location/line" 1)
            (has "/result/Ok/rules/1/condition" "other.css")
            (eq "/result/Ok/rules/2/kind" "Style")
            (eq "/result/Ok/diagnostics" [ ])
          ])
        (css "imports-refuse-when-not-authorized"
          "@import \"./base.css\" layer(theme) supports(display: grid) screen;\n@import url(other.css);\n.a { color: red; }"
          cssConfig [
            (selectSet "/result/Ok/rules" "/native_kind" [ "Style" ])
            (eq "/result/Ok/coverage" "Partial")
            (eq "/result/Ok/diagnostics/0/location/line" 0)
          ])
        (css "late-import-is-rejected-not-loaded"
          ".a {}\n@import \"./late.css\";"
          (cssConfig // { allow_imports = true; }) [
            (selectSet "/result/Ok/rules" "/native_kind" [ "Style" ])
            (eq "/result/Ok/coverage" "Partial")
            (eq "/result/Ok/diagnostics/0/location/line" 1)
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
