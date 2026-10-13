# Native TypeScript project work

Report 21's installed-dependency cases timed out during the whole-program checker. Source review found two concrete work problems: resolved `skipLibCheck` was lost when constructing checker options, and every worker rebuilt the same global lookup tables.

The diagnostic policy now belongs to resolved compiler options and the admitted AST declaration bit. Skipping declaration semantic diagnostics preserves declaration sources, parse diagnostics, bindings, lazy type queries, and application errors. Both ordinary and partitioned checking obey the policy; augmentation passes still run, and declaration diagnostics added by those passes are filtered at final assembly. Default and explicit false retain checking. The tests include a referenced erroneous declaration and an application assignment that must retain TS2322.

One sealed set of global lookup tables is constructed per borrowed program plan and shared across checker workers and resets. It reuses the existing table builder without applying a separate semantic context, changing scope stamps, or carrying tables across program updates. Prepared controls compare legacy lookup rows, augmentation ownership, sharing, and distinct-program identity.

Boundary observations retain the existing poll and work budget. Detailed records require the subscriber target `compiler.typescript_project=debug`; admission and the first observed stop remain INFO. Events distinguish entry, return, and unwinding and explicitly report truncation after 262,144 records. Enabling tracing can perturb elapsed time and I/O. It does not establish semantic completion or identify a reported package's first source file as the actual offender.

Root and Luna reviewed all three atoms and the final DEBUG change. Root source `1b2b54e5598c559c5f170c9cebc174a4cccb5ff4`, tree `be9861dde79039c438bd46787397a93c92cb66ab`, has the exact binary patch and all 18 postimages of author `4993b2946e7cf65165e5bd6d7c1b03dbc4d3b2df` against its base. Unrelated Python work from that author's base is excluded. Rust syntax parsing and diff checks pass. Receipt SHA-256: `5579c758d3e7eec5d1539a92d785b7b7204a5ff644cf84d698f1999c98e230da`.

Cargo/type checks, native controls, genuine pinned Node/TypeScript comparisons, real-project recovery, and benchmarks remain **unrun**. Source structure supports reduced repeated work; no measured speedup, memory improvement, or Report 21 recovery is claimed.
