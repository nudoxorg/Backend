# Clang corpus review

| project | build system | TUs | decoded facts | type facts | slowest TU | peak RSS | source truth / delta |
|---|---|---:|---|---:|---|---|---|
| stb | none; authored compdb | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/stb/tests/stb.c: Authority(ScratchCapacity { lane: Declarations, capacity: 128, required: 129 }) |
| sqlite-amalgamation | none; authored compdb | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/sqlite-amalgamation/sqlite3.c: Authority(ScratchCapacity { lane: Declarations, capacity: 128, required: 129 }) |
| redis | make | — | — | — | — | — | lane defect: redis: build command needs 157 arguments, capacity is 64 |
| lua | make | — | — | — | — | — | lane defect: lua: compile command has an unrecognized compiler: gcc -Wall -O2  -Wfatal-errors -Wextra -Wshadow -Wundef -Wwrite-strings -Wredundant-decls -Wdisabled-optimization -Wdouble-promotion -Wmissing-declarations -Wconversion  -Wdeclaration-after-statement -Wmissing-prototypes -Wnested-externs -Wstrict-prototypes -Wc++-compat -Wold-style-definition  -Wlogical-op -Wno-aggressive-loop-optimizations  -std=c99 -DLUA_USE_LINUX -fno-stack-protector -fno-common   -c -o lapi.o lapi.c |
| json-c | cmake | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/json-c/json_object.c: Authority(ScratchCapacity { lane: References, capacity: 512, required: 513 }) |
| yaml-cpp | cmake (C++) | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/yaml-cpp/src/emitter.cpp: Authority(ScratchCapacity { lane: References, capacity: 512, required: 513 }) |
| kilo | none; authored compdb | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/kilo/kilo.c: Authority(ScratchCapacity { lane: Declarations, capacity: 128, required: 129 }) |
| zlib | cmake (configure prepared) | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/zlib/deflate.c: Authority(ScratchCapacity { lane: References, capacity: 512, required: 513 }) |
| Vulkan-Headers | cmake | — | — | — | — | — | lane defect: Vulkan-Headers: build path operation failed |
| buck2-with-prelude | buck2 (unsupported query) | — | — | — | — | — | lane defect: buck2-with-prelude: build path operation failed |
| klib | authored compdb | 1 | records=0 enums=0 aliases=0 functions=3; occurrences=86 includes=2 diagnostics=0 | 35 | 18312us (/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/klib/test/khash_keith.c) | 1,062,912,000 bytes (whole test; /usr/bin/time -l) | truth(struct lines)=90 delta=analyzed headers/macros |
| miniaudio | authored compdb | 0 | records=0 enums=0 aliases=0 functions=0; occurrences=0 includes=0 diagnostics=0 | 0 | 0us () | 1,062,912,000 bytes (whole test; /usr/bin/time -l) | truth(struct lines)=168 delta=analyzed headers/macros |
| vurtun-lib | authored compdb | 0 | records=0 enums=0 aliases=0 functions=0; occurrences=0 includes=0 diagnostics=0 | 0 | 0us () | 1,062,912,000 bytes (whole test; /usr/bin/time -l) | truth(struct lines)=147 delta=analyzed headers/macros |
| rxi-map | authored compdb | 1 | records=1 enums=0 aliases=0 functions=12; occurrences=292 includes=3 diagnostics=0 | 115 | 26480us (/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/map.c/src/map.c) | 1,062,912,000 bytes (whole test; /usr/bin/time -l) | truth(struct lines)=1 delta=analyzed headers/macros |
| q3vm | cmake | — | — | — | — | — | lane defect: q3vm: compile command has an unrecognized compiler: gcc -std=c89 -fdata-sections -ffunction-sections -fno-strict-aliasing -fmessage-length=0 -MMD -fno-common -MP -MF"build/main.d" -Wall -Wextra -O2 -I"src/vm" -c -o"build/main.o" "./src/main.c" |
| STC | authored compdb | 0 | records=0 enums=0 aliases=0 functions=0; occurrences=0 includes=0 diagnostics=0 | 0 | 0us () | 1,062,912,000 bytes (whole test; /usr/bin/time -l) | truth(struct lines)=249 delta=analyzed headers/macros |
| pugixml | cmake (priority over meson) | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/pugixml/src/pugixml.cpp: Authority(ScratchCapacity { lane: References, capacity: 512, required: 513 }) |
| cJSON | cmake | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/cJSON/cJSON.c: Authority(ScratchCapacity { lane: References, capacity: 512, required: 513 }) |
| nng | cmake | — | — | — | — | — | lane defect: nng: compilation database JSON was rejected |
| Unity | cmake | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/Unity/src/unity.c: Authority(ScratchCapacity { lane: References, capacity: 512, required: 513 }) |

## Defects and smallest reproductions

No lane fixes are made by this harness. Observed lane defects: stb/tests/stb.c and kilo/kilo.c rejected at Declarations capacity 129>128; sqlite-amalgamation/sqlite3.c rejected at Declarations capacity 129>128; json-c/json_object.c, yaml-cpp/src/emitter.cpp, zlib/deflate.c, pugixml/src/pugixml.cpp, cJSON/cJSON.c, and Unity/src/unity.c rejected at References capacity 513>512; redis generated a 157-argument command over the 64-argument database limit; lua/lapi.c and q3vm/src/main.c used gcc, which the clang-only adapter rejects; Vulkan-Headers produced no compile database; nng produced invalid compile database JSON; buck2-examples produced a build-path terminal before the required unsupported query evidence; miniaudio, vurtun/lib, and STC authored source selectors matched no translation unit. Smallest reproductions are the named files or adapter invocations shown in each row.

## Preparation

Twenty local checkouts were consumed from `.local/corpus/`; no network access was used. zlib's existing CMake marker won detection over its Makefile. Peak RSS is the maximum from `/usr/bin/time -l` around the complete run. Buck2 evidence command: `/Users/mileswirht/.local/bin/buck2 build //cpp/hello_world:main`.
