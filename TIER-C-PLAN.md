# TIER-C-PLAN — TypeScript Checker Oracle (tsgo)

**Status: SPECIFICATION** (Phase 6 of OXC-PLAN, 2026-07-12)

Detailed implementation plan for the tsgo-backed Tier C oracle (modes 1 & 2),
built on the same subprocess architecture as the Go and Java producers.

---

## 1. Vendoring: tsgo Binary

### 1.1 Source & licensing

- **Source**: npm package `typescript@7.0.2` (Apache-2.0) wraps per-platform native Go binaries
- **Availability**: Available via npm registry; each tarball contains binaries for darwin-arm64, darwin-x64, linux-x64, linux-arm64
- **Strategy**: Extract at build time, vendor per-platform binaries under `build/third-party/tools/`

### 1.2 Build/flake.nix additions

Add a `flake.nix` input for typescript (similar to pyrefly/ruff pattern):

```nix
# flake.nix (outputs.inputs section)
typescript = {
  url = "https://registry.npmjs.org/typescript/-/typescript-7.0.2.tgz";
  flake = false;  # raw tarball, not a flake
};

# In eachSystem:
pkgs.stdenvNoCC.mkDerivation {
  pname = "tsgo";
  version = "7.0.2";
  src = typescript;  # npm tarball
  # Extract the platform-specific binary for this system
  # (darwin-arm64 / darwin-x64 / linux-x64 / linux-arm64)
  nativeBuildInputs = [ pkgs.nodejs ];
  installPhase = ''
    mkdir -p $out/bin
    # npm tarball layout: package/lib/<platform>/tsgo
    cp package/lib/aarch64-apple-darwin/tsgo $out/bin/  # or x86_64-apple-darwin / aarch64-unknown-linux-gnu / x86_64-unknown-linux-gnu
    chmod +x $out/bin/tsgo
  '';
};
```

### 1.3 build/third-party/git.bzl additions

```bzl
TSGO_VERSION = "7.0.2"
TSGO_REVISION = "v7.0.2"  # npm version tag

# Pin each per-platform binary separately
TSGO_BINARIES = {
    "darwin_arm64": {
        "urls": ["https://registry.npmjs.org/typescript/-/typescript-7.0.2.tgz"],
        "sha256": "<computed via nix-prefetch-url>",  # ~40 MB after extraction
    },
    "darwin_x64": { ... },
    "linux_x64": { ... },
    "linux_arm64": { ... },
}
```

Alternatively, fetch the full npm tarball once and extract the platform-specific binary in a genrule (simpler but larger closure).

### 1.4 workspace/compiler/BUCK resources

Update the compiler crate's `resources` attr:

```bzl
resources = {
    "go-oracle": "//workspace/compiler/compile/go/oracle:oracle",
    "java-oracle.jar": "//workspace/compiler/compile/java/oracle:extractor",
    "tsgo": "//build/third-party/tools:tsgo-binary",  # NEW
},
```

Create `build/third-party/tools/BUCK` entry:

```bzl
# Prebuilt tsgo binary (from npm tarball)
http_file(
    name = "tsgo_aarch64_darwin",
    urls = ["<precomputed tarball URL with tsgo binary>"],
    sha256 = "<hash>",
)

# Unpack and expose
export_file(
    name = "tsgo-binary",
    src = ":tsgo_aarch64_darwin",  # or select by platform
    visibility = ["//workspace/compiler/..."],
)
```

---

## 2. Oracle Subprocess Architecture (Go/Java precedent)

### 2.1 Discovery & invocation pattern

**File**: `workspace/compiler/compile/typescript/oracle/mod.rs` (NEW module)

```rust
pub mod tsgo;      // Tier C: tsgo binary lifecycle
pub mod isolated;  // Tier B: isolated_declarations wrapper
```

### 2.2 Binary location (mirroring Go producer)

**File**: `workspace/compiler/compile/typescript/oracle/tsgo.rs`

