# Flow: producer → IR

**Entry point:** `crates/nudox-producer/src/lib.rs:331` — `produce()`
**Terminates at:** `nudox_ir::apply::PristineIntroTable`
**Crosses:** optionally a subprocess boundary (oracle producers); otherwise
none — this is a pure in-process function call

## Why this flow matters

This is the contract every language frontend implements, and the only place a
language's native symbol vocabulary becomes the shared IR. Get the contract
wrong in a new producer and the failure is not a crash: it is silently wrong
IR that indexes fine and answers queries incorrectly. Get the `ProducerId`
version wrong and the failure is worse — the job key does not change, so cached
IR from the previous output shape keeps being served as if it were current.

**Read the boundary note first.** This flow lives entirely on the local-first
plane (`crates/nudox-*`). The server plane's ingest pipeline does **not** call
`produce()` and does not link any `nudox-producer-*` crate — it execs a guest
binary inside a microVM. See
[Two producer planes](#two-producer-planes) below.

## The contract

`crates/nudox-producer/src/lib.rs:267`, verbatim:

```rust
pub trait Producer {
    /// The producer's native symbol identifier.
    type Id: Eq + Hash + Clone + fmt::Debug;

    /// The deserialized oracle output.
    type Oracle;

    /// Stable versioned identity string. Convention: `"<lang>-<tier>/<version>"`.
    const ID: ProducerId;

    /// The source language this producer lowers.
    const LANGUAGE: Language;

    fn invoke(&self, src: &PackageSource) -> Result<Self::Oracle, ProducerError>;

    fn lower(
        &self,
        oracle: &Self::Oracle,
        out: &mut Lowering<Self::Id>,
    ) -> Result<(), ProducerError>;
}
```

And the driver, `crates/nudox-producer/src/lib.rs:331`:

```rust
pub fn produce<P: Producer>(
    producer: &P,
    src: &PackageSource,
    lineage: &PackageLineageId,
) -> Result<PristineIntroTable, ProducerError>;
```

`Language` here is `nudox_ir::body::Language`, **not** `heart::Language`.
`nudox-ir` carries its own minimal enum deliberately so it does not depend on
`heart` (`lib.rs:285-288`). The two planes do not share the ecosystem
vocabulary.

## The path

1. **`crates/nudox-producer/src/lib.rs:331` — `produce`**
   Calls `producer.invoke(src)` and holds the returned `Oracle` alive on the
   stack for the whole call. → `P::Oracle`

2. **`P::invoke(&PackageSource)`**
   Runs the language's oracle and deserializes its output. Subprocess producers
   use `oracle::run_json` (`crates/nudox-producer/src/oracle.rs:57`), which
   spawns the binary with stdin null, captures stdout and stderr, turns a
   non-zero exit into `ProducerError::OracleExit` carrying the stderr, and
   deserializes stdout as JSON. In-process producers do the equivalent
   analysis directly.

   **`invoke` must not emit into any sink.** The split exists so the caller can
   hold the oracle alive across `lower` without a borrow conflict
   (`lib.rs:298-300`).

3. **`crates/nudox-producer/src/lib.rs:341-355`** — synthesize the root symbol
   and construct the sink. `PackageId::path(src.root())` derives the package
   id from the root path; the root `Symbol` takes its name from
   `PackageSource.name`, is `Visibility::Public`, and leaves span, docs,
   aliases, deprecation, doc-links, attrs, and cfg at their empty defaults.
   → `Lowering<P::Id>`

4. **`P::lower(&oracle, &mut sink)`**
   One pass, no intermediate tree. `Lowering::declare` for symbols,
   `Lowering::refer` for forward references. **The sink is order-independent**
   — parents need not be declared before children (`lib.rs:306-307`), which is
   what lets a producer stream straight out of its oracle's iteration order.

5. **`Lowering::finish()`** (`crates/nudox-ir`, called at
   `crates/nudox-producer/src/lib.rs:358`)
   Validates three structural properties: no reference to an undeclared id, no
   duplicate declaration, and no cycle in the parent pointers. A violation is
   `LoweringError`, which `produce` wraps as
   `ProducerError::LoweringFailed { package, detail }` with the package name
   attached — and which the crate's own docs call **always a bug in the
   producer's `lower`** (`lib.rs:162`). → `IrPackage<P::Id>`

6. **`IrPackage::seal(lineage)`** (`crates/nudox-producer/src/lib.rs:363`)
   Binds the package to its `PackageLineageId`. → `PristineIntroTable`

Nothing on this path touches a sandbox, a daemon, a cage, a worker pool, a
cancel token, or a resource profile. The crate says so explicitly
(`lib.rs:10-14`): execution policy belongs to the execution plane and is not
imported here.

## Who actually implements the trait

**Four of the eight producer crates implement `Producer`.** Verified with
`grep -rn 'impl Producer for' --include=*.rs crates/`:

| Crate | Implements `Producer`? | `ProducerId` | `Id` | `Oracle` | Style |
| --- | --- | --- | --- | --- | --- |
| `nudox-producer-rust` | **yes** (`src/lib.rs:124`) | `"rust-ra/1"` | `RaId` | `LoadedWorkspace` | in-process (rust-analyzer) |
| `nudox-producer-python` | **yes** (`src/producer.rs:36`) | `"python-pyrefly/1"` | `PythonId` | `PythonOracle` | in-process (pyrefly), feature-gated |
| `nudox-producer-typescript` | **yes** (`src/producer.rs:91`) | `"typescript-oxc/1"` | `TsId` | generic `O: TsOracle` | in-process (OXC) |
| `nudox-producer-clang` | **yes** (`src/producer.rs:40`) | `"clang-libclang/1"` | `Usr` | `ClangOracle` | in-process (libclang) |
| `nudox-producer-go` | **no** | `PRODUCER_ID` is a bare `&str` const (`src/producer.rs:44`) | `GoId` | `oracle::Output` | subprocess, ad-hoc entry points |
| `nudox-producer-java` | **no** | — | — | — | ad-hoc |
| `nudox-producer-csharp` | **no** | — | — | — | ad-hoc |

Go, Java, and C# do not even depend on `nudox-producer` — their `Cargo.toml`s
carry no such dependency (verified across all seven manifests). `GoProducer`
exposes `lower_bytes(oracle_json, pkg_id, lineage) -> IrPackage<GoId>`
(`crates/nudox-producer-go/src/producer.rs:56`) and a separate
`invoke_oracle`, and its module doc still describes the trait crate as "being
authored by the prodcore agent in parallel" behind a
`#[cfg(feature = "producer-trait")]` gate that no longer exists in the file
(`crates/nudox-producer-go/src/producer.rs:1-30`). **The Go crate has not been
migrated onto the trait.** Recorded as
[OQ-13](../open-questions.md#oq-13--are-the-go-java-and-c-producers-meant-to-implement-producer).

### A worked implementor: `nudox-producer-python`

The smallest complete one, `crates/nudox-producer-python/src/producer.rs:36`:

```rust
impl Producer for PythonProducer {
    type Id = PythonId;
    type Oracle = PythonOracle;

    const ID: ProducerId = ProducerId("python-pyrefly/1");
    const LANGUAGE: Language = Language::Python;

    fn invoke(&self, src: &PackageSource) -> Result<Self::Oracle, ProducerError> {
        #[cfg(feature = "pyrefly")]
        { crate::context::invoke_oracle(src) }
        #[cfg(not(feature = "pyrefly"))]
        { let _ = src; Ok(PythonOracle::default()) }
    }

    fn lower(&self, oracle: &Self::Oracle, out: &mut Lowering<Self::Id>)
        -> Result<(), ProducerError>
    {
        emit_package(oracle, out);
        Ok(())
    }
}
```

Two things worth copying and one worth noticing:

- `invoke` is where all the I/O and all the language-specific machinery goes.
  Under `--features pyrefly` it discovers `.py`/`.pyi` files, loads them into a
  pyrefly `State`, commits a transaction, walks the result, and extracts owned
  `ModuleData`. The pyrefly `State` is dropped before `invoke` returns — no
  lifetime escapes into `Oracle`.
- `lower` is a thin call into a dedicated module. It emits and returns; it does
  no validation, because `Lowering::finish` already does the three structural
  checks for every producer.
- **Without the `pyrefly` feature, `invoke` returns an empty oracle and
  `produce` succeeds with an empty package.** That is deliberate — offline and
  CI builds cannot build `pyrefly_bundled`, which downloads a typeshed at build
  time (`producer.rs:1-14`) — but it means "the Python producer ran and found
  nothing" and "the Python producer was compiled without its oracle" are
  indistinguishable from the outside.

`nudox-producer-rust` is the in-process counterpart at scale:
`type Oracle = LoadedWorkspace` (`src/lib.rs:126`), a self-referential
rust-analyzer workspace held alive by `produce` for the whole `lower` call
(`crates/nudox-producer-rust/src/ra/loaded.rs:52`).

## The `ProducerId` versioning rule

`crates/nudox-producer/src/lib.rs:107`:

```rust
pub struct ProducerId(pub &'static str);
```

Convention: `"<lang>-<tier>/<version>"`. The `<tier>` names the analysis
technique (`ra`, `pyrefly`, `oxc`, `libclang`, `oracle`, `roslyn`), and the
version is a bare integer suffix.

**Bump the version suffix whenever the output shape changes.**

### What breaks if you don't

The id becomes part of the job key, and the job key gates cached-IR
invalidation (`lib.rs:102-105`). If you change what `lower` emits — a new IR
kind, a renamed field, a different span convention, a changed visibility rule —
without bumping the suffix, the job key for a given package + toolchain +
producer is unchanged. The cache reports a hit. The system serves IR produced
by the *previous* shape of your producer, indefinitely, for every package
already indexed.

There is no runtime check that would catch this. Nothing hashes the producer's
output schema. The failure surface is a query returning plausible, stale,
subtly-wrong results — the hardest kind to notice and the hardest kind to trace
back. The `<version>` suffix is the only mechanism, and bumping it is free.

Bump it for: new or removed IR kinds, changed field semantics, changed span
conventions, changed visibility or deprecation rules, a different oracle
version whose output you lower differently. You do not need to bump it for
pure refactors that provably emit identical IR.

## Memory discipline

The crate has opinions and states them (`lib.rs:16-24`):

- **Borrow from `Oracle`.** `lower` takes `&Self::Oracle` precisely so
  producers can build `Symbol`s and kind bodies from slices, strings, and
  identifiers borrowed out of the deserialized oracle document rather than
  cloning them.
- Prefer `&str` over `String` in intermediate structures.
- Use the IR's `Box<[T]>` (`List<T>`) convention for finished collections whose
  count is known.
- Always `with_capacity` when collecting into a `Vec`.

## Error taxonomy

`ProducerError` (`crates/nudox-producer/src/lib.rs:123`) has five variants, and
every message names the package:

| Variant | When | Fatal? |
| --- | --- | --- |
| `OracleExit { command, code, stderr }` | the oracle subprocess exited non-zero. `code` is a `String` so `"signal: 9"` and `"1"` both fit. | yes |
| `OracleSpawn { command, reason }` | the binary could not be spawned at all | yes |
| `Decode { package, reason }` | oracle stdout did not deserialize | yes |
| `LoweringFailed { package, detail }` | `Lowering::finish` rejected the declarations. **Always a producer bug.** | yes |
| `UnsupportedConstruct { package, symbol, description }` | a language construct the producer cannot lower. **Not inherently fatal** — a producer may omit the symbol and continue; this variant exists for when it chooses to surface it. | producer's choice |

The blanket `From<LoweringError<Id>>` (`lib.rs:228`) cannot know the package
name and fills in `"<unknown>"`. `produce` never uses it — it constructs
`LoweringFailed` directly with the real name (`lib.rs:358-361`). Prefer the
same in any producer that calls `finish` itself.

## Two producer planes

This is the trap. There are two entirely separate notions of "producer" in this
repository and they share no code.

| | Local-first plane | Server plane |
| --- | --- | --- |
| What runs | `nudox_producer::produce(&P, &src, &lineage)`, in-process | a guest binary at `/opt/nudox/<lang>/bin/nudox-<lang>-producer`, inside a SmolvmCage microVM |
| Contract | the `Producer` trait | argv: `--source /mnt/ro0 --emit ndirf1` (`workspace/driver/coordination/indexing.rs:719`) |
| Output | `PristineIntroTable`, a Rust value | NdIrF1 postcard frames on stdout, decoded by `ir_vcs::protocol::StreamReceiver` |
| Callers | `nudox-store`'s `ProducerSource` (`crates/nudox-store/src/source/producer.rs:85`), and tests | `run_producer_in_cage` (`workspace/driver/coordination/indexing.rs:892`) |
| Links `nudox-producer-*`? | yes — `nudox-store` links `nudox-producer-rust` | **no.** No `workspace/*` crate depends on any producer crate. |

The guest binaries are provisioned inside the golden toolchain OCI images,
whose content is a deployment concern
(`workspace/driver/coordination/indexing.rs:685-687`). Whether they are built
from these crates, from the dead Buck2 `workspace/compiler/` tree, or from
something else entirely is not determinable from this repository —
[OQ-14](../open-questions.md#oq-14--what-builds-the-guest-producer-binaries).

Two independent signals that the server plane's table is *not* generated from
these crates:

- `producer_command` has a `Language::Nix` arm naming
  `/opt/nudox/nix/bin/nudox-nix-producer`
  (`workspace/driver/coordination/indexing.rs:775`), and **there is no
  `crates/nudox-producer-nix`**.
- The C/C++ arm calls its producer a tree-sitter static parse
  (`indexing.rs:803`), while `crates/nudox-producer-clang` is a libclang oracle
  (`ProducerId("clang-libclang/1")`).

`crates/nudox-producer/src/lib.rs:5` also claims "Seven language frontends
(Rust, Go, Java, C#, Python, TypeScript, Nix)". That list is wrong: there is no
Nix producer crate, and the seventh is `clang` (C/C++). Verified with
`ls crates/`.

## What is a stub

- `nudox-producer-python`'s `invoke` without the `pyrefly` feature returns
  `PythonOracle::default()` — an empty oracle, and therefore an empty package
  (`crates/nudox-producer-python/src/producer.rs:53-57`). The default build has
  no `pyrefly` feature.
- `nudox-producer-go`, `-java`, `-csharp` implement no trait and are reachable
  only through their own ad-hoc entry points; nothing in the workspace drives
  them through `produce`.
- Nothing else on this path is stubbed. `produce`, `Lowering::finish`, and
  `seal` all do their full work.

## Failure modes

| What fails | What the caller sees | Retried? |
| --- | --- | --- |
| Oracle binary missing | `ProducerError::OracleSpawn` naming the command | no — caller's decision |
| Oracle exits non-zero | `OracleExit` with the captured stderr | no |
| Oracle stdout is not the expected JSON | `Decode { package, reason }` | no |
| `lower` refers to an id it never declared | `LoweringFailed`, detail from `LoweringError` | no — producer bug |
| `lower` declares the same id twice | `LoweringFailed` | no — producer bug |
| `lower` builds a parent-pointer cycle | `LoweringFailed` | no — producer bug |
| Producer meets an unsupported construct | either silently omitted, or `UnsupportedConstruct` | producer's choice |

`produce` swallows nothing. Every error from `invoke`, `lower`, or `finish`
propagates as a typed variant (`lib.rs:327-330`).

## Where to start reading

1. `crates/nudox-producer/src/lib.rs` — 364 lines, the entire contract. Read it
   top to bottom.
2. `crates/nudox-producer-python/src/producer.rs` — the smallest complete
   implementor, ~70 lines of real code.
3. `crates/nudox-producer-rust/src/lib.rs:124` — the same contract under load,
   with a self-referential oracle.
4. `crates/nudox-producer/src/oracle.rs:57` — `run_json`, if you are writing a
   subprocess producer.
