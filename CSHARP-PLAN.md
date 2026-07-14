# CSHARP-PLAN — C# Producer, Registry, and Renderer (2026-07-12)

Full-stack plan for a C# (.NET/NuGet) language integration: oracle-pattern IR
producer, NuGet registry ecosystem, occurrence extractor, and C# render
backend. Modeled on the Java producer (`workspace/compiler/compile/java/`),
which is the closest sibling (oracle in the target language, JSON facts,
Rust lowering), with C#-specific deviations called out explicitly.

---

## 0. Architecture decision

### 0.1 Oracle = Roslyn console tool, dual extraction mode

The oracle is a C# console app (`net10.0`) using **Roslyn 5.6.0** that emits
JSON facts, mirroring the Java doclet. It supports two modes behind ONE
output schema:

- **Mode M (metadata)** — input: `{dll, xml?, dependency-ref dlls[]}`.
  Build a `CSharpCompilation` over `MetadataReference`s, walk
  `IAssemblySymbol.GlobalNamespace`. This is the high-fidelity path for
  published NuGet packages (a nupkg ships compiled DLL + XML docs — no build
  required, unlike Maven/crates where we compile from source). Roslyn decodes
  NRT/tuple-names/dynamic/scoped attributes into symbol properties
  automatically when reading metadata.
- **Mode S (source)** — input: source roots. `CSharpSyntaxTree.ParseText`
  each `.cs` (LanguageVersion.CSharp14) into one compilation with framework
  refs only; third-party deps resolve to error-type symbols
  (`TypeKind.Error`, names preserved) — exactly the fidelity Java gets from
  running javadoc without a dependency classpath. Matches the repo's
  `SealedInput` source-root contract and git-tag acquisition.

The Rust host picks the mode: nupkg available → M; sealed source tree → S.
`AuxOutputs.extraction_tier` records `"metadata"` vs `"source"` vs
`"source-degraded"` (error types encountered).

### 0.2 Explicit non-choices

- **No MSBuild / MSBuildWorkspace / Buildalyzer.** Requires full SDK +
  restore + network; hostile to the cage. Compilation-from-metadata and
  parse-from-source need neither.
- **No NativeAOT.** `Microsoft.CodeAnalysis` is not NativeAOT-compatible
  (confirmed; `RequiresDynamicCode` paths). No trimming either — Roslyn is
  not trim-safe. Publish framework-dependent; run as `dotnet oracle.dll`.
- **Framework refs embedded, not restored.** `Basic.Reference.Assemblies`
  (Net90/Net80 variants; 1.8.9) embeds reference assemblies as resources →
  the oracle is fully offline inside the sandbox. net10.0 ref pack via
  `microsoft.netcore.app.ref` mounted RO is the upgrade path (also carries
  BCL .xml docs, needed later for inheritdoc-from-BCL).
- **Daemon mode deferred.** Cold start ~150–400 ms framework-dependent;
  acceptable per-package. NDJSON-over-stdio daemon (matches the worker
  pattern) is a later optimization; `/p:PublishReadyToRun=true` first.

### 0.3 Pinned dependencies (lockstep `=` pins, repo convention)

| Dep | Pin | Why |
|---|---|---|
| `Microsoft.CodeAnalysis.CSharp` | `= 5.6.0` | C# 14 symbol APIs; extension-member APIs churned across 5.0–5.6 previews — never float |
| `Basic.Reference.Assemblies.Net90` | `= 1.8.9` | offline framework refs (no Net100 variant published yet) |
| `ICSharpCode.Decompiler` | `= 10.1.0.x` | optional source-snippet/signature fallback (phase 8) |
| .NET SDK | `dotnetCorePackages.sdk_10_0` (nixpkgs) | LTS to Nov 2028; ships Roslyn 5.x runtime |
| `tree-sitter-c-sharp` | latest compatible with repo's tree-sitter | occurrence extractor (phase 6) |

NOT dependencies: `NuGet.Protocol`/`NuGet.Packaging` (acquisition is plain
HTTP from Rust), `Microsoft.Build.Locator`, `Buildalyzer`.

---

## Phase 1 — Plumbing (enums, toolchain, sandbox, build)

### 1.1 Ecosystem enums

- `workspace/heart/ecosystem.rs`: add `Language::CSharp`,
  `Toolchain::CSharp { sdk: Version }`. Sweep every exhaustive `match` on
  `Language` (compiler dispatch, registry facets, server search — the
  compiler will point at each).
- `workspace/util/sandbox/profiles.rs`: add `ProducerProfile::CSharp`:
  4 GiB RAM (Roslyn compilations of large packages are hungrier than
  javadoc's 2 GiB), 600 s CPU, 10 min wall, 256 pids, 8 MiB stdout (moot —
  JSON goes to `-outfile` like Java), 256 KiB stderr, 2 GiB fsize, 2048
  nofile. `ThreatTier::Untrusted` (source mode runs no user code — Roslyn
  only parses — but analyzers/source-generators in packages are a code-exec
  vector if we ever load them; treat like Java/Go).
- `workspace/compiler/compile/producer/mod.rs`: `Language::CSharp =>
  ProducerProfile::CSharp` in `Producer::profile()`.

### 1.2 Nix + Buck toolchain

- `flake.nix`: add `dotnetCorePackages.sdk_10_0` to devshell packages. In
  the NIX-GENERATED `.buckconfig` fragment (same hook that writes
  `java.java_home`), emit:
  - `csharp.dotnet = ${sdk}/bin/dotnet`
  - `csharp.nuget_packages = <nix-materialized offline feed>` (see 1.3)