```rust
/// Locate the tsgo binary vendored as a Buck2 resource.
pub fn tsgo_binary() -> Result<PathBuf> {
    producer::buck_resource("tsgo")
        .map_err(|source| TypescriptError::ResourceNotFound { source })
}
```

Integration point: Call from `Producer::plan()` before constructing `IsolatedCommand`.
Precedent: `workspace/compiler/compile/go/package.rs:28-30` (`oracle_binary()`).

### 2.3 Execution flow

**Call chain**:
1. `TypescriptProducer::plan()` constructs an `IsolatedCommand` (new)
2. `IsolatedCommand::new(&tsgo_binary(), ProducerProfile::Typescript)`
3. Mount sealed input as RO, tmpfs `outDir` as RW
4. `isolate::run_isolated(ctx, cmd)` runs under the sandbox cage
5. `decode()` parses stdout/stderr

**Sandbox profile**: NEW enum variant `ProducerProfile::Typescript` (LOW-tier static parser, same limits as deno_doc/pyrefly).

Update `workspace/util/sandbox/profiles.rs:24`:

```rust
/// TypeScript tsgo oracle (Mode 1 batch + Mode 2 LSP) — LOW static parser.
Typescript,
```

Add limits in `base_limits()`:

```rust
Self::Typescript => Limits::from_const(
    1024 * 1024 * 1024,          // mem 1 GiB
    120,                          // cpu_secs
    2 * 60,                       // wall
    32,                           // pids
    4 * 1024 * 1024,              // max_stdout
    100 * 1024,                   // max_stderr
    512 * 1024 * 1024,            // fsize_bytes
    1024,                         // nofile
),
```

---

## 3. Mode 1: Batch Normalize (API-Extractor pattern)

### 3.1 Concept

For source packages (entry is `.ts` / `.tsx`, not a prebuilt `.d.ts`), when Tier-B diagnostics are non-empty:

1. Run `tsgo --declaration --emitDeclarationOnly --outDir <tmpdir>` inside the sandbox
2. Re-run the Tier-A (`oxc_semantic`) extractor over the emitted `.d.ts`
3. Return the merged result as Tier-C extraction
4. On tsgo failure (type errors, crashes), fall back gracefully to Tier A/B output

### 3.2 Implementation

**File**: `workspace/compiler/compile/typescript/oracle/tsgo.rs`

#### Mode 1 API

```rust
/// Batch normalization mode: tsgo --emitDeclarationOnly + re-extract.
pub fn normalize_with_tsgo<C: ForgeContext>(
    ctx: &C,
    sealed_input: &SealedInput,
    source_root: &Path,
    tier_b_facts: ModuleFacts,  // from isolated_declarations pass
) -> Result<NormalizationOutput, TypescriptError> {
    let tsgo_bin = tsgo_binary()?;
    let tmpdir = sealed_input.budget.fs.scratch_path().join("tsgo-out");
    fs::create_dir_all(&tmpdir)?;
    
    let cmd = IsolatedCommand::new(&tsgo_bin, ProducerProfile::Typescript)
        .args(&["--declaration", "--emitDeclarationOnly", "--outDir"])
        .arg(&tmpdir)
        .arg(source_root)
        .ro(&tsgo_bin)
        .ro(source_root)
        .rw(&tmpdir)
        .rw(sealed_input.budget.fs.scratch_path());
    
    match isolate::run_isolated(ctx, cmd) {
        Ok(output) => {
            if output.status.success() {
                // Re-extract from emitted .d.ts files
                let facts = extract_from_emitted_dts(&tmpdir)?;
                Ok(NormalizationOutput {
                    facts,
                    tier: ExtractionTier::TsgoEmit,
                })
            } else {
                // Soft failure: fall back to Tier A/B
                Ok(NormalizationOutput {
                    facts: tier_b_facts,
                    tier: ExtractionTier::IsolatedDecls,  // downgrade
                    failure_reason: Some(format!(
                        "tsgo: {}",
                        String::from_utf8_lossy(&output.stderr)
                    )),
                })
            }
        }
        Err(e) => {
            // Resource kill, missing toolchain, etc. → fall back
            tracing::warn!("tsgo isolation failed: {}", e);
            Ok(NormalizationOutput {
                facts: tier_b_facts,
                tier: ExtractionTier::IsolatedDecls,
                failure_reason: Some(e.to_string()),
            })
        }
    }
}

/// Extract declarations from emitted `.d.ts` files (re-run Tier-A pipeline).
fn extract_from_emitted_dts(out_dir: &Path) -> Result<ModuleFacts> {
    // Walk out_dir for .d.ts files matching the original module structure
    // Re-parse each with Tier-A (oxc_parser → oxc_semantic → extract)
    // Merge into a new ModuleFacts tree
    // (Identical to Pass 1 of the original two-pass architecture)
    todo!("walk out_dir, parse, semantic, extract per module")
}

#[derive(Debug)]
pub struct NormalizationOutput {
    pub facts: ModuleFacts,
    pub tier: ExtractionTier,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractionTier {
    Syntactic,       // Tier A: pure oxc_parser
    IsolatedDecls,   // Tier B: oxc_isolated_declarations
    TsgoEmit,        // Tier C, Mode 1: tsgo + re-extract
    TsgoLsp,         // Tier C, Mode 2: tsgo LSP oracle
}
```

