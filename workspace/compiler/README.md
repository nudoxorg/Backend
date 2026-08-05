# Compiler — producer plane

Cargo workspace members (Cargo.toml root members list):

| Path                                   | Package                 | Role                                               |
|----------------------------------------|-------------------------|----------------------------------------------------|
| `workspace/compiler/producer/`         | `nudox-producer`        | Shared producer contract (traits, wire types)      |
| `workspace/compiler/languages/go/`     | `nudox-producer-go`     | Go producer                                        |
| `workspace/compiler/languages/csharp/` | `nudox-producer-csharp` | C# producer (Roslyn oracle + XML-doc)              |
| `workspace/compiler/languages/rust/`   | `nudox-producer-rust`   | Rust producer (rust-analyzer oracle)               |
| `workspace/compiler/languages/typescript/` | `nudox-producer-typescript` | TypeScript producer (OXC + optional tsz)        |
| `workspace/compiler/languages/java/`   | `nudox-producer-java`   | Java producer (javadoc extractor oracle)           |
| `workspace/compiler/languages/python/` | `nudox-producer-python` | Python producer (optional pyrefly oracle)          |
| `workspace/compiler/languages/clang/`  | `nudox-producer-clang`  | C/C++ producer (libclang)                          |

The Buck2-only `sandbox/` crate lives at `workspace/compiler/sandbox/` and is not affected
by this layout.

The old Buck compiler package (`compile/`, `generate/`, `graph/`, `render/`,
`treesitter/`, `bin/`, `tests/`, `BUCK`, `lib.rs`, `protocol.rs`, `error.rs`,
`image.nix`, `package.nix`) has been removed. Its maintained producers were
moved out of `crates/` into the `languages/` directory above.