- Devshell env: `DOTNET_CLI_TELEMETRY_OPTOUT=1`, `DOTNET_NOLOGO=1`,
  `DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1`, `DOTNET_CLI_HOME=$TMPDIR/dotnet`
  (dotnet needs writable HOME — same class of fix as the GOCACHE
  sandbox-PATH issue from the snapshot-test drive).
- `build/toolchains/BUCK`: no dotnet rule exists in the prelude. Follow the
  Go zero-config spirit but via config: read `csharp.dotnet` with
  `read_root_config`, expose as a `command_alias`/`sh_binary` toolchain
  target `//build/toolchains:dotnet`.

### 1.3 Oracle build (the one genuinely new build problem)

Java's oracle is a `java_library` — the prelude knows Java. Buck2 has no
dotnet rule, and `dotnet publish` wants a NuGet restore. Plan:

1. Oracle project pins deps with `packages.lock.json`
   (`RestorePackagesWithLockFile=true` in the csproj).
2. Nix materializes the locked NuGet deps into an offline folder feed
   (`buildDotnetModule`-style `nugetDeps` fetch, or a simple
   fixed-output derivation over the lock file), surfaced to Buck as
   `csharp.nuget_packages`.
3. `workspace/compiler/compile/csharp/oracle/BUCK`: a `genrule` running
   `dotnet publish -c Release --no-self-contained /p:PublishReadyToRun=true
   --locked-mode` with `NUGET_PACKAGES=$(csharp.nuget_packages)` and
   `HOME`/`DOTNET_CLI_HOME` pointed into the genrule scratch dir. Output:
   the publish directory (oracle.dll + deps + runtimeconfig).
4. `workspace/compiler/BUCK` resources dict (lib AND test targets):
   `"csharp-oracle": "//workspace/compiler/compile/csharp/oracle:oracle"`.
   `buck_resource("csharp-oracle")` then resolves the publish dir at
   runtime, exactly like `"java-oracle.jar"`.

Fallback if the genrule fights Buck: build the publish dir entirely in Nix
and alias it into Buck with `export_file`; keep the resource key identical
so the Rust side never knows.

---

## Phase 2 — Oracle (C#, `workspace/compiler/compile/csharp/oracle/`)