### 3.3 Integration with Producer

**File**: `workspace/compiler/compile/typescript/producer.rs`

Modify `TypescriptProducer::plan()` and `decode()`:

```rust
fn plan<C: ForgeContext>(
    &self,
    ctx: &C,
    input: &SealedInput,
) -> Result<ExecPlan, ProducerError> {
    // Mode 1: run tsgo as an external subprocess
    let tsgo_bin = oracle::tsgo_binary()
        .map_err(|e| ProducerError::plan(format!("tsgo not found: {}", e)))?;
    
    let mut cmd = IsolatedCommand::new(&tsgo_bin, ProducerProfile::Typescript)
        .args(&["--declaration", "--emitDeclarationOnly"])
        .arg("--outDir").arg(input.budget.fs.scratch_path().join("tsgo-out"))
        .arg(&input.root);
    
    cmd = cmd
        .ro(&tsgo_bin)
        .ro(&input.root)
        .rw(input.budget.fs.scratch_path());
    
    Ok(ExecPlan::Commands(vec![isolate::seal(ctx, cmd)]))
}

fn decode(
    &self,
    input: &SealedInput,
    captured: Captured,
) -> Result<ProducerOutput, ProducerError> {
    // Check tsgo exit status
    if !captured.status.success() {
        return Err(ProducerError::decode(format!(
            "tsgo failed: {}",
            String::from_utf8_lossy(&captured.stderr)
        )));
    }
    
    // Re-extract from emitted .d.ts
    let out_dir = input.budget.fs.scratch_path().join("tsgo-out");
    let index = extract_from_tsgo_output(&out_dir)?;
    
    Ok(ProducerOutput {
        index,
        aux: AuxOutputs {
            extraction_tier: Some(ExtractionTier::TsgoEmit),
            ..Default::default()
        },
    })
}
```

### 3.4 Known GA gaps & fallback strategy

**OXC-PLAN §6.2** documents:
- **#972**: tsgo emits nothing when type errors exist in source
- **#1952**: occasional declaration-transformer crashes on malformed input

**Strategy**: Record in `AuxOutputs` which tier produced the result. If Mode 1 fails, either:
- Retry with Mode 2 (LSP, below) if enabled
- Fall back to Tier A/B output for that package

---

## 4. Mode 2: LSP Oracle (Targeted enrichment)

### 4.1 Concept

For opaque symbols marked by Tier-B diagnostics, spawn one warm `tsgo --lsp -stdio` process per package and:

1. `textDocument/hover` → strip markdown fence, wrap as `type __T = <printed>;`, parse via oxc_parser
2. `textDocument/definition` / `textDocument/typeDefinition` → cross-module type link resolution

### 4.2 Dependencies

Add to `workspace/compiler/BUCK` deps:

```bzl
crate("async-lsp", pin_only = True),  # Oxalica LSP client
crate("tokio", features = ["io-util", "macros", "rt"], pin_only = True),
```

### 4.3 Implementation

**File**: `workspace/compiler/compile/typescript/oracle/tsgo.rs` (mode 2 extension)

```rust
use async_lsp::lsp::*;
use tokio::process::{Command, Child};
use tokio::io::{AsyncBufReadExt, BufReader, AsyncWriteExt};

/// Warm LSP session for a package.
pub struct TsgoLspSession {
    /// Child process handle (tsgo --lsp -stdio).
    child: Child,
    /// JSON-RPC client (async_lsp wrapping stdin/stdout).
    client: LspClient,
}

impl TsgoLspSession {
    /// Spawn tsgo LSP in the sandbox and initialize.
    pub async fn new<C: ForgeContext>(
        ctx: &C,
        sealed_input: &SealedInput,
    ) -> Result<Self> {
        let tsgo_bin = tsgo_binary()?;
        
        // Run inside sandbox (requires async isolation bridge — see Phase 4 DAEMON-PLAN)
        let child = IsolatedCommand::new(&tsgo_bin, ProducerProfile::Typescript)
            .arg("--lsp")
            .arg("-stdio")
            .ro(&tsgo_bin)
            .ro(&sealed_input.root)
            .rw(sealed_input.budget.fs.scratch_path());
        
        // TODO: async isolation wrapper (currently isolate::run_isolated is sync)
        // For now, spawn directly in tests; production requires ForgeContext cage
        // integration with async support.
        
        let mut child = Command::new(&tsgo_bin)
            .arg("--lsp")
            .arg("-stdio")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        
        let stdin = child.stdin.take().ok_or_else(|| {
            TypescriptError::Other("tsgo stdin unavailable".into())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            TypescriptError::Other("tsgo stdout unavailable".into())
        })?;
        
        let client = LspClient::new(stdin, BufReader::new(stdout));
        
        // Initialize
        let resp = client.initialize(InitializeParams {
            process_id: Some(std::process::id()),
            root_path: Some(sealed_input.root.to_string_lossy().into_owned()),
            root_uri: format!("file://{}", sealed_input.root.display()),
            capabilities: ClientCapabilities {
                text_document: Some(TextDocumentClientCapabilities {
                    hover: Some(HoverClientCapabilities {
                        dynamic_registration: Some(false),
                        content_format: Some(vec![MarkupKind::Markdown, MarkupKind::PlainText]),
                    }),
                    definition: Some(DefinitionClientCapabilities {
                        dynamic_registration: Some(false),
                    }),
                    type_definition: Some(TypeDefinitionClientCapabilities {
                        dynamic_registration: Some(false),
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        }).await?;
        
        Ok(Self { child, client })
    }
    
    /// Get printed type for a symbol (hover).
    pub async fn hover_type(
        &self,
        file_uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Option<String>> {
        let resp = self.client.hover(HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: file_uri.parse()? },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
        }).await?;
        
        match resp {
            Some(Hover { contents, .. }) => {
                let text = match contents {
                    HoverContents::Markup(MarkupContent { value, .. }) => value,
                    HoverContents::Array(arr) => {
                        arr.into_iter()
                            .filter_map(|c| match c {
                                MarkedString::String(s) => Some(s),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    }
                    _ => return Ok(None),
                };
                
                // Strip markdown fence if present
                let stripped = if text.starts_with("```typescript") {
                    text.trim_start_matches("```typescript")
                        .trim_end_matches("```")
                        .trim()
                        .to_string()
                } else {
                    text
                };
                
                Ok(Some(stripped))
            }
            None => Ok(None),
        }
    }
    
    /// Get definition location (type link resolution).
    pub async fn type_definition(
        &self,
        file_uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Option<Location>> {
        self.client.type_definition(TypeDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri: file_uri.parse()? },
                position: Position { line, character },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        }).await.map_err(Into::into)
    }
    
    /// Close the session.
    pub async fn shutdown(mut self) -> Result<()> {
        self.client.shutdown().await?;
        self.child.kill().await.ok();
        Ok(())
    }
}
```

### 4.4 Re-parsing hover types into ir::Type

**File**: `workspace/compiler/compile/typescript/oracle/tsgo.rs` (hover_to_ir_type)

```rust
/// Parse tsgo's printed type string back into ir::Type.
pub fn hover_type_to_ir_type(printed: &str) -> Result<ir::ty::Type> {
    // Wrap as `type __T = <printed>;`
    let src = format!("type __T = {printed};");
    
    let allocator = oxc_allocator::Allocator::new();
    let parser = oxc_parser::Parser::new(&allocator, &src, SourceType::d_ts());
    let ret = parser.parse();
    
    if !ret.diagnostics.is_empty() || ret.panicked {
        return Err(TypescriptError::ParseHoverType {
            printed: printed.to_string(),
            details: format!("{} diagnostics", ret.diagnostics.len()),
        });
    }
    
    // Extract the type alias RHS
    let type_alias = ret.program.body.iter()
        .find_map(|stmt| {
            if let oxc_ast::Statement::TSTypeAliasDeclaration(alias) = stmt {
                Some(&alias.type_annotation)
            } else {
                None
            }
        })
        .ok_or_else(|| TypescriptError::ParseHoverType {
            printed: printed.to_string(),
            details: "no type alias found".into(),
        })?;
    
    // Lower via the existing types.rs lower_ts_type()
    lower_ts_type(type_alias)  // existing Tier-A lowering
}
```

### 4.5 Cross-module type_links from definition

**File**: `workspace/compiler/compile/typescript/link.rs` (new integration)

When a symbol's type is printed by the LSP hover:
- Record the printed string in `SymbolFacts.hover_type`
- On cross-module reference: call `tsgo_lsp.type_definition()` to get the definition location
- Resolve the location URI → (module, export name) → look up in the IR index
- Add a `type_links` entry with the resolved symbol id

```rust
/// Resolve type_link via LSP when semantic couldn't (Tier C enrichment).
pub async fn resolve_type_link_via_lsp(
    session: &TsgoLspSession,
    ref_module: &str,
    ref_line: u32,
    ref_char: u32,
) -> Result<Option<EntryId>> {
    if let Some(loc) = session.type_definition(ref_module, ref_line, ref_char).await? {
        let uri = &loc.uri;
        let def_module = uri_to_module_path(uri)?;
        let def_entry = find_entry_by_module_location(&def_module, loc.range.start)?;
        Ok(Some(def_entry))
    } else {
        Ok(None)
    }
}
```

---

## 5. AuxOutputs Extension

### 5.1 New field in AuxOutputs

**File**: `workspace/compiler/compile/producer/mod.rs`

Update the struct to record which tier produced the result:

```rust
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct AuxOutputs {
    /// Absolute path → source text (Rust).
    pub source_map: Option<HashMap<String, String>>,
    /// TypeScript extraction tier used: syntactic / isolated-decls / tsgo-emit / tsgo-lsp
    pub extraction_tier: Option<String>,  // or enum for strict typing
    /// Diagnostic reason if Mode 1 fell back (for observability).
    pub extraction_failure: Option<String>,
}
```

### 5.2 Recording in Producer

When Mode 1 succeeds:
```rust
Ok(ProducerOutput {
    index,
    aux: AuxOutputs {
        extraction_tier: Some("tsgo-emit".into()),
        extraction_failure: None,
        ..Default::default()
    },
})
```

When Mode 1 fails and falls back:
```rust
Ok(ProducerOutput {
    index,
    aux: AuxOutputs {
        extraction_tier: Some("isolated-decls".into()),
        extraction_failure: Some("tsgo: type errors in source".into()),
        ..Default::default()
    },
})
```

---

## 6. Wiring Checklist: Concrete File Locations

### 6.1 New files to create

1. **`workspace/compiler/compile/typescript/oracle/mod.rs`** (NEW)
   - Module facade
   - Re-exports: `pub use tsgo::*;` and `pub use isolated::*;`

2. **`workspace/compiler/compile/typescript/oracle/tsgo.rs`** (NEW, ~600 lines)
   - `tsgo_binary()` — resource lookup (§2.2)
   - `normalize_with_tsgo()` — Mode 1 (§3.2)
   - `extract_from_emitted_dts()` — re-extraction (§3.2)
   - `TsgoLspSession` — Mode 2 (§4.3)
   - `hover_type_to_ir_type()` — hover parsing (§4.4)

3. **`workspace/compiler/compile/typescript/oracle/isolated.rs`** (NEW, ~200 lines)
   - Tier-B wrapper around `oxc_isolated_declarations`
   - Diagnostics collection & "type requires checker" routing

### 6.2 Files to modify

1. **`workspace/compiler/compile/typescript/producer.rs`**
   - Import: `use super::oracle;`
   - Modify `Producer::plan()` to construct `IsolatedCommand` (§3.3)
   - Modify `Producer::decode()` to handle tsgo output (§3.3)

2. **`workspace/compiler/compile/typescript/mod.rs`**
   - Add `pub mod oracle;` (currently imports: context, item, function, types, docstring)
   - Module-level doc: update references from deno_doc to oxc + oracle tiers

3. **`workspace/compiler/compile/typescript/link.rs`** (if in Phase 3, new file for cross-module logic)
   - Add `resolve_type_link_via_lsp()` (§4.5)
   - Integrate into the main `link_pass()` workflow

4. **`workspace/compiler/BUCK`**
   - Add `"tsgo"` to `resources` dict (line ~11)
   - Add `async-lsp` and tokio features to `deps` (line ~13)

5. **`workspace/util/sandbox/profiles.rs`**
   - Add `Typescript` variant to `ProducerProfile` enum (line ~24)
   - Add `Self::Typescript` arm to `base_limits()` (after `StaticParser`)

6. **`build/third-party/git.bzl`**
   - Add tsgo vendoring entries (§1.3)

7. **`flake.nix`**
   - Add typescript input (§1.2)

### 6.3 Integration points with Phase 2/3

- **Phase 2 (extract)**: `ModuleFacts` type must include optional `hover_type: Option<String>` for opaque symbols
- **Phase 3 (link)**: `resolve_ir_type_to_entry_id()` fallback can call `resolve_type_link_via_lsp()` when semantic resolution fails
- **AuxOutputs wiring**: produced in `Producer::decode()` return (§5.2)

---

## 7. Feature Gating & Phased Rollout

### 7.1 Cargo feature

Add to `workspace/compiler/Cargo.toml`:

```toml
[features]
default = []
typescript-oracle = ["async-lsp", "tokio"]  # Optional; Phase 6 only
```

### 7.2 Conditional compilation

**In oracle modules**:
```rust
#[cfg(feature = "typescript-oracle")]
pub mod tsgo;
#[cfg(feature = "typescript-oracle")]
pub mod isolated;
```

**In producer**:
```rust
#[cfg(feature = "typescript-oracle")]
fn plan(...) -> Result<ExecPlan, ProducerError> {
    // Mode 1 / 2 orchestration
}

