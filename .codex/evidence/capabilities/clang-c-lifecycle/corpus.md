# Clang corpus review

| project | build system | TUs | decoded facts | type facts | slowest TU | row wall time | source truth / delta |
|---|---|---:|---|---:|---|---:|---|
| stb | none; authored compdb | — | — | — | — | — | lane defect: stb missing decoded spot stb_regex |
| sqlite-amalgamation | none; authored compdb | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/sqlite-amalgamation/sqlite3.c: Authority(ScratchCapacity { lane: Declarations, capacity: 1024, required: 1025 }) |
| redis | make | — | — | — | — | — | lane defect: No such file or directory (os error 2) |
| lua | make | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/lua/lapi.c: Lowering(NoSupportedDeclaration) |
| json-c | cmake | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/json-c/json_object.c: Lowering(NoSupportedDeclaration) |
| yaml-cpp | cmake (C++) | — | — | — | — | — | lane defect: yaml-cpp missing decoded spot Node |
| kilo | none; authored compdb | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/kilo/kilo.c: Lowering(NoSupportedDeclaration) |
| zlib | cmake (configure prepared) | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/zlib/deflate.c: Authority(ScratchCapacity { lane: References, capacity: 4096, required: 4097 }) |
| Vulkan-Headers | cmake | — | — | — | — | — | lane defect: Vulkan-Headers: build path operation failed |
| buck2-with-prelude | buck2 (unsupported query) | — | — | — | — | — | lane defect: buck2-with-prelude: build tool failed: buck2 |
| klib | none; authored compdb | 2 | records=0 enums=0 aliases=0 functions=4; occurrences=86 includes=decoded diagnostics=unavailable | 37 | 27053us (/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/klib/test/khash_keith.c) | row=38289us | truth(struct lines)=90 delta=main-file decoded |
| miniaudio | none; authored compdb | 1 | records=0 enums=0 aliases=0 functions=1; occurrences=0 includes=decoded diagnostics=unavailable | 2 | 112128us (/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/miniaudio/.nudox-corpus-scratch/miniaudio-driver.c) | row=112128us | truth(struct lines)=168 delta=main-file decoded |
| vurtun-lib | none; authored compdb | 1 | records=0 enums=0 aliases=0 functions=1; occurrences=0 includes=decoded diagnostics=unavailable | 2 | 11266us (/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/lib/.nudox-corpus-scratch/vurtun-lib-driver.c) | row=11266us | truth(struct lines)=147 delta=main-file decoded |
| rxi-map | none; authored compdb | — | — | — | — | — | lane defect: rxi-map missing decoded spot map_new |
| q3vm | cmake | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/q3vm/./src/vm/vm.c: Lowering(NoSupportedDeclaration) |
| STC | none; authored compdb | 1 | records=0 enums=0 aliases=0 functions=1; occurrences=0 includes=decoded diagnostics=unavailable | 2 | 47937us (/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/STC/.nudox-corpus-scratch/STC-driver.c) | row=47937us | truth(struct lines)=249 delta=main-file decoded |
| pugixml | cmake (priority over meson) | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/pugixml/src/pugixml.cpp: Authority(ScratchCapacity { lane: References, capacity: 4096, required: 4097 }) |
| cJSON | cmake | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/cJSON/cJSON.c: Lowering(NoSupportedDeclaration) |
| nng | cmake | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/nng/src/nng.c: Rejected { source_identity: SourceIdentity { identity: ContentId(0e376643caf8ffff3f5e907ececaf8f602d23043a943554dedb4ce703ec09c30), byte_len: 43620 }, recipe: CompileRecipeFact { identity: ContentId(11985c5598514ac5092b48b02be36faeb430c4c0b3b04bc2fe83e65fb980eb35), profile: C(C23), stage: LowerIr, tool: Clang, toolchain: ContentId(0fbcbafe666b209d81dc31600424a0864897c1dbe3beb785f98a6cb35299e2e6) }, rejected: FactRejection { fact: 1024, name_len: 2, cause: Capacity } } |
| Unity | cmake | — | — | — | — | — | lane defect: /Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus/Unity/src/unity.c: Lowering(NoSupportedDeclaration) |

## Defects and smallest reproductions

This run records the following smallest reproductions without changing product code: stb and
yaml-cpp fail decoded spot-entity review; sqlite-amalgamation, zlib, pugixml, and nng terminate
at the reported clang scratch-capacity/rejection facts; lua, json-c, kilo, q3vm, cJSON, and Unity
terminate at `Lowering(NoSupportedDeclaration)`; redis reports an OS build-probe ENOENT; Vulkan-
Headers reports build-path failure; buck2 reports its typed build-tool terminal. rxi-map fails its
named decoded-entity review. The three driver-only rows (klib, miniaudio, vurtun-lib, and STC)
complete both publication generations and old-fragment reopening, but index sealing and decoded
include/diagnostic accessors remain unproved in this harness. These are named evidence defects,
not accepted corpus rows.

## Preparation

Twenty local checkouts were consumed without network access. Measured peak RSS was 253,476,864
bytes, once, using:
`env NUDOX_CORPUS_DIR=/Users/mileswirht/Downloads/backend/.local/worktrees/clang-lifecycle/.local/corpus CARGO_TARGET_DIR=$PWD/.local/target /usr/bin/time -l cargo test -p compiler-driver --offline --test corpus_harness -- --nocapture`