Files: `Program.cs` (arg parsing, mode dispatch), `Surface.cs`
(SymbolVisitor walk + filters), `TypeSig.cs` (type encoder), `Docs.cs`
(XML doc harvest + inheritdoc), `Json.cs` (port the Java `Json.java`
hand-rolled writer if we want zero deps, or just use `System.Text.Json` —
it's in the BCL, no package needed; **use System.Text.Json**, the Java
constraint doesn't apply), `oracle.csproj`, `packages.lock.json`, `BUCK`.

CLI:
```
oracle --mode metadata --dll <path> [--xml <path>] [--ref <dll>]... --out <json>
oracle --mode source   --root <dir>... [--ref <dll>]... --out <json>
```

### 2.1 Compilation setup

Mode M:
```csharp
var doc = xmlPath is null ? null : XmlDocumentationProvider.CreateFromFile(xmlPath);
var target = MetadataReference.CreateFromFile(dllPath, documentation: doc); // docs are OPT-IN — never auto-discovered
var comp = CSharpCompilation.Create("extract",
    references: Net90.References.All.Concat(depRefs).Append(target),
    options: new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary,
        nullableContextOptions: NullableContextOptions.Enable));
var asm = (IAssemblySymbol)comp.GetAssemblyOrModuleSymbol(target)!;
```
Mode S: same but `syntaxTrees:` from parsed sources and walk
`comp.Assembly.GlobalNamespace`; collect `comp.GetDiagnostics()` error
counts into the output header for tier reporting.

### 2.2 Surface walk + filter rules

`SymbolVisitor` from `GlobalNamespace`. Include a member iff:

1. Effective accessibility (walk `ContainingType` chain) ∈
   `{Public, Protected, ProtectedOrInternal}` — plus explicit interface
   implementations (`DeclaredAccessibility == Private` but
   `ExplicitInterfaceImplementations` non-empty; they ARE surface). Emit
   ALL visibilities under a `--private` flag like the doclet, and let the
   Rust side filter (Java precedent: doclet runs `-private`).
2. Not `IsImplicitlyDeclared`, and `CanBeReferencedByName` (except ctors,
   operators, indexers, explicit impls — those legitimately fail the name
   test; gate by `MethodKind`/`IsIndexer` first).
3. Record-synthesized filter by name heuristic when
   `INamedTypeSymbol.IsRecord`: drop `EqualityContract`, `PrintMembers`,
   `<Clone>$`, compiler `Equals`/`GetHashCode`/`ToString`/`Deconstruct`/
   `op_Equality`/`op_Inequality`/copy-ctor **unless the member carries its
   own XML doc** (user-overridden). Note: `IsRecord` from *metadata* is
   heuristic-based and must be verified against a compiled record-struct
   with 5.6.0 (pitfall register #12); fall back to has-`EqualityContract`.
4. Skip `IsAnonymousType`, `<PrivateImplementationDetails>`,
   `<>c__DisplayClass*`, file-local types
   (`CompilerFeatureRequired("FileTypes")`).
5. `EditorBrowsable(Never)` and `[Obsolete(error: true)]`: EMIT with flags
   (`hidden: true`, `deprecated: "error"`) — policy decided Rust-side.
6. Enum members: `IFieldSymbol.IsConst` with `ConstantValue`; skip
   `value__`.
7. Assembly-level: emit `Identity`, `TargetFrameworkAttribute`,
   `GetForwardedTypes()` (forwarded types are surface — mark
   `forwarded: true`; classic facade packages), IVT list as metadata only.

### 2.3 Per-symbol facts (APIs to use)

- Doc-ID key for every symbol: `ISymbol.GetDocumentationCommentId()` —
  never hand-construct (explicit-impl `#` mangling, nested arity, `~` on
  conversions).
- Modifiers: `IsStatic/IsAbstract/IsSealed/IsVirtual/IsOverride/IsExtern/
  IsReadOnly` + `ITypeSymbol.IsRefLikeType`, `IsReadOnly`,
  `INamedTypeSymbol.IsRecord`, `EnumUnderlyingType`,
  `DelegateInvokeMethod`. `new`-hiding: no flag — detect base member with
  same signature where `IsOverride == false`. `unsafe`: no flag — infer
  from pointer/function-pointer occurrence in the signature.
- Methods: full `MethodKind` dispatch (Ordinary, Constructor,
  StaticConstructor, Destructor, UserDefinedOperator, Conversion,
  ExplicitInterfaceImplementation, PropertyGet/Set, EventAdd/Remove,
  ReducedExtension, DelegateInvoke). Operator names incl. C# 11 checked
  (`op_CheckedAddition`…) and C# 14 compound assignment
  (`op_AdditionAssignment`…). Async/iterator hints from
  `AsyncStateMachineAttribute`/`IteratorStateMachineAttribute`
  (`IsAsync` is unreliable from metadata).
- Extension methods (classic): `IsExtensionMethod`, receiver = param 0.
  C# 14 extension blocks: nested types with `INamedTypeSymbol.IsExtension`,
  receiver via `ExtensionParameter`, doc-XML pairing via
  `IMethodSymbol.AssociatedExtensionImplementation` (compiler emits doc IDs
  against the lowered static impls). Compile-time-verify these exact
  members against 5.6.0 (API churn, pitfall #16).
- Properties: `GetMethod`/`SetMethod` each with own accessibility
  (asymmetric accessors), `SetMethod.IsInitOnly`, `IsRequired`,
  `IsIndexer` + `Parameters`, `ReturnsByRef(Readonly)`,
  `IsPartialDefinition`.
- Events: `IEventSymbol.Type` (delegate type), Add/Remove accessibility.
- Fields: `IsConst` + `ConstantValue` (decimal via
  `DecimalConstantAttribute` — verify Roslyn decodes), `IsReadOnly`,
  `IsVolatile`, `IsRequired`, `IsFixedSizeBuffer` + `FixedSize`; skip
  `AssociatedSymbol != null` (backing fields).
- Parameters: `RefKind` all FOUR (`Ref`, `Out`, `In`,
  `RefReadOnlyParameter`), `IsParams` + array-vs-collection
  (`IsParamsArray`/`IsParamsCollection`), `HasExplicitDefaultValue`/
  `ExplicitDefaultValue` (null + has=true → render `default`; map enum
  defaults back to member names), `ScopedKind`, caller-info + NRT-flow
  attributes.
- Generics: `ITypeParameterSymbol` — `Variance`, `HasReferenceTypeConstraint`
  (+ `ReferenceTypeConstraintNullableAnnotation` for `class?`),
  `HasValueTypeConstraint`, `HasNotNullConstraint`,
  `HasUnmanagedTypeConstraint`, `HasConstructorConstraint`,
  `ConstraintTypes` + `ConstraintNullableAnnotations`,
  `AllowsRefLikeType` (C# 13).
- Attributes: `GetAttributes()` + `GetReturnTypeAttributes()`. Suppress the
  feature-encoding set Roslyn already decoded (Nullable*, TupleElementNames,
  Dynamic, IsReadOnly, IsByRefLike, IsUnmanaged, Extension, ParamArray/
  ParamCollection, ScopedRef, RequiredMember, CompilerFeatureRequired,
  Async/IteratorStateMachine, CompilerGenerated, NativeInteger,
  DecimalConstant, DefaultMember). Surface the doc-meaningful set:
  Obsolete (Message/IsError/DiagnosticId/UrlFormat), Experimental,
  EditorBrowsable, AttributeUsage, Flags, CallerX, NotNullWhen/
  MaybeNullWhen/NotNullIfNotNull/MemberNotNull/DoesNotReturn/AllowNull/
  DisallowNull, UnmanagedCallersOnly, OverloadResolutionPriority,
  SetsRequiredMembers, Supported/UnsupportedOSPlatform,
  RequiresUnreferencedCode/RequiresDynamicCode. Match attribute classes via
  `SymbolEqualityComparer` against `GetTypeByMetadataName` handles — not
  string compares (and per-assembly lookup to dodge ambiguity, pitfall #14).

### 2.4 TypeSig encoding (JSON, parallel to Java's `TypeMirror`)

Recursive, `"kind"`-tagged, depth-guarded at 64:

```
named       {name, args[], owner?, nullable: "none"|"annotated"|"notAnnotated", typeKind}
typeParam   {name, ownerKind: "type"|"method", nullable}
array       {element, rank, nullable}           // rank 1 = SZ; jagged = nested
pointer     {pointee}
funcPtr     {params[], return, callConv, unmanagedCallConvs[]}
tuple       {elements: [{name?, type}], nullable}   // names from TupleElements
dynamic     {}
nullableValue {inner}                            // Nullable<T>
error       {name}
```

`nullable` carried on EVERY reference-type node (3-state; oblivious =
`"none"` — never collapse, pitfall #10). `nint`/`nuint`:
`named{name:"nint"}` when `IsNativeIntegerType`. Also emit, per symbol, a
`display` string from `ToDisplayString` with a custom `SymbolDisplayFormat`
(IncludeParameters|IncludeType|IncludeRef|IncludeModifiers|
IncludeExplicitInterface|IncludeConstantValue; generics with constraints +
variance; `IncludeNullableReferenceTypeModifier`) — the renderer gets a
free authoritative signature to diff against.

### 2.5 Docs

- Per symbol: raw XML from `GetDocumentationCommentXml()` (mode M requires
  the `documentation:` provider — pitfall #13). ALSO parse the package
  `.xml` file directly keyed by doc-ID and prefer it (preserves raw markup;
  dodges Roslyn normalization).
- `inheritdoc` resolution happens ORACLE-SIDE (Roslyn's
  `expandInheritdoc` is unreliable for metadata symbols): chain =
  `OverriddenMethod`/`OverriddenEvent`/overridden property → base-class
  walk → `ExplicitInterfaceImplementations` →
  `FindImplementationForInterfaceMember` reverse lookup → interfaces in
  declaration order; honor `cref`/`path` attributes; cycle-detect. Emit
  both `doc` (resolved) and `docInherited: true` marker.
- crefs inside docs: resolve with
  `DocumentationCommentId.GetFirstSymbolForDeclarationId(id, comp)` and
  emit a `docLinks: {cref → docId}` map so the Rust side can build
  `doc_links: HashMap<String, NudoxPath>` without reimplementing ID
  parsing.

### 2.6 Output envelope

```json
{ "format": 1, "dotnetVersion": "10.0", "roslyn": "5.6.0",
  "mode": "metadata|source", "assembly": {name, version, tfm, forwardedTypes[], ivt[]},
  "diagnostics": {errorTypeCount, errorCount},
  "namespaces": [{name, doc?}],
  "types": [ ...flat, nesting by enclosing key, like Java's TypeDecl... ] }
```

Types carry: `docId, qualifiedName (metadata name, arity backticks kept),
simpleName, kind (CLASS|STRUCT|INTERFACE|ENUM|DELEGATE|RECORD|RECORD_STRUCT),
namespace, enclosing?, modifiers[], typeParams[], baseType?, interfaces[],
enumUnderlying?, delegateSig?, attributes[], deprecated?, hidden?,
forwarded?, doc?, docLinks?, extensionReceiver? (C#14 blocks), members
{fields[], properties[], events[], constructors[], methods[], operators[],
conversions[], indexers[], nested[]}`.

---

## Phase 3 — Rust host (`workspace/compiler/compile/csharp/`)

File-for-file mirror of the Java module:

```
mod.rs        — re-exports
error.rs      — CSharpError → {PackageError, OracleError{Spawn, JsonParse, Extraction}, VersionError}
package.rs    — project discovery + lowering entrypoint
oracle.rs     — IsolatedCommand construction + invocation
schema.rs     — serde mirrors of §2.6 (all camelCase; TypeSig as #[serde(tag="kind")] enum)
context.rs    — Lowering state, NudoxPath scheme, lower_extraction
types.rs      — TypeSig → ir::ty::Type
item.rs       — type decl → Entry
function.rs   — methods/ctors/operators → ir::Function
xmldoc.rs     — XML doc tags → markdown documentation string
producer.rs   — CSharpProducer
nupkg.rs      — NuGet flat-container fetch + zip + TFM selection (host-side, pre-seal)
```

### 3.1 `producer.rs`

`CSharpProducer` with `ID = ProducerId("roslyn-oracle/1")`,
`language() = Language::CSharp`, `tier() = ThreatTier::Untrusted`. Like
Java, override `produce` → `lower_in_process` (adaptive: probe for nupkg
artifacts vs source), OR keep the Go-style `plan`/`decode` split for mode M
(single command, clean). **Decision: Go-style `plan`/`decode` for mode M,
Java-style adaptive only if source mode needs multi-step.** Start with
`plan` building one `IsolatedCommand`:

```rust
IsolatedCommand::new("dotnet", ProducerProfile::CSharp)
  .args([oracle_dir.join("oracle.dll"), "--mode", mode, ..., "--out", scratch.join("facts.json")])
  .env("DOTNET_CLI_HOME", scratch).env("DOTNET_NOLOGO", "1").env("DOTNET_CLI_TELEMETRY_OPTOUT", "1")
  .read_only([oracle_dir, dll_dir | source_roots, ref_dirs])
  .writable([scratch])
```

Oracle publish dir via `producer::buck_resource("csharp-oracle")`. JSON via
`-outfile`-equivalent (never stdout — Roslyn JSON for big packages beats
the 8 MiB cap). Validate: `format == 1`, non-empty types, record
`diagnostics.errorTypeCount` into `extraction_tier`.

### 3.2 `package.rs` — discovery

`discover_project(start)`: walk upward for `*.sln`/`*.csproj`/
`Directory.Build.props` (Layout::Sdk), else Plain. Parse csproj with the
same hand-rolled minimal XML scanner style as `parse_pom` (capture
`PackageId`, `Version`/`VersionPrefix`, `TargetFramework(s)`,
`RootNamespace`, `GenerateDocumentationFile`). Source roots: csproj dir
(SDK-style globs all `.cs` recursively; skip `bin/`, `obj/`,
`artifacts/`). Version resolution from git tags via shared
`version::resolve_from_tags` with prefixes `v`, `V`, bare, and
`<PackageId>-` — NuGet versions are SemVer2-with-4-part-legacy; reuse the
`MavenVersion`-style custom parser shape, not `semver::Version` (it rejects
`1.0.0.5`). New `NuGetVersion` in `traversal.rs`-equivalent: 4 numeric
parts + case-insensitive prerelease comparison + build-metadata dropped.

### 3.3 `nupkg.rs` — NuGet acquisition (host-side, NOT in cage)

Plain HTTP, no NuGet client libs:

- Service index `GET https://api.nuget.org/v3/index.json` → resolve
  `PackageBaseAddress/3.0.0` (flat container) and
  `RegistrationsBaseUrl/3.6.0`.
- IDs lowercase, versions NuGet-normalized (strip leading zeros, drop
  `.0` 4th part, drop `+build`).
- Versions: `{base}{id}/index.json` (includes unlisted — cross-check
  registration `listed` flag).
- nupkg: `{base}{id}/{version}/{id}.{version}.nupkg` (zip). Extract:
  prefer `ref/{tfm}/` over `lib/{tfm}/`; TFM precedence
  `net10.0 > net9.0 > net8.0 > … > net5.0 > netcoreapp3.1 > netstandard2.1
  > netstandard2.0 > net48…net45`; ignore platform-suffixed
  (`net8.0-windows`) unless sole option; grab the sibling `.xml` doc file
  (may be absent or culture-nested).
- Dependencies (for mode M refs): registration
  `{reg}{id}/index.json` → `catalogEntry.dependencyGroups` matching chosen
  TFM → resolve each range to its MINIMUM satisfying version (NuGet rule),
  fetch ref DLLs one level deep. Deeper misses are tolerable — Roslyn
  degrades to error-type symbols with names intact; record count as a
  fidelity diagnostic, don't fail (pitfall: `CSharpCompilation` never
  errors on missing refs).
- Everything content-addressed into the CAS (`workspace/cas`) keyed by
  nupkg sha512 from the flat container; the sealed job then mounts CAS
  paths RO. `dep_lock_hash` = hash of the resolved (id, version, sha)
  set — slots directly into `seal_package`'s job key.

### 3.4 NudoxPath scheme (`context.rs`)

Follow Java precisely:

- `Local("<namespace>")` — namespace → `Entry::Module`
- `Local("(global)")` — global namespace (Java: `(default)`)
- `Local("<namespace>::<Type>")` — type; nested =
  `Ns::Outer.Inner`; KEEP metadata arity backticks (`Ns::List`1`) —
  C# types overload by arity (`List` vs `List<T>` are distinct), Java
  doesn't have this problem.
- `Local("<namespace>::<Type>.<member>")` — method group / property /
  event / field.
- Operators/conversions: `…::<Type>.op_Addition` etc. (metadata names —
  unique, doc-ID-aligned).
- Store the oracle's `docId` in `Symbol.aliases` so cref `doc_links` and
  the future occurrences work can join on it.

Resolver map (simple → FQN) seeded with `System.*` common types, mirroring
the `java.lang.*` seeding.

### 3.5 `types.rs` — TypeSig → `ir::ty::Type`

| TypeSig | IR (`workspace/ir/ty.rs`) |
|---|---|
| named bool | `Primitive(Bool)` |
| named sbyte/short/int/long | `Primitive(Int(W8/W16/W32/W64))` |
| named byte/ushort/uint/ulong | `Primitive(UInt(W8/W16/W32/W64))` |
| named nint / nuint | `Primitive(Int(Arch))` / `Primitive(UInt(Arch))` |
| named float/double | `Primitive(Float(W32/W64))` |
| named char | `Primitive(Char)` |
| named string (System.String) | `Primitive(String)` |
| named decimal | **`Primitive(Decimal)` (new variant — see 3.8)**; fallback `TypeReference("System.Decimal")` |
| named void (return) | `Tuple(vec![])` |
| named System.Object | `Any` |
| dynamic | `Any` (doc note "dynamic") |
| named with owner | `QualifiedPath { name, self_type }` |
| named generic | `TypeReference { identifier, generic_args }` |
| typeParam | `GenericParam { name }` |
| array rank 1 | `Slice(element)` |
| array rank n>1 | `TypeOperator { operator: "[,]"×(n-1) commas, type: element }` |
| pointer | `RawPointer { is_mutable: true, type }` |
| funcPtr | `FunctionPointer` (calling conv → doc/attr) |
| tuple, all unnamed | `Tuple(Vec<Type>)` |
| tuple, any named | `NamedTuple(Vec<TupleMember>)` — exact existing fit |
| nullableValue | `TypeReference { identifier: "System.Nullable", generic_args: [T] }` |
| nullable == annotated (ref type) | `TypeOperator { operator: "?", type }` |
| nullable == notAnnotated | bare type |
| nullable == none (oblivious) | bare type + item-level fidelity note (Track A; see 3.8) |
| error | `TypeReference { identifier: name }` best-effort |

### 3.6 `item.rs` — declarations → `Entry`

- **class / struct / record / record struct** → `Entry::RecordType`
  (`Symbol<Record>`): `super_types` = baseType + interfaces;
  `implemented_protocols` = interface NudoxPaths; fields → `Field::Known`;
  properties → `Field::Known` with accessor encoding (3.8); constructors →
  `Record.constructors`; methods → standalone `Entry::Function` under
  member paths (Java pattern) + `Record.members` wiring. Struct-ness /
  readonly / ref / static-class / sealed / abstract → Track A: "Declared:"
  doc section (Java precedent for non-access modifiers); required members →
  `FieldAttributes.decorators += "required"`.
- **record positional params** → they ARE public init properties: emit as
  fields with `init` decorator + `Deconstruct` kept.
- **interface** → `Entry::TraitDef`: abstract members →
  `required_methods`; DIMs (`!IsAbstract` on interface member) →
  `provided_methods` (`has_default_implementation: true`); static
  abstract/virtual (generic math) → required/provided with
  `ReceiverKind::Static`; properties → `TraitDef.properties`; variance +
  constraints → `Generics`; base interfaces → `super_traits`.
- **enum** → `Entry::SumType`: variants with constant values as
  `SumVariant` (value in `ConstExpr`/documentation); `[Flags]` →
  documentation section + alias; underlying type recorded.
- **delegate** → `Entry::TypeAlias(Type::FunctionPointer)` — exact
  existing fit; doc + attrs on the Symbol.
- **static class of classic extension methods** → RecordType whose methods
  carry receiver = first param type (3.7); **C# 14 extension blocks** →
  flatten block members to the enclosing static class with
  `extensionReceiver` recorded; doc join via
  `AssociatedExtensionImplementation` docId done oracle-side.
- **explicit interface impls** → member name keeps the
  `IFace.Member` dotted form; `Visibility::Public` semantics note
  ("callable via interface only") in docs.
- **forwarded types** → `Entry::Info` stub pointing at the target, or
  full emit with `forwarded` doc marker (decide in review; start with
  full emit + marker).
- Visibility mapping: public→`Public`, protected→`Protected`,
  internal→`Internal`, protected internal→`Protected` (+doc note),
  private protected→`Package` (+doc note), private→`Private` —
  `Visibility::Internal` already exists (`workspace/ir/kind.rs:15`
  even cites C#).

### 3.7 `function.rs`

Java conventions reused verbatim:

- `fold_overloads`: overload group → primary = first, rest in
  `Function.overloads` (C# is overload-heavy — this is load-bearing).
- Parameters → `LiteralParameter { name, type, default_value, description
  from <param> }`. `params` → `ParameterAttribute::Variadic` +
  `Attribute::Variadic`. Modifiers: `ref` → `Inout`; `out` → `Inout` +
  decorator `"out"` (Track A) / `ParameterAttribute::Out` (Track B);
  `in`/`ref readonly` → `Borrowing`; optional → `Optional` +
  `default_value`; `scoped` → decorator.
- Receiver: static → `ReceiverKind::Static`; instance →
  `SharedRef` (readonly struct members) / `MutRef`? — Java used
  `SharedRef` everywhere; do the same, refine later.
- Return → unnamed output `LiteralParameter` with `<returns>` text.
  `<exception cref>` → named `"throws"` outputs with `Optional` attr —
  exact Java throws encoding, works unchanged for C#'s undeclared
  exceptions (docs-only).
- `Attribute::Async` from the async hint; iterators →
  `Attribute::Generator`. Operators: name = metadata `op_*`;
  conversion ops carry `implicit`/`explicit` + checked-ness as doc
  section (Track A) or new `Attribute` variants (Track B).

### 3.8 IR extensions (two tracks; schema-version consciousness)

**Track A (land first, ZERO IR schema changes)** — everything above maps
onto existing IR using the Java playbook (decorators, `TypeOperator`, doc
sections). Lossy spots: 3-state nullability collapses oblivious→bare;
property accessor asymmetry rides decorators (`"get"`, `"set"`,
`"init"`, `"get;private set"`); events ride `Record.fields` with
decorator `"event"` + delegate type as field type (`Entry::Event` is
`Symbol<()>` — payloadless, unusable as-is); struct-ness/sealed-ness ride
docs.

**Track B (schema bump, aligned with TYPES-ARCHITECTURE precision goals)**
— the four extensions that carry real semantic weight, in priority order:

1. `Primitive::Decimal` (`workspace/ir/primitives.rs`) — decimal128 is not
   IEEE `Float(W128)`; TS/Python `Decimal` want it too.
2. `KnownField` accessors: `accessors: Option<Accessors>` where
   `Accessors { get: Option<Visibility>, set: Option<SetterKind>,
   set_visibility: Option<Visibility> }`, `SetterKind = Set | Init` —
   properties are C#'s dominant member kind; also benefits Swift/Kotlin
   later.
3. Nullability: `nullability: Option<Nullability>` on `LiteralParameter` /
   `KnownField` (3-state `NonNull | Nullable | Oblivious`) or a
   `Type::Nullable` wrapper — pick in review; parameter-level is less
   invasive than a new Type variant.
4. `Entry::Event(Symbol<EventDef>)` with
   `EventDef { delegate_type: Type, add_visibility, remove_visibility }` —
   variant payload change = serialization break, so batch with the next
   schema bump (schema-v3 merge hazard noted in perf work — coordinate).

### 3.9 `xmldoc.rs` — XML docs → markdown

Parse the resolved doc XML (oracle already did inheritdoc/includes):
`<summary>`→ first section; `<remarks>`→ body; `<param>`/`<typeparam>`→
parameter descriptions; `<returns>`→ return description; `<value>`→
property description; `<exception cref>`→ throws list; `<example>` +
`<code language?>`→ fenced blocks; `<c>`→ backticks; `<see cref>`→
resolved link via oracle `docLinks` → `[Name]` + `doc_links` map;
`<see langword="null">`→ `` `null` ``; `<paramref>/<typeparamref>`→
backticked name; `<list type="bullet|number|table">`→ md lists/tables;
`<para>`→ paragraph break; `<inheritdoc>` leftovers→ drop (already
resolved); unknown tags→ strip-preserve-text. Same
summary/body/params/returns/throws struct shape as `ParsedJavadoc`.

---

## Phase 4 — Registry (NuGet ecosystem)

- `workspace/registry/resolve.rs`: NuGet range constraint —
  `RangeConstraint::Range { ecosystem: Language::CSharp, spec }` parsing
  NuGet interval notation (`[1.0.0, 2.0.0)`, bare `1.2.3` = "≥ minimum",
  NOT exact). Comparison via the `NuGetVersion` type from 3.2.
- `versions_url` for CSharp →
  `https://api.nuget.org/v3-flatcontainer/{id_lower}/index.json`;
  `raw_versions` parses `{"versions":[…]}` (filter unlisted via
  registration when listing matters).
- `RegistryOrigin::NuGet` variant + wherever origins enumerate
  (`workspace/registry/lib.rs`, blob emit, server registry store).
- Index/search: `Language::CSharp` in facet enums
  (`workspace/registry/index/mod.rs`, `workspace/server/registry/…`,
  `workspace/server/search/…` — the facets column mirrors `failure`, so
  sweep the ingest→postgres→tantivy path); symbol tokenizer already
  language-agnostic, but add C# test terms (`IEnumerable`, `op_Addition`,
  backtick-arity names must tokenize sanely — strip `` `n `` in the
  tokenizer normalization).
- Ingestion tests: extend `workspace/registry/tests/registry_publish.rs` /
  `index_lifecycle.rs` / `registry_search_tantivy.rs` with a NuGet-origin
  package fixture.

---

## Phase 5 — Renderer (`workspace/compiler/render/emit/csharp.rs`)

- `render/backend.rs`: add `Language::CSharp` (render-local enum) +
  dispatch arm `Language::CSharp => &emit::csharp::CSharp`;
  `emit/mod.rs`: `pub mod csharp;`.
- `pub struct CSharp; impl Backend for CSharp` — six methods:
  `doc_comment` (`/// <summary>…` XML doc emission, wrap at width),
  `ty`, `record`, `sum`, `function`, `interface`.
- Type mapping (inverse of 3.5):

| IR | C# |
|---|---|
| Int(W8/16/32/64/128/Arch) | sbyte/short/int/long/Int128/nint |
| UInt(…) | byte/ushort/uint/ulong/UInt128/nuint |
| Float(W32/other) | float/double |
| Decimal (Track B) | decimal |
| Bool/String/Char/Bytes | bool/string/char/byte[] |
| Date | DateTimeOffset |
| Tuple([]) | void |
| Tuple/NamedTuple | (T1, T2) / (int x, string y) |
| Slice / Array | T[] |
| Any / Infer | object |
| Optional field / TypeOperator "?" | T? |
| Union | object + doc (C# has no unions) — or OneOf<> note |
| FunctionPointer | Func<…>/Action<…> (managed view) |
| BorrowedRef mutable / immutable | ref T / in T |

- `known_type` additions (`emit/mod.rs`): Vec→`List<T>`, Option→`T?`,
  HashMap/BTreeMap→`Dictionary<K,V>`, HashSet→`HashSet<T>`,
  Result→`T` (+doc), Box/Rc/Arc→bare, String→`string` for the CSharp arm.
- Shape decisions (mirror Java's conservatism): Record→`class` (or
  `record` when constructors look positional — start with `class`),
  Sum→`abstract record Base` + `sealed record Variant : Base` per variant
  (C# ≥9 idiom, parallels Java's sealed-interface encoding; fieldless
  all-unit sums → `enum`), TraitDef→`interface` (DIMs render bodies as
  `=> throw` stubs or comment), casing via existing
  `to_pascal_case`/`to_camel_case` helpers.

---

## Phase 6 — Occurrences / references extractor

Per REFERENCES-PLAN (Phases 0–3 landed: occurrence contract +
`LanguageSpec` extractors ×6): add the 7th `LanguageSpec` for C# using
`tree-sitter-c-sharp`. Verify the exact trait shape in
`workspace/ir/pipeline/` before writing (contract landed recently; don't
trust this doc over the code). C# specifics: `using` directives +
aliases, namespace-qualified references, generic-arity name joins
(`List`1` ↔ `List<int>`), attribute-name `[Foo]` ↔ `FooAttribute`
convention, extension-method call sites resolving to the static class.
Known-gaps precedent: Go-method-anchoring / external-import gaps — C#
extension methods and global usings land in the same "resolve later"
bucket; ship with local-resolution only.

---

## Phase 7 — Tests

- `workspace/compiler/tests/fixtures/csharp/<fixture>/` (`.cs` sources,
  SDK-less — mode S needs no csproj for Plain layout): `classes/`,
  `structs/`, `records/`, `interfaces/` (incl. DIMs + static abstracts),
  `enums/` (incl. Flags + underlying types), `delegates/`, `generics/`
  (variance + every constraint incl. `allows ref struct`), `properties/`
  (asymmetric accessors, init, required, indexers), `events/`,
  `operators/` (incl. checked + conversions), `extensions/` (classic +
  C# 14 blocks), `nullability/` (annotated/oblivious mix), `xmldoc/`
  (every tag + inheritdoc + cref forms), `deprecated/`, `visibility/`
  (all 6 + explicit impls), `unsafe/` (pointers, function pointers),
  `asyncawait/`.
- `workspace/compiler/tests/snap_csharp.rs` — clone `snap_java.rs`
  structure: `lower()` via `LocalForgeContext`, `sorted_entries`,
  compile-target snapshots + render snapshots at widths 40/80/100.
  Accept with `INSTA_UPDATE=always buck2 test //workspace/compiler:snap_csharp`.
- Mode M integration test: check in one tiny pre-compiled fixture
  dll+xml (built once by the oracle's own SDK in a genrule, or committed
  binary) to pin metadata-path behavior — records-from-metadata
  (`IsRecord` heuristic, pitfall #12) MUST have a test.
- Sandbox gotchas to pre-empt (learned from go/java snapshot drive):
  `dotnet` on sandbox PATH, writable `DOTNET_CLI_HOME`, first-run
  extraction dir; wire into `.config/scripts/test-all.nu`.
- Registry: NuGet version-grammar unit tests (4-part, prerelease
  case-insensitivity, normalization), flat-container URL construction,
  TFM selection table test.

---

## Phase 8 — Deferred / enhancements (explicit non-goals now)

1. Oracle daemon mode (NDJSON stdio worker, `ExecPlan::Library`-style).
2. `ICSharpCode.Decompiler` source snippets + SourceLink/snupkg real
   sources.
3. BCL doc resolution (`microsoft.netcore.app.ref` .xml mounts) for
   inheritdoc-from-framework.
4. Multi-TFM surface merge (per-member TFM applicability sets) — v1 picks
   ONE canonical TFM and records it (`TargetFrameworkAttribute` echoed in
   the IR module doc); the `#if`-divergence problem (pitfall #5) is
   documented, not solved.
5. Analyzer/source-generator surface extraction (code-exec risk; needs
   threat-tier decision).
6. Track B IR extensions if not batched earlier (3.8).

---

## Pitfall register (verify during implementation)

1. Doc IDs: never hand-construct (`#ctor`, `System#IDisposable#Dispose`,
   `` `n ``/`` ``n `` arity split, `~ReturnType` on conversions, `@` byref,
   `[0:,0:]` multi-dim) — always `GetDocumentationCommentId`.
2. `MetadataReference.CreateFromFile` does NOT auto-load the adjacent
   `.xml` — docs are silently empty without `documentation:`.
3. Record synthesized members are not all `IsImplicitlyDeclared` — filter
   by name+record heuristic, keep user overrides (have XML docs).
4. `IsRecord` from metadata is heuristic — empirical test required.
5. Multi-TFM `#if` surface divergence — pick + record canonical TFM.
6. Oblivious ≠ nullable ≠ non-null — 3-state everywhere.
7. Extension-member APIs (`IsExtension`/`ExtensionParameter`/
   `AssociatedExtensionImplementation`) churned across Roslyn 5.x — pin
   `= 5.6.0`, compile-time-verify.
8. Extension-block doc XML keys target the lowered static impls — join via
   `AssociatedExtensionImplementation`.
9. NuGet versions are not SemVer (`1.0.0.5`) — custom version type, no
   `semver` crate.
10. Flat-container version list includes unlisted versions.
11. `GetTypeByMetadataName` returns null on cross-assembly ambiguity —
    per-assembly lookup for attribute matching.
12. Missing dependency refs degrade silently to error-type symbols — count
    and report as fidelity tier, don't fail (mode S) / do resolve one dep
    level (mode M).
13. `unsafe` and `new`-hiding have no symbol flags — infer.
14. Forwarded types (facade packages) live in dependencies — walk
    `GetForwardedTypes()` or the surface is empty for packages like
    `System.Memory`.
15. dotnet in sandbox: writable `DOTNET_CLI_HOME`, telemetry/nologo env,
    ReadyToRun for startup.

---

## File manifest

**New:** `workspace/compiler/compile/csharp/{mod,error,package,oracle,schema,context,types,item,function,xmldoc,producer,nupkg}.rs`;
`workspace/compiler/compile/csharp/oracle/{Program.cs,Surface.cs,TypeSig.cs,Docs.cs,oracle.csproj,packages.lock.json,BUCK}`;
`workspace/compiler/render/emit/csharp.rs`;
`workspace/compiler/tests/snap_csharp.rs` + `tests/fixtures/csharp/**`.

**Modified:** `workspace/heart/ecosystem.rs` (Language, Toolchain),
`workspace/util/sandbox/profiles.rs` (ProducerProfile::CSharp),
`workspace/compiler/compile/{mod.rs,producer/mod.rs}`,
`workspace/compiler/BUCK` (resources ×2 targets + deps),
`workspace/compiler/render/{backend.rs,emit/mod.rs}`,
`workspace/registry/{resolve.rs,lib.rs,index/mod.rs}` + server registry/search facet sweeps,
`build/toolchains/BUCK` (dotnet toolchain), `flake.nix` (SDK + buckconfig
fragment + offline NuGet feed), `.config/scripts/test-all.nu`.

**Phase order:** 1 → 2 → 3 (mode S first end-to-end, then mode M+nupkg) →
7 (snapshots gate everything) → 4 → 5 → 6 → 8. Phases 4/5/6 are
independent of each other once 3 lands and can go to parallel worktrees.