#[cfg(not(feature = "typescript-oracle"))]
fn plan(...) -> Result<ExecPlan, ProducerError> {
    // Fallback: Library(WorkerLang::Typescript) → in-process Tier A
}
```

### 7.3 Environment/flag control

Add to config/override flow (if available):
```
NUDOX_TYPESCRIPT_ORACLE_MODE=1       # Mode 1 only
NUDOX_TYPESCRIPT_ORACLE_MODE=2       # Mode 2 (LSP)
NUDOX_TYPESCRIPT_ORACLE_MODE=off     # Disable (Tier A/B only)
```

---

## 8. Testing & Acceptance Criteria

### 8.1 Unit tests

**`tests/typescript_oracle.rs`** (NEW):

```rust
#[test]
fn test_tsgo_binary_location() {
    let bin = oracle::tsgo_binary();
    assert!(bin.is_ok(), "tsgo binary must be resolved");
}

#[test]
fn test_mode1_normalize_happy_path() {
    // Fixture: a simple .ts source with inferred return type
    // Expected: tsgo emits .d.ts, re-extraction recovers the type
}

#[test]
fn test_mode1_fallback_on_errors() {
    // Fixture: .ts with type errors
    // Expected: tsgo fails, fallback to Tier A output, AuxOutputs records failure
}

