# Clang C corpus review

The opt-in harness is `compiler/driver/tests/corpus_harness.rs`. It requires shallow,
caller-prepared checkouts under `.local/corpus/` and does not access the network.

| project | build system | TUs | decoded facts | type facts | slowest TU | peak RSS | source truth / delta |
|---|---|---:|---|---:|---|---|---|
| stb | authored compdb | pending | pending | pending | pending | pending | `rg -c '^\s*(typedef\s+)?struct\s+\w+\s*\{' --glob '*.h'`; header expansion expected |
| sqlite-amalgamation | authored compdb | pending | pending | pending | pending | pending | `rg -c '^\s*(typedef\s+)?struct\s+\w+\s*\{' sqlite3.c`; amalgamation expansion expected |
| redis | make | pending | pending | pending | pending | pending | source command recorded in harness |
| lua | make | pending | pending | pending | pending | pending | source command recorded in harness |
| json-c | cmake | pending | pending | pending | pending | pending | source command recorded in harness |
| yaml-cpp | cmake (C++) | pending | pending | pending | pending | pending | source command recorded in harness |
| kilo | authored compdb | pending | pending | pending | pending | pending | source command recorded in harness |
| zlib | make (configure prepared) | pending | pending | pending | pending | pending | configure is fixture preparation |
| Vulkan-Headers | cmake | pending | pending | pending | pending | pending | header-only layout |
| buck2-with-prelude | buck2 | pending | ToolPresentUndrivable pending | — | — | — | separately verify `buck2 build //cpp/hello_world:main` |
| klib | authored compdb | pending | pending | pending | pending | pending | niche single-header |
| miniaudio | authored compdb | pending | pending | pending | pending | pending | niche single-header |
| vurtun-lib | authored compdb | pending | pending | pending | pending | pending | niche single-header |
| rxi-map | authored compdb | pending | pending | pending | pending | pending | niche single-header |
| q3vm | cmake | pending | pending | pending | pending | pending | non-standard layout |
| STC | authored compdb | pending | pending | pending | pending | pending | non-standard header layout |
| pugixml | cmake (priority over meson) | pending | pending | pending | pending | pending | both markers; cmake wins |
| cJSON | cmake | pending | pending | pending | pending | pending | source command recorded in harness |
| nng | cmake | pending | pending | pending | pending | pending | source command recorded in harness |
| Unity | cmake | pending | pending | pending | pending | pending | non-standard src layout |

## Defects and smallest reproductions

No corpus execution has been recorded yet. Lane defects must be added here with repository,
file/line, decoded versus expected facts, and the smallest reproducer; this worker does not
repair lane defects.