#[test]
fn test_hover_type_parsing() {
    let printed = "ReturnType<typeof someFunc>";
    let ir_type = hover_type_to_ir_type(printed);
    assert!(ir_type.is_ok());
}
```

### 8.2 Integration tests

- **Corpus sweep**: Run Mode 1 over ~50 npm packages, verify:
  - `AuxOutputs.extraction_tier` distribution (syntactic / isolated-decls / tsgo-emit)
  - No regressions vs. Tier A/B in cases Mode 1 doesn't apply
  - Failure rate < 1% (known GA gaps)

- **Type accuracy**: Compare hover-printed types against hand-verified samples

### 8.3 Benchmark

Record Mode 1 wall time per package (overhead should be < 50% vs. Tier A/B, since tsgo is fast).

---

## 9. Known Limitations & Future Work

### 9.1 Phase 6 (this plan) constraints

1. **Mode 2 async isolation** — requires ForgeContext to support async spawning (Phase 4 DAEMON-PLAN). For now, Mode 2 is a skeleton; production requires cage bridge for async commands.

2. **Tsgo GA gaps**:
   - No type info when source has errors (#972)
   - Occasional transformer crashes on malformed input (#1952)
   - Workaround: always fall back gracefully; record tier in AuxOutputs

3. **LSP hover truncation**: tsgo truncates huge printed types (2-3 KB limit observed). For those, synthesis/fallback to Tier A.

4. **JSON-RPC stability**: async-lsp is young; consider testing with multiple concurrent sessions or falling back to CLI mode if needed.

### 9.2 Future (Phase 7+)

- **TS 7.1+ programmatic API**: if stable, replace LSP client with direct Go FFI (tsgolint pattern)
- **Permanent LSP oracle pool**: reuse one warm tsgo process across multiple packages (save startup overhead)
- **Differential corpus harness** (mentioned in OXC-PLAN §7): enhance to show tier breakdown

---

## 10. File Tree Summary

```
workspace/compiler/compile/typescript/
  mod.rs                      — add "pub mod oracle;"
  producer.rs                 — updated plan()/decode()
  entry.rs                    — (unchanged from Phase 2)
  graph.rs                    — (unchanged from Phase 1)
  extract/
    mod.rs, facts.rs, ..      — (unchanged from Phase 2)
  link.rs                     — add resolve_type_link_via_lsp() (Phase 3)
  oracle/                     — NEW (this plan)
    mod.rs                    — module facade
    tsgo.rs                   — binary location, Mode 1, Mode 2, hover parsing
    isolated.rs               — Tier-B oxc_isolated_declarations wrapper

workspace/util/sandbox/
  profiles.rs                 — add ProducerProfile::Typescript variant + base_limits()

workspace/compiler/
  BUCK                        — add "tsgo" resource, async-lsp/tokio deps

build/third-party/
  git.bzl                     — add TSGO_VERSION, TSGO_BINARIES entries
  tools/BUCK                  — (if tooling for binary extraction needed)

flake.nix                     — add typescript input
```

---

## References

- **OXC-PLAN.md** §6 (this file's parent)
- **Go producer** pattern: `workspace/compiler/compile/go/producer.rs` + `oracle.rs` + `package.rs:111-169` (run_oracle)
- **Java producer** pattern: `workspace/compiler/compile/java/producer.rs` (adaptive multi-step)
- **Sandbox profiles**: `workspace/util/sandbox/profiles.rs:14-165`
- **Isolate module**: `workspace/compiler/compile/isolate.rs:73-161` (IsolatedCommand API)
- **async-lsp**: github.com/oxalica/async-lsp (LSP client over stdio)

