# Handoff: Audit IR, graph, treesitter
- Transcript: `/Users/philocalyst/.claude/projects/-Users-philocalyst-Projects-Backend/befc9920-dc9d-40dc-b94e-b3a2d2ed39ad/subagents/agent-a02d75ccbb278be76.jsonl`
- Agent id: `agent-a02d75ccbb278be76`
- Deliverable: `/Users/philocalyst/Projects/Backend/docs/research/librarification/04-ir-audit.md`
- Existing report: False (0 lines)
- Tools used: 58 ({'Bash': 11, 'Read': 47})
- Files/greps touched: 47
- Web queries: 0

## Web queries performed

## Files read / grepped (sample)
- /Users/philocalyst/Projects/Backend/workspace/ir/lib.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/entry.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/function.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/kind.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/record.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/ty.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/generics.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/parameter.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/module.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/primitives.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/protocols.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/syntax/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/syntax/occurrence.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/syntax/types.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/syntax/walker.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/pipeline/pipeline.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/pipeline/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/syntax/body.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/identity
- /Users/philocalyst/Projects/Backend/workspace/heart/content.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/symbol.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/identity/symbol.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/identity/package.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/identity/id.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/identity/namespace.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/identity/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/graph/model.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/graph/from_ir.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/graph/link.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/graph/symtab.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/linked_data/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/linked_data/emit.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/blob_info.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/cst.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/source_archive.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/occurrences.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/resolve.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/treesitter/spec.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/treesitter/mod.rs
- /Users/philocalyst/Projects/Backend/workspace/heart/package
- /Users/philocalyst/Projects/Backend/workspace/ir/Cargo.toml
- /Users/philocalyst/Projects/Backend/workspace/heart/package/coordinates.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/linked_data/schema.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/generate/surface.rs
- /Users/philocalyst/Projects/Backend/workspace/ir/pipeline/parse_common.rs
- /Users/philocalyst/Projects/Backend/workspace/compiler/graph/mod.rs

## Assistant synthesis fragments (inert history — verify)

### fragment

I'll conduct a thorough audit of the IR, graph, treesitter, and identity systems in the nudox workspace. Let me start by exploring the codebase structure broadly.

### fragment

Now I have enough information to write the comprehensive audit report. Let me create the output directory and write the report:

## ORIGINAL PROMPT (complete this mission)

You are a research subagent producing an exhaustive codebase audit that feeds a major architecture plan.

CONTEXT — the nudox project (repo: /Users/philocalyst/Projects/Backend):
nudox is a multi-language code-intelligence / documentation platform: per-language "producers" (rust-analyzer, OXC, Pyrefly, go/types, javadoc doclet, Roslyn, snix) lower package source to a shared IR (workspace/ir), emitted as blobs into a content-addressed store, fanned out to Tantivy (text), Qdrant (vectors), TerminusDB (graph).

THE PLANNED RESTRUCTURE ("librarification") — the parts your audit feeds:
- Both the remote INDEX and the local REGISTRY must store, alongside IR: resolved tree-sitter trees and source code, as efficiently as possible.
- TerminusDB is kept only for the hottest packages; for everything else, graph-specific operations must run directly over IR loaded from blobs (or over the graph representation used for Terminus publishing — whichever is simpler).
- The whole pipeline becomes deeply incremental: on a committed change, only changed symbols are re-embedded/re-indexed/re-published. This demands a rigorous notion of SYMBOL IDENTITY ACROSS GENERATIONS — "when are two symbols, across versions, the same?" — and lineage links between IR generations published to Terminus.

YOUR MISSION — exhaustively audit the IR and its graph/treesitter consumers:
- workspace/ir (all modules: entry, function, record, generics, protocols, ty, module, primitives, parameter, syntax, kind, pipeline; note the `Option<Vec<T>>` absent-vs-empty convention, yoke usage, arborium-tree-sitter dep, facet/serde features)
- workspace/compiler/graph/ (model.rs, from_ir.rs, link.rs — IR→TerminusDB lowering)
- workspace/compiler/treesitter* (CST snippet extraction)
- workspace/compiler/generate/linked_data and generate/ (how IR becomes blobs; wave-ordered linked-data emit)
- workspace/heart identity types (PackageId, SymbolId, EntryUri, ContentHash) and any occurrence/reference contract (a REFERENCES-PLAN introduced an occurrence contract + SymbolTable refactor + graph Reference projection — find what landed)

Answer with file:line evidence:
1. The exact IR shape: every top-level type, how symbols are identified (what IS a SymbolId today — how is it constructed, is it stable across recompiles? across versions?), how entries nest, how types/generics/signatures are represented, how docs/attributes attach.
2. Serialization: which blobs exist, their formats (serde JSON? facet? tar?), naming/addressing (ContentHash?), whether serialization is canonical/deterministic (map ordering, hashes, floats).
3. The occurrence/reference layer: what landed of the occurrence contract, SymbolTable, resolve engine, Reference projection. How references point at symbols (by what key?).
4. Graph lowering: the TerminusDB document model (model.rs), how from_ir.rs walks the IR, what link.rs resolves, what IDs documents get. Critically: which graph OPERATIONS the system needs (what queries does anything run against Terminus today / what is the graph FOR), and whether those operations could be served by walking IR blobs directly with an in-memory index — sketch what that "graph-over-blobs" module would need.
5. Tree-sitter: how trees/snippets are produced and stored (yoke? arborium?), whether whole resolved trees are kept or only snippets, any serialization of trees, and what "store resolved treesitter trees + source efficiently" would require given the current shapes.
6. Symbol identity readiness: audit exactly which parts of a symbol's current representation are stable across (a) a recompile of identical source, (b) a source change elsewhere in the package, (c) a rename, (d) a signature change, (e) a move between modules/files. Identify every place instability creeps in (ordinals, file paths, spans, hash inputs). This feeds a cross-generation identity design — be precise about what exists.
7. Incrementality: is there any diffing between IR generations today? Where would symbol-level diff naturally sit (IR level vs graph-doc level)? What canonical per-symbol hash could be computed from existing data, and what's missing?

DELIVERABLE:
- Write your FULL report as dense markdown to /Users/philocalyst/Projects/Backend/docs/research/librarification/04-ir-audit.md. Be exhaustive (600-1500 lines). Cite file:line everywhere.
- Do NOT build anything; pre-existing build failures are out of scope. Root-level compiler/ is legacy; live code is workspace/.
- Your final reply must contain ONLY: (a) 300-600 word executive summary, (b) top concrete recommendations, (c) open questions/risks.


## EXTRACTED TOOL RESULTS (stale — re-verify key facts)

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/entry.rs
```
1	use std::path::PathBuf;
2	
3	/// ! We store our data as a nested tree structure to ensure maximum
4	/// composability for building the structure, and the ease of Serde and so on
5	/// and so forth. We're able to store references to other objects using absolute
6	/// paths, and during upload time, a graph is composed once. A (highly) unsafe
7	/// structure should be considered in the future to avoid this reconstruction
8	/// between backends.
9	use rustc_hash::FxHashMap as HashMap;
10	#[cfg(feature = "serde")]
11	use serde::{Deserialize, Serialize};
12	
13	pub use crate::kind::Entry;
14	
15	/// A path to a location within the Nudox registry.
16	#[derive(Debug, Clone, PartialEq, Eq, Hash)]
17	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
18	pub enum NudoxPath {
19		/// A path inside an external dependency, bundled with its crate/package name.
20		External { path: PathBuf, dependency: String },
21	
22		/// A path local to the current project / crate / package.
23		Local(PathBuf),
24	}
25	
26	/// Top-level index table for IR entries.
27	///
28	/// A flat `HashMap` keyed by stable integer IDs keeps TerminusDB integration
29	/// straightforward and avoids deep nesting at the root level.
30	#[derive(Debug, Clone, PartialEq)]
31	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
32	pub struct Index {
33		pub root_ids:        Vec<NudoxPath>,
34		pub entries_by_path: HashMap<NudoxPath, Entry>,
35	}
36	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/lib.rs
```
1	// Convention: fields typed `Option<Vec<T>>` distinguish *absent* (`null` in
2	// JSON) from *empty* (`[]`). Callers must double-unwrap; this is intentional
3	// because the distinction carries semantic meaning (e.g. "no parameters" vs
4	// "zero parameters").
5	pub mod entry;
6	pub mod function;
7	pub mod generics;
8	pub mod kind;
9	pub mod module;
10	pub mod parameter;
11	pub mod pipeline;
12	pub mod primitives;
13	pub mod protocols;
14	pub mod record;
15	pub mod syntax;
16	pub mod ty;
17	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/function.rs
```
1	use rustc_hash::FxHashMap as HashMap;
2	#[cfg(feature = "serde")]
3	use serde::{Deserialize, Serialize};
4	
5	use crate::{entry::NudoxPath, generics::Generics, parameter::Parameter, protocols::ReceiverKind};
6	
7	#[derive(Debug, Clone, PartialEq)]
8	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
9	pub struct Function {
10		pub input_parameters:  Option<Vec<Parameter>>,
11		pub output_parameters: Option<Vec<Parameter>>,
12	
13		pub type_links:  Option<HashMap<String, i64>>,
14		pub attributes:  Option<Vec<Attribute>>,
15		pub generics:    Option<Generics>,
16		pub receiver:    Option<ReceiverKind>,
17		pub overloads:   Option<Vec<Function>>,
18		pub implemented: bool,
19	
20		/// Child entries conceptually scoped inside this function.
21		///
22		/// In many dynamic languages (e.g., JavaScript/TypeScript), functions are
23		/// first-class objects that can act as namespaces containing static
24		/// properties, nested classes, or secondary exported functions.
25		pub members: Option<Vec<NudoxPath>>,
26	
27		/// Protocols or traits implemented explicitly by this function object.
28		///
29		/// While rare in systems languages, callable objects or first-class functions
30		/// may dynamically implement interfaces at runtime.
31		pub implemented_protocols: Option<Vec<NudoxPath>>,
32	}
33	
34	impl Eq for Function {}
35	
36	/// The various attributes a function can have
37	#[derive(Debug, Clone, PartialEq)]
38	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
39	pub enum Attribute {
40		/// Takes an arbitrary amount of arguments
41		Variadic,
42	
43		/// Can suspend and resume execution between yields.
44		Generator,
45	
46		/// Determined at compile-time
47		Const,
48	
49		/// No side-effects
50		Pure,
51	
52		/// Runs asynchronously
53		Async,
54	
55		/// For Rust, happens within an unsafe context
56		Unsafe,
57	}
58	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/kind.rs
```
1	use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
2	#[cfg(feature = "serde")]
3	use serde::{Deserialize, Serialize};
4	
5	use crate::{
6		entry::NudoxPath,
7		function::Function,
8		generics::{ConstExpr, Generics},
9		module::Module,
10		protocols::{TraitDef, TraitImpl},
11		record::{Record, SumType},
12		ty::Type,
13	};
14	
15	/// Typed payload for constants and variables — type and optional value so
16	/// resolution and docs retain the binding's contract (not just the name).
17	///
18	/// Producers that cannot yet recover a type leave both fields `None`; that is
19	/// equivalent to the historical empty `()` payload.
20	#[derive(Debug, Clone, PartialEq, Default)]
21	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
22	pub struct TypedBinding {
23		/// The binding's type, when known.
24		#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
25		pub ty: Option<Type>,
26		/// Compile-time value (literals, simple expressions), when known.
27		#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
28		pub value: Option<ConstExpr>,
29		/// Whether the binding is mutable (`let mut`, `static mut`, Python non-Final, …).
30		/// `None` when the producer does not distinguish mutability.
31		#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
32		pub mutable: Option<bool>,
33	}
34	
35	/// Payload for type aliases / typedefs, preserving declaration-site generics.
36	///
37	/// Historical `Entry::TypeAlias(Symbol<Type>)` only stored the RHS; generic
38	/// parameters on `type Foo<T> = …` / `type IsString<T> = …` were discarded.
39	#[derive(Debug, Clone, PartialEq)]
40	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
41	pub struct TypeAliasBody {
42		/// Generic parameters on the alias declaration, if any.
43		#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
44		pub generics: Option<Generics>,
45		
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/record.rs
```
1	#[cfg(feature = "serde")]
2	use serde::{Deserialize, Serialize};
3	
4	use crate::{entry::NudoxPath, function::Function, generics::{ConstExpr, Generics}, kind::Visibility, ty::Type};
5	
6	#[derive(Debug, Clone, PartialEq)]
7	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
8	pub struct Record {
9		/// Optional name of the record (e.g., "User", "Point").
10		/// Anonymous records (like tuples or JS objects) may omit this.
11		pub name: Option<String>,
12	
13		/// Optional generic parameters (e.g., <T, U>).
14		pub generics: Option<Generics>,
15	
16		/// The fields of the record (if applicable).
17		/// An empty lack of fields implies dynamic fields, AKA classical JS and
18		pub fields: Vec<Field>,
19	
20		/// Callable signatures attached directly to the record shape.
21		pub call_signatures: Option<Vec<Function>>,
22	
23		/// Constructor signatures attached directly to the record shape.
24		pub constructors: Option<Vec<Function>>,
25	
26		/// Inline methods declared directly on the record shape.
27		pub methods: Option<Vec<Function>>,
28	
29		/// Index signatures such as `[key: string]: number`.
30		pub index_signatures: Option<Vec<IndexSignature>>,
31	
32		/// Base or implemented types explicitly referenced by this record.
33		pub super_types: Option<Vec<Type>>,
34	
35		/// Child entries conceptually scoped to this Record.
36		///
37		/// Explicit struct fields are tracked via the `fields` array inline. However,
38		/// a Record might also encapsulate full nested types (e.g., Java nested
39		/// classes), static namespaces, or separated explicit methods (e.g., Rust
40		/// inherent `impl` blocks).
41		pub members: Option<Vec<NudoxPath>>,
42	
43		/// Protocols, traits, or interfaces this Record explicitly implements.
44		pub implemented_protocols: Option<Vec<NudoxPath>>,
45	}
46	
47	/// For defining the shape of data
48	/// Ex: [key: string]: number;
49	#[derive(Debug, Clone, PartialEq)]
50	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/ty.rs
```
1	#[cfg(feature = "serde")]
2	use serde::{Deserialize, Serialize};
3	
4	use super::function;
5	use crate::{generics::{GenericArg, TraitRef}, parameter::Parameter, primitives::Primitive, protocols::GenericBound, record::{Record, SumVariant}};
6	
7	/// Universal representation of types across languages.
8	#[derive(Debug, Clone, PartialEq)]
9	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
10	#[cfg_attr(feature = "serde", serde(tag = "type", content = "value"))]
11	pub enum Type {
12		/// A named reference to a concrete type or struct.
13		/// Ex: `std::string::String`, `MyStruct`, `Vec<T>`
14		TypeReference(TypeReference),
15	
16		/// A receiver/self type such as Rust `Self` or TypeScript `this`.
17		SelfType,
18	
19		/// A dynamically dispatched trait object or interface.
20		/// Ex: `dyn std::fmt::Display` in Rust, or `Runnable` in Java.
21		DynTrait(DynTrait),
22	
23		/// A generic type parameter or a Higher-Kinded Type (HKT) variable.
24		/// Ex: `T` in `Box<T>`, or `F<_>` in Scala.
25		GenericParam(GenericParam),
26	
27		/// A fundamental, language-level built-in type.
28		/// Ex: `i32`, `f64`, `bool`.
29		// NOTE: Most literals should resolve to this
30		Primitive(Primitive),
31	
32		/// A function signature or pointer to a function.
33		/// Ex: `fn(i32) -> bool` or `(a: number) => string`.
34		FunctionPointer(FunctionPointer),
35	
36		/// A fixed-length, heterogeneous collection of types.
37		/// Ex: `(i32, String)`. An empty vec `()` represents the Unit type.
38		Tuple(Vec<Type>),
39	
40		/// An inline record or object literal type.
41		RecordLiteral(Box<Record>),
42	
43		/// A dynamically-sized view into a contiguous sequence.
44		/// Ex: `[u8]` or `[]T`.
45		Slice(Box<Type>),
46	
47		/// A fixed-size contiguous sequence.
48		/// Ex: `[i32; 4]` or `std::array<int, 4>`.
49		Array { r#type: Box<Type>, length: usize },
50	
51		/// An abstract type bound by traits (Existential types).
52		/// Ex: `impl Iterator<Item = u8>`.
53		ImplTrait(Vec<Generi
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/generics.rs
```
1	#[cfg(feature = "serde")]
2	use serde::{Deserialize, Serialize};
3	
4	use crate::{parameter::Parameter, ty::Type};
5	
6	/// A universal representation of a generic parameter list across languages.
7	///
8	/// `Generics` bundles every kind of compile-time parameter — type, constant,
9	/// lifetime, dependent, and module variables — together with the constraints
10	/// that govern them into a single, language-agnostic structure.  It mirrors
11	/// closely what an angle-bracket or parenthetical list encodes in languages
12	/// like Rust, C++, Swift, TypeScript, Scala, OCaml, Agda, and Haskell.
13	#[derive(Debug, Clone, PartialEq)]
14	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
15	pub struct Generics {
16		/// The parameters this generic scope introduces, in declaration order.
17		pub params: Vec<Parameter>,
18	
19		/// Additional constraints that must hold over the parameters above.
20		pub constraints: Vec<Constraint>,
21	}
22	
23	/// A predicate that restricts how the generic parameters of a declaration may
24	/// be instantiated.
25	///
26	/// Constraints are collected separately from the parameters themselves so that
27	/// complex where-clauses can be represented without bloating each individual
28	/// parameter definition.
29	#[derive(Debug, Clone, PartialEq)]
30	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
31	pub enum Constraint {
32		/// A trait or protocol conformance bound (e.g., `T: Clone`).
33		TraitBound { param: String, trait_ref: TraitRef },
34	
35		/// A bound on an associated type (e.g., `T::Item: Display`).
36		AssociatedTypeBound { param: String, assoc_name: String, bound: TypeExpr },
37	
38		/// A higher-kinded bound constraining the *kind* of a type constructor
39		/// (e.g., `F: * -> *`).
40		HigherKindedBound { param: String, kind: Kind },
41	
42		/// An associated-item equality constraint (e.g., `Iterator<Item = u8>`).
43		AssociatedItem { name: String, args: Option<Vec<GenericArg>>, term: Term },
44	
45		/
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/parameter.rs
```
1	#[cfg(feature = "serde")]
2	use serde::{Deserialize, Serialize};
3	
4	use crate::{generics::{ConstExpr, Kind, TypeExpr, Variance}, ty::Type};
5	
6	/// Represents a complete parameter — whether value-level (a typed argument)
7	/// or generic (type, constant, lifetime, dependent, or module).
8	///
9	/// This unified enum lets callers iterate over *all* parameter kinds in a
10	/// single collection without losing the type information that distinguishes
11	/// them.  The ordering of variants follows a rough progression from the most
12	/// concrete (value-level literals) to the most abstract (module-level).
13	#[derive(Debug, Clone, PartialEq)]
14	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
15	pub enum Parameter {
16		/// A value-level argument passed to a function or method at call-sites.
17		Literal(LiteralParameter),
18	
19		/// A generic type parameter (e.g., `T`, `K extends Hashable`).
20		Type(TypeParam),
21	
22		/// A generic constant parameter (e.g., Rust's `const N: usize`).
23		Const(ConstParam),
24	
25		/// A lifetime / region parameter (e.g., Rust's `'a`).
26		Lifetime(LifetimeParam),
27	
28		/// A dependently-typed parameter: a term value whose *type* may reference
29		/// earlier parameters by name (e.g., Agda/Idris/Lean `(n : Nat)`).
30		Dependent(DependentParam),
31	
32		/// A module-level parameter for ML-family functors
33		/// (e.g., OCaml `(M : Map.OrderedType)`).
34		Module(ModuleParam),
35	}
36	
37	/// A concrete, value-level parameter in a function or method signature.
38	///
39	/// This is the most common kind of parameter across all languages —
40	/// a named slot that accepts a value at call-sites, optionally annotated
41	/// with a type, a default, and calling-convention attributes.
42	#[derive(Debug, Clone, PartialEq)]
43	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
44	pub struct LiteralParameter {
45		/// The binding name of the parameter (e.g., `count`, `self`).
46		pub name: String,
47	
48		/// The de
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/module.rs
```
1	#[cfg(feature = "serde")]
2	use serde::{Deserialize, Serialize};
3	
4	use crate::entry::NudoxPath;
5	
6	#[derive(Debug, Clone, PartialEq)]
7	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
8	pub struct Module {
9		pub members: Option<Vec<NudoxPath>>,
10	}
11	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/primitives.rs
```
1	#[cfg(feature = "serde")]
2	use serde::{Deserialize, Serialize};
3	
4	/// An unsigned integer width.
5	#[derive(Debug, Clone, PartialEq)]
6	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
7	pub enum Width {
8		W8,
9		W16,
10		W32,
11		W64,
12		W128,
13	
14		/// Machine-dependent / pointer-sized (e.g., `usize`, `isize`).
15		/// Generally not applicable to Floats
16		Arch,
17	}
18	
19	/// A language-level primitive type, independent of any target architecture.
20	#[derive(Debug, Clone, PartialEq)]
21	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
22	pub enum Primitive {
23		/// Signed integer (8 / 16 / 32 / 64 / 128 / arch-dependent bits).
24		Int(Width),
25	
26		/// Unsigned integer (8 / 16 / 32 / 64 / 128 / arch-dependent bits).
27		UInt(Width),
28	
29		/// Floating-point number (16 / 32 / 64 bits).
30		Float(Width),
31	
32		/// Boolean logic.
33		Bool,
34	
35		/// UTF-8 or platform-equivalent string type.
36		String,
37	
38		/// Single Unicode code-point.
39		Char,
40	
41		/// Raw binary data / byte buffer.
42		Bytes,
43	
44		/// Temporal / date type.
45		Date,
46	
47		/// Raw pointer-sized address (`void *`).
48		Address,
49	}
50	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/protocols.rs
```
1	#[cfg(feature = "serde")]
2	use serde::{Deserialize, Serialize};
3	
4	use super::function;
5	use crate::{entry::NudoxPath, function::Function, generics::{ConstExpr, Constraint, Generics, TraitRef}, parameter::Parameter, record::Field, ty::Type};
6	
7	/// Universal representation of traits (Rust), protocols (Swift), interfaces
8	/// (Java/C#/TypeScript), etc.
9	#[derive(Debug, Clone, PartialEq)]
10	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
11	pub struct TraitDef {
12		/// Generic parameters
13		pub generics: Option<Generics>,
14	
15		/// Supertraits/protocol inheritance/interface extends
16		pub super_traits: Option<Vec<TraitRef>>,
17	
18		/// Associated types (Rust/Swift protocols)
19		pub associated_types: Option<Vec<AssociatedType>>,
20	
21		/// Property requirements declared directly on the trait or interface.
22		pub properties: Option<Vec<Field>>,
23	
24		/// Required methods
25		pub required_methods: Option<Vec<TraitMethod>>,
26	
27		/// Provided/default method implementations
28		pub provided_methods: Option<Vec<TraitMethod>>,
29	
30		/// Required constants/static members
31		pub required_constants: Option<Vec<TraitConstant>>,
32	
33		/// Trait-level attributes
34		pub attributes: Option<Vec<TraitAttribute>>,
35	
36		/// Whether the trait is object-safe / dyn-compatible.
37		///
38		/// `Some(true)` = usable as `dyn Trait`; `Some(false)` = not; `None` = the
39		/// producer did not determine it. Rust populates this from the compiler's
40		/// dyn-compatibility check.
41		#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
42		pub object_safe: Option<bool>,
43	
44		/// Whether the trait is *sealed* (cannot be implemented outside its defining
45		/// crate/module by convention, e.g. a private supertrait). `None` when the
46		/// producer does not detect sealing.
47		#[cfg_attr(feature = "serde", serde(default, skip_serializing_if = "Option::is_none"))]
48		pub sealed: Option<bool>,
49	
50		/// cfg-gatin
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/syntax/mod.rs
```
1	pub mod body;
2	pub mod error;
3	pub mod occurrence;
4	pub mod types;
5	pub mod walker;
6	
7	pub use body::{FunctionBody, ParsedBody};
8	pub use error::ParseError;
9	pub use occurrence::{
10		Confidence, FileOccurrences, KindConfidenceCount, Occurrence, OccurrenceSet, ResolutionStats,
11		Role,
12	};
13	pub use types::{ReferenceKind, ResolvedReference};
14	pub use walker::walk_references;
15	
16	#[cfg(test)]
17	pub mod tests;
18	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/syntax/occurrence.rs
```
1	//! The language-agnostic *occurrence* contract.
2	//!
3	//! An [`Occurrence`] is one span in one source file that either *is* a
4	//! definition or *uses* one, resolved to the [`NudoxPath`] identity the IR,
5	//! graph, and search layers already speak. Tree-sitter is the universal
6	//! baseline producer; semantic oracles later emit the *same* artifact at higher
7	//! [`Confidence`]. Consumers never learn which tier produced a row.
8	//!
9	//! This sits beside the surface IR (never inside `ir::Entry`): occurrences are a
10	//! sibling corpus keyed by path + file/span, not a new shape of declaration.
11	
12	use std::{ops::Range, path::PathBuf};
13	
14	#[cfg(feature = "serde")]
15	use serde::{Deserialize, Serialize};
16	
17	use crate::{entry::NudoxPath, syntax::types::ReferenceKind};
18	
19	/// Whether this span *is* the thing or *uses* the thing.
20	#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
21	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
22	pub enum Role {
23		/// The span names the item at its declaration site.
24		Definition,
25		/// The span refers to an item declared elsewhere.
26		Reference,
27	}
28	
29	/// How the target path was established.
30	///
31	/// The ordering is meaningful: higher variants are more trustworthy, so the
32	/// derived [`Ord`] drives both the resolution ladder's "keep the best match"
33	/// logic and the graph's assertion policy (`>= Index` is graph-worthy).
34	#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
35	#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
36	pub enum Confidence {
37		/// Raw identifier only; target is a best-effort name. Never graph-asserted.
38		Syntactic,
39		/// Unique last-segment match against the package index.
40		Suffix,
41		/// Exact/alias match against the package index, or module-scope sibling.
42		Index,
43		/// Resolved through the file's import table (incl. external deps).
44		Import,
45		/// Resolved by the language's sema
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/syntax/types.rs
```
1	#[derive(Debug, Clone, PartialEq, Eq)]
2	#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
3	pub struct ResolvedReference {
4		pub target: crate::entry::NudoxPath,
5		pub span:   std::ops::Range<usize>,
6		pub kind:   ReferenceKind,
7	}
8	
9	#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
10	#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
11	#[derive(strum::FromRepr)]
12	#[repr(u8)]
13	pub enum ReferenceKind {
14		FunctionCall  = 0,
15		MethodCall    = 1,
16		TypeReference = 2,
17		VariableUse   = 3,
18		MacroInvocation = 4,
19		FieldAccess   = 5,
20		Import        = 6,
21	}
22	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/syntax/walker.rs
```
1	use std::ops::Range;
2	
3	use arborium_tree_sitter as tree_sitter;
4	
5	use crate::syntax::ResolvedReference;
6	
7	pub fn walk_references(
8		tree: &tree_sitter::Tree,
9		source: &str,
10		mut classify: impl FnMut(&str, &str, Option<&str>, Range<usize>) -> Option<ResolvedReference>,
11	) -> Vec<ResolvedReference> {
12		let mut references = Vec::new();
13		let mut cursor = tree.root_node().walk();
14	
15		loop {
16			let node = cursor.node();
17			let parent_kind = node.parent().map(|p| p.kind());
18			let span = node.byte_range();
19			if let Ok(name) = node.utf8_text(source.as_bytes())
20				&& let Some(rr) = classify(name, node.kind(), parent_kind, span.clone())
21			{
22				references.push(rr);
23			}
24	
25			if cursor.goto_first_child() {
26				continue;
27			}
28	
29			loop {
30				if cursor.goto_next_sibling() {
31					break;
32				}
33				if !cursor.goto_parent() {
34					return references;
35				}
36			}
37		}
38	}
39	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/syntax/body.rs
```
1	use std::{ops::Range, sync::Arc};
2	
3	use arborium_tree_sitter as tree_sitter;
4	use yoke::{Yoke, Yokeable};
5	
6	use crate::syntax::{ParseError, ResolvedReference, walker::walk_references};
7	
8	#[derive(Yokeable, Debug, Clone)]
9	pub struct FunctionBody {
10		pub tree:       tree_sitter::Tree,
11		pub references: Vec<ResolvedReference>,
12	}
13	
14	impl PartialEq for FunctionBody {
15		fn eq(&self, other: &Self) -> bool { self.references == other.references }
16	}
17	
18	impl Eq for FunctionBody {}
19	
20	pub type ParsedBody = Yoke<FunctionBody, Arc<str>>;
21	
22	impl FunctionBody {
23		pub fn parse(
24			source: Arc<str>,
25			language: tree_sitter::Language,
26		) -> Result<ParsedBody, ParseError> {
27			Yoke::try_attach_to_cart(source, |source_ref| {
28				let mut parser = tree_sitter::Parser::new();
29				parser.set_language(&language)?;
30				let tree = parser.parse(source_ref.as_bytes(), None).ok_or(ParseError::Parse)?;
31				Ok(FunctionBody { tree, references: Vec::new() })
32			})
33		}
34	
35		pub fn parse_and_resolve(
36			source: Arc<str>,
37			language: tree_sitter::Language,
38			classify: impl FnMut(&str, &str, Option<&str>, Range<usize>) -> Option<ResolvedReference>,
39		) -> Result<ParsedBody, ParseError> {
40			Yoke::try_attach_to_cart(source, |source_ref| {
41				let mut parser = tree_sitter::Parser::new();
42				parser.set_language(&language)?;
43				let tree = parser.parse(source_ref.as_bytes(), None).ok_or(ParseError::Parse)?;
44				let references = walk_references(&tree, source_ref, classify);
45				Ok(FunctionBody { tree, references })
46			})
47		}
48	}
49	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/pipeline/mod.rs
```
1	pub mod parse_common;
2	#[allow(clippy::module_inception)]
3	pub mod pipeline;
4	
5	pub use parse_common::{output_parameters_from_type, parameter_link_key};
6	pub use pipeline::{Collected, Indexed, Ir, Stage};
7	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/pipeline/pipeline.rs
```
1	use rustc_hash::FxHashMap as HashMap;
2	
3	use crate::{entry::{Index, NudoxPath}, kind::Entry};
4	
5	pub struct Collected;
6	pub struct Indexed;
7	
8	pub trait Stage {
9		type Data;
10	}
11	
12	impl Stage for Collected {
13		type Data = Vec<Entry>;
14	}
15	
16	impl Stage for Indexed {
17		type Data = Index;
18	}
19	
20	pub struct Ir<S: Stage> {
21		data: S::Data,
22	}
23	
24	impl Ir<Collected> {
25		pub fn from_entries(entries: Vec<Entry>) -> Self { Ir { data: entries } }
26	
27		pub fn into_entries(self) -> Vec<Entry> { self.data }
28	
29		pub fn index(self) -> Ir<Indexed> {
30			let mut entries_by_path =
31				HashMap::with_capacity_and_hasher(self.data.len(), Default::default());
32			let mut root_ids = Vec::new();
33	
34			for entry in self.data {
35				// Roots are top-level local items only: a single path component, not
36				// nested under another local path (`alpha` yes; `alpha/beta` no) and
37				// not a fully-qualified `::`-style spelling.
38				if let NudoxPath::Local(p) = entry.path() {
39					let lossy = p.to_string_lossy();
40					if p.components().count() == 1 && !lossy.contains("::") {
41						root_ids.push(entry.path().clone());
42					}
43				}
44				entries_by_path.insert(entry.path().clone(), entry);
45			}
46	
47			Ir { data: Index { root_ids, entries_by_path } }
48		}
49	
50		pub fn len(&self) -> usize { self.data.len() }
51	
52		pub fn is_empty(&self) -> bool { self.data.is_empty() }
53	
54		pub fn entries(&self) -> &[Entry] { &self.data }
55	}
56	
57	impl Ir<Indexed> {
58		pub fn get(&self, id: &NudoxPath) -> Option<&Entry> { self.data.entries_by_path.get(id) }
59	
60		pub fn root_ids(&self) -> &[NudoxPath] { &self.data.root_ids }
61	
62		pub fn iter(&self) -> impl Iterator<Item = &Entry> { self.data.entries_by_path.values() }
63	
64		pub fn len(&self) -> usize { self.data.entries_by_path.len() }
65	
66		pub fn is_empty(&self) -> bool { self.data.entries_by_path.is_empty() }
67	
68		pub fn into_index(self) -> Index { self.data }
69	}
70	

```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/identity
```
EISDIR: illegal operation on a directory, read '/Users/philocalyst/Projects/Backend/workspace/heart/identity'
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/content.rs
```
1	//! Content addressing, job keys, and freshness.
2	
3	use serde::{Deserialize, Serialize};
4	
5	/// The content hash which serves three roles:
6	/// 1. Ensuring that package freshness hasn't changed.
7	/// 2. Dedupe on the content
8	/// 3. type marking anything that depends on it
9	#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
10	pub struct ContentHash([u8; 32]);
11	
12	impl ContentHash {
13		/// Wrap a raw 32-byte digest (e.g. read back from postgres).
14		pub const fn from_bytes(bytes: [u8; 32]) -> Self { Self(bytes) }
15	
16		/// The raw digest bytes.
17		pub const fn as_bytes(&self) -> &[u8; 32] { &self.0 }
18	
19		/// Hash a contiguous byte buffer.
20		pub fn of_bytes(bytes: &[u8]) -> Self { Self(*blake3::hash(bytes).as_bytes()) }
21	
22		/// Lower-hex encoding for filesystem names and log lines.
23		pub fn hex(&self) -> String { data_encoding::HEXLOWER.encode(&self.0) }
24	
25		pub fn builder() -> ContentHasher { ContentHasher(blake3::Hasher::new()) }
26	}
27	
28	impl std::fmt::Display for ContentHash {
29		fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
30			f.write_str(&self.hex())
31		}
32	}
33	
34	/// An incremental hasher for building a [`ContentHash`] from a stream of parts
35	/// without holding the whole package in memory.
36	pub struct ContentHasher(blake3::Hasher);
37	
38	impl ContentHasher {
39		/// Fold another chunk into the digest.
40		pub fn update(&mut self, bytes: &[u8]) -> &mut Self {
41			self.0.update(bytes);
42			self
43		}
44	
45		/// Finalize into a [`ContentHash`].
46		pub fn finalize(&self) -> ContentHash { ContentHash(*self.0.finalize().as_bytes()) }
47	}
48	
49	/// Domain-separated cache / producer job identity.
50	///
51	/// `JobKey = H(producer_version ‖ toolchain ‖ source ‖ dep_lock)` with each
52	/// component length-prefixed (little-endian `u64`), matching the historical
53	/// `CacheKey::derive` layout so keys stay stable across the CAS migration.
54	///
55	//
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/symbol.rs
```
1	use serde::{Deserialize, Serialize};
2	use smol_str::SmolStr;
3	
4	use crate::{
5	    ecosystem::Language,
6	    identity::{PackageId, SymbolId},
7	};
8	
9	// TODO: Merely derive from the IR, no OTHER and no manualy constructed SYMBOLKIND
10	
11	/// A symbol's identifying names: the bare identifier and its fully-qualified
12	/// path. Private fields with accessors so the two can't be transposed.
13	#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
14	pub struct Name {
15	    pub plain: SmolStr,
16	    pub fully_qualified: SmolStr,
17	}
18	
19	/// The canonical symbol record surfaced by search and graph queries.
20	#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
21	pub struct Symbol {
22	    /// The stable, deterministic global identity of this symbol.
23	    pub id: SymbolId,
24	
25	    /// The package this symbol belongs to.
26	    pub package: PackageId,
27	
28	    /// The ecosystem, carried so the language-erased read plane can still filter.
29	    pub ecosystem: Language,
30	
31	    /// The identifying information of the symbol
32	    pub name: Name,
33	
34	    /// What kind of thing this symbol is.
35	    pub kind: SymbolKind,
36	    // I don't think we need to carry around Generation
37	}
38	
39	/// What kind of code entity a symbol represents.
40	///
41	/// The `Display` token for each variant is its PascalCase name (e.g. `"Function"`).
42	/// `EnumString` matches the same tokens so `from_str` is the exact inverse of
43	/// `to_string()` — no manual match table needed.  `VariantNames::VARIANTS` is
44	/// the static slice the schema CHECK constraint is derived from.
45	#[derive(
46	    Debug,
47	    Clone,
48	    Copy,
49	    PartialEq,
50	    Eq,
51	    Hash,
52	    Serialize,
53	    Deserialize,
54	    strum::Display,
55	    strum::EnumString,
56	    strum::VariantNames,
57	)]
58	pub enum SymbolKind {
59	    Function,
60	    Type,
61	    Module,
62	    Constant,
63	    Variable,
64	    Trait,
65	    Impl,
66	    Other,
67	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/identity/symbol.rs
```
1	//! Symbol identity: the in-package locator ([`EntryUri`]) and the deterministic,
2	//! per-instance [`SymbolId`] derived from it.
3	
4	use serde::{Deserialize, Serialize};
5	use smol_str::SmolStr;
6	
7	use super::{Id, PackageId, namespace};
8	use crate::symbol::Symbol;
9	
10	/// A globally-unique symbol identity — an [`Id`] branded with the serving
11	/// [`Symbol`] record, so it reads literally as `Id<Symbol>`. Deterministic per
12	/// graph instance (offline-recomputable) and salted with the TerminusDB instance
13	/// so two instances of the same corpus don't share ids. Derive one with
14	/// [`EntryUri::symbol_id`].
15	pub type SymbolId = Id<Symbol>;
16	
17	/// A symbol's path *within* a package — the ecosystem-relative locator that,
18	/// combined with the graph instance, yields a [`SymbolId`].
19	#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
20	pub struct EntryUri {
21		/// The package the symbol lives in.
22		pub package: PackageId,
23		/// The `::`/`.`-agnostic path segments to the symbol within the package.
24		pub path:    Box<[SmolStr]>,
25	}
26	
27	impl EntryUri {
28		/// The canonical string form of this URI: the package id followed by every
29		/// path segment, `/`-joined — a separator no ecosystem's symbol grammar uses,
30		/// so the form is unambiguous and injective.
31		pub fn canonical(&self) -> String {
32			self.path.iter().fold(self.package.to_string(), |mut uri, segment| {
33				uri.push('/');
34				uri.push_str(segment);
35				uri
36			})
37		}
38	
39		/// Derive the deterministic symbol id from the graph-instance token and this
40		/// URI — the one construction site. Salting with the instance keeps two
41		/// instances of the same corpus from colliding.
42		pub fn symbol_id(&self, instance_token: &str) -> SymbolId {
43			let mut bytes = instance_token.as_bytes().to_vec();
44			bytes.push(0);
45			bytes.extend_from_slice(self.canonical().as_bytes());
46			Id::from_name(&namespace::SYMBOL, &bytes)
47		}
48	}
49	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/identity/package.rs
```
1	//! Package identity: the coordinates that name a package and the deterministic
2	//! [`PackageId`] derived from them.
3	
4	use std::borrow::Cow;
5	
6	use serde::{Deserialize, Serialize};
7	use smol_str::SmolStr;
8	use thiserror::Error;
9	
10	use super::Id;
11	use crate::ecosystem::Language;
12	
13	/// Identity tag for packages. Zero-variant: it exists only to brand [`Id`], never
14	/// to be constructed.
15	pub enum Package {}
16	
17	/// The stable, deterministic identity of a package across the whole system.
18	pub type PackageId = Id<Package>;
19	
20	/// Why a raw package name was rejected.
21	#[derive(Debug, Error, PartialEq, Eq)]
22	pub enum NameError {
23	    #[error("package name is empty")]
24	    NameEmpty,
25	    #[error("package name for {ecosystem} is too long (len={len}, max={max})")]
26	    NameTooLong { ecosystem: Language, len: usize, max: usize },
27	    #[error("package name for {ecosystem} contains invalid characters: {invalid_chars:?}")]
28	    NameHasInvalidChars { ecosystem: Language, invalid_chars: Vec<char> },
29	}
30	
31	/// Wrapper carrying raw input + source for Cargo/npm (both use semver but
32	/// kept distinct so ecosystem is traceable in the error chain).
33	#[derive(Debug, Error)]
34	#[error("invalid cargo version {raw:?}: {source}")]
35	pub struct CargoVersionError {
36	    raw: String,
37	    #[source]
38	    source: semver::Error,
39	}
40	
41	#[derive(Debug, Error)]
42	#[error("invalid npm version {raw:?}: {source}")]
43	pub struct NpmVersionError {
44	    raw: String,
45	    #[source]
46	    source: semver::Error,
47	}
48	
49	/// Wrapper for PEP 440.
50	#[derive(Debug, Error)]
51	#[error("invalid python version {raw:?}: {source}")]
52	pub struct PythonVersionError {
53	    raw: String,
54	    #[source]
55	    source: uv_pep440::VersionParseError,
56	}
57	
58	/// FlakeHub versions are Cargo-semver (`X.Y.Z+rev-{sha}`); kept distinct from
59	/// Cargo so the ecosystem stays traceable in the error chain.
60	#[derive(Debug, Error)]
6
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/identity/id.rs
```
1	//! The one identifier constructor for the whole system.
2	
3	use std::{fmt, marker::PhantomData};
4	
5	use serde::{Deserialize, Serialize};
6	use uuid::Uuid;
7	
8	/// Strongly-typed id: named by phantom `T` so ids for different entity kinds
9	/// are incompatible at the type level.
10	pub struct Id<T>(Uuid, PhantomData<fn() -> T>);
11	
12	impl<T> Id<T> {
13		/// Wrap a raw UUID as a tagged id — the inverse of [`Id::as_uuid`], used when
14		/// hydrating a row read back from storage.
15		pub const fn from_uuid(uuid: Uuid) -> Self { Self(uuid, PhantomData) }
16	
17		/// The raw UUID, for storage keys and wire encoding.
18		pub const fn as_uuid(&self) -> &Uuid { &self.0 }
19	
20		/// Mint a fresh random (v4) id. Use only for values with no natural,
21		/// reproducible key; anything content-derived should use [`Id::from_name`].
22		pub fn new_random() -> Self { Self(Uuid::new_v4(), PhantomData) }
23	
24		/// Derive a deterministic (v5) id from a namespace and name bytes — the single
25		/// primitive every `*Id` derivation (package coordinates, entry URIs, source
26		/// names) is built on, so "the same thing" always hashes to the same id.
27		pub fn from_name(namespace: &Uuid, name: &[u8]) -> Self {
28			Self(Uuid::new_v5(namespace, name), PhantomData)
29		}
30	
31		/// Re-tag this id as identifying a different type, preserving the UUID.
32		pub const fn cast<U>(self) -> Id<U> { Id(self.0, PhantomData) }
33	}
34	
35	impl<T> Clone for Id<T> {
36		fn clone(&self) -> Self { *self }
37	}
38	impl<T> Copy for Id<T> {}
39	impl<T> PartialEq for Id<T> {
40		fn eq(&self, other: &Self) -> bool { self.0 == other.0 }
41	}
42	impl<T> Eq for Id<T> {}
43	impl<T> PartialOrd for Id<T> {
44		fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> { Some(self.cmp(other)) }
45	}
46	impl<T> Ord for Id<T> {
47		fn cmp(&self, other: &Self) -> std::cmp::Ordering { self.0.cmp(&other.0) }
48	}
49	impl<T> std::hash::Hash for Id<T> {
50		fn hash<H: std::hash::Hasher>(&self, state: &m
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/identity/namespace.rs
```
1	//! UUIDv5 namespaces for deterministic id derivation. Each identifiable kind gets
2	//! its own namespace so, e.g., a package and a symbol deriving from the same bytes
3	//! still land on distinct ids.
4	use uuid::Uuid;
5	
6	/// Namespace for [`super::PackageId`] (derived from [`super::PackageCoordinates`]).
7	pub const PACKAGE: Uuid = Uuid::from_u128(0x6e75_646f_785f_706b_675f_6e73_0000_0001);
8	
9	/// Namespace for [`super::SymbolId`] (derived from an [`super::EntryUri`]).
10	pub const SYMBOL: Uuid = Uuid::from_u128(0x6e75_646f_785f_7379_6d5f_6e73_0000_0002);
11	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/identity/mod.rs
```
1	pub mod id;
2	pub mod namespace;
3	pub mod package;
4	pub mod symbol;
5	
6	pub use id::Id;
7	pub use package::{
8	    CargoVersionError, NameError, NpmVersionError, Package, PackageId, PackageVersion,
9	    PythonVersionError, RegistryOrigin, VersionError,
10	};
11	pub use symbol::{EntryUri, SymbolId};
12	
13	/// Re-export so `heart::identity::PackageCoordinates` resolves (mirrors the
14	/// alias used in `registry::identity`).
15	pub use crate::package::Coordinates as PackageCoordinates;
16	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/graph/model.rs
```
1	//! The TerminusDB document model for the nudox IR — the graph-native projection
2	//! of the `ir` crate. `#[derive(TerminusDBModel)]` generates both the schema
3	//! (class/enum/tagged-union definitions) and the instance JSON-LD, so there is
4	//! no hand-written emitter or serde glue.
5	//!
6	//! Design (see GRAPH-ARCHITECTURE.md at the repo root):
7	//! - [`Symbol`] is the node class, client-minted id `Symbol/{lang}/{pkg}/{fq}`.
8	//!   Symbol→symbol relations (`member_of`, `implements`, `extends`, `mentions`,
9	//!   `takes`, `returns`) are `TdbLazy<Symbol>` fields — real class-typed links
10	//!   in the schema, so WOQL/GraphQL traverse them server-side. `EntityIDFor` is
11	//!   never used for edges: it renders as `xsd:string`, not a link.
12	//! - The structural payload stays an inline subdocument tree ([`Shape`]), the
13	//!   document of record for rendering. Its name leaves ([`TyReference`],
14	//!   [`TraitRef`]) carry `resolves_to` links so path queries can walk *through*
15	//!   a signature to named nodes.
16	//! - Attributed relations are reified, `value_hash`-keyed (content-addressed):
17	//!   [`Implementation`] and [`Reference`].
18	//!
19	//! Shape conventions (unchanged):
20	//! - Recursive data enums (e.g. [`Type`], [`ConstExpr`]) use *newtype* variants
21	//!   wrapping named structs/enums — never inline `Variant { … }` struct
22	//!   variants. The derive's schema-tree walker shares its dedup set across
23	//!   newtype payloads but not across inline-struct variants, so only the
24	//!   newtype form terminates on self-reference.
25	//! - IR `Option<Vec<T>>` collapses to `Vec<T>` (TDB List) or `BTreeSet<T>` (TDB
26	//!   Set); genuinely optional singles stay `Option<T>`.
27	//! - Link-bearing types derive only `Debug + Clone` ([`TdbLazy`] is not
28	//!   `PartialEq`), and that leaks transitively through the [`Type`] tree.
29	
30	use std::collections::BTreeSet;
31	
32	// The derive emits Serialize/Deserialize and calls that need these traits 
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/graph/link.rs
```
1	//! The linker: the resolution pass that turns the IR's name-strings into
2	//! symbol IRIs, so every relation in the graph is a real link.
3	//!
4	//! Resolution is **total**: a name that matches no index entry still yields an
5	//! IRI — a stub [`model::Symbol`] (`kind: Unresolved`, `resolved: false`) is
6	//! minted under a deterministic address, so edges never dangle and cross
7	//! package boundaries. When the defining package is ingested later under the
8	//! same identity scheme, its real node lands at a knowable address.
9	//!
10	//! Lookup order: exact fq path → alias → unique last-segment suffix → stub.
11	
12	use std::cell::RefCell;
13	use std::collections::{BTreeMap, BTreeSet, HashMap};
14	
15	use ir::entry::{Index, NudoxPath};
16	use terminusdb_schema::{EntityIDFor, TdbLazy};
17	
18	use super::model as m;
19	use super::symtab::SymbolTable;
20	
21	/// The pseudo-package for names whose owning package is unknown (bare
22	/// identifiers in signatures that resolve nowhere). References with a known
23	/// dependency (`NudoxPath::External`) go under the real package name instead.
24	pub const EXTERN_PACKAGE: &str = "~extern";
25	
26	/// Coordinates of the package being projected.
27	#[derive(Debug, Clone)]
28	pub struct PackageCtx {
29	    pub language: String,
30	    pub package: String,
31	    pub version: Option<String>,
32	}
33	
34	/// Keep IRIs to a conservative charset so client-minted ids are always valid
35	/// TerminusDB document ids. `::` survives (the legacy `Entry/...` URIs proved
36	/// it); everything exotic collapses to `_`.
37	fn sanitize(segment: &str) -> String {
38	    segment
39	        .chars()
40	        .map(|c| {
41	            if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '~') {
42	                c
43	            } else {
44	                '_'
45	            }
46	        })
47	        .collect()
48	}
49	
50	/// Hierarchical coordinate → single id segment.
51	///
52	/// [`EntityIDFor`] only accepts `TypeName/
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/graph/from_ir.rs
```
1	//! Projection from the compiler IR (`ir` crate) into the graph [`model`] —
2	//! the linking pass that makes the store graph-native.
3	//!
4	//! Unlike the old 1:1 mirror, this projection resolves every name-string it
5	//! meets (via [`Linker`]) and emits edges alongside the structural payload:
6	//!
7	//! - `TypeReference`/`TraitRef` leaves get `resolves_to` links;
8	//! - each symbol accumulates derived adjacency (`mentions`, and for functions
9	//!   `takes`/`returns`) as it is projected;
10	//! - `implemented_protocols` → `implements` links, supertypes/supertraits →
11	//!   `extends` links, inverted `members` → `member_of`;
12	//! - `TraitImpl` entries additionally reify an [`model::Implementation`];
13	//! - tree-sitter-resolved function-body references — dropped entirely by the
14	//!   old projection — become [`model::Reference`] nodes (the call graph).
15	//!
16	//! Deliberate losses: `type_links` (superseded by the linker) and the
17	//! structured `LogicalPredicate` tree, flattened to a string as before.
18	//! Multivalued `Option<Vec<_>>` IR fields collapse to `Vec`/`BTreeSet`
19	//! (absent ⇒ empty).
20	
21	use std::collections::BTreeSet;
22	
23	use ir::entry::{Index, NudoxPath};
24	use ir::kind::Visibility as IrVis;
25	use ir::syntax::OccurrenceSet;
26	use terminusdb_schema::{EntityIDFor, TdbLazy};
27	
28	use super::link::{Linker, PackageCtx, package_iri, package_version_iri};
29	use super::model as m;
30	
31	// ───────────────────────── the emit context ─────────────────────────
32	
33	/// Which signature slot is being projected; decides which derived-adjacency
34	/// buckets a resolved name lands in.
35	#[derive(Clone, Copy, PartialEq)]
36	enum Role {
37	    Neutral,
38	    Input,
39	    Output,
40	}
41	
42	/// Per-symbol projection state: the linker plus the adjacency accumulated
43	/// while walking this symbol's shape.
44	struct EmitCx<'l> {
45	    linker: &'l Linker,
46	    role: Role,
47	    mentions: BTreeSet<String>,
48	    takes: BTreeSet
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/graph/symtab.rs
```
1	//! The package symbol table: fq-name → path resolution over one [`Index`].
2	//!
3	//! This is the one place name-resolution semantics live. The graph
4	//! [`Linker`](super::link::Linker) consumes it and encodes results to symbol
5	//! IRIs; the occurrence resolver (`generate::resolve`) consumes the same table
6	//! and stays in [`NudoxPath`]. Building it once means emit-time and
7	//! generate-time resolution can never drift (REFERENCES-PLAN §4.2).
8	
9	use std::collections::hash_map::Entry;
10	
11	use ir::entry::{Index, NudoxPath};
12	use rustc_hash::{FxHashMap as HashMap, FxHashSet as HashSet};
13	
14	/// The path components of a [`NudoxPath`], as owned strings.
15	///
16	/// Mirrors the graph linker's coordinate derivation: for both `Local` and
17	/// `External` paths the fq spelling is the `PathBuf` components joined with
18	/// `::`; the owning package lives elsewhere in the `NudoxPath` and does not
19	/// enter the fq key.
20	///
21	/// Many producers mint single-component paths that already embed `::`
22	/// separators (`"crate::Type::method"`, `"com.example::Foo.bar"`,
23	/// `"pkg.mod::name"`). Those are expanded so suffix resolution and
24	/// module-member tables see real leaf segments. A component that mixes
25	/// language module dots with a trailing `::name` is split as
26	/// `a.b::c` → `["a", "b", "c"]` so treesitter module paths align.
27	pub fn path_segments(path: &NudoxPath) -> Vec<String> {
28		let pb = match path {
29			NudoxPath::Local(pb) => pb,
30			NudoxPath::External { path, .. } => path,
31		};
32		let mut out = Vec::new();
33		for c in pb.iter() {
34			let s = c.to_string_lossy();
35			if s.contains("::") {
36				for part in s.split("::") {
37					if part.is_empty() {
38						continue;
39					}
40					// Module segments often use `.` (Python/Java/C#/Go import paths).
41					if part.contains('.') && !part.contains('/') {
42						out.extend(part.split('.').filter(|p| !p.is_empty()).map(str::to_owned));
43					} else {
44						out.push(
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/linked_data/mod.rs
```
1	//! Processing the IR into linked data (JSON-LD) for the graph store.
2	//!
3	//! The document *schema* is derived from the graph model (`crate::graph`) via
4	//! `terminusdb-schema-derive`; this module owns the [`schema`] context and the
5	//! streaming [`emit`] pipeline that projects an `ir::Index` into graph documents
6	//! and ships them to a sink in bounded batches (never a whole-corpus `Vec`).
7	
8	pub mod emit;
9	pub mod schema;
10	
11	pub use emit::{DocumentSink, emit};
12	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/linked_data/emit.rs
```
1	//! The emission pipeline: project an `ir::Index` into graph documents and stream
2	//! them to a sink.
3	//!
4	//! The index is projected to a [`crate::graph::from_ir::GraphCorpus`] via
5	//! [`crate::graph::from_ir::project`], then serialized to JSON-LD documents and
6	//! handed to the [`DocumentSink`] in the wave order the graph store's
7	//! referential integrity expects (GRAPH-ARCHITECTURE.md §5): packages → bare
8	//! symbols → symbols with links → reified relations → version membership.
9	
10	use std::collections::HashMap;
11	use std::hash::{Hash, Hasher};
12	
13	use ir::entry::Index;
14	use ir::syntax::OccurrenceSet;
15	use serde_json::Value;
16	use terminusdb_schema::{ToJson, ToTDBInstance};
17	
18	use crate::error::{EmitLinkedDataError, GenerateError};
19	use crate::graph::from_ir::project;
20	use crate::graph::link::PackageCtx;
21	
22	/// The `Symbol` fields that are links to other symbols. Wave 2 emits symbols
23	/// with these stripped (targets may not exist yet); wave 3 re-emits the full
24	/// documents once every target does. `resolves_to` lives on the shape's name
25	/// leaves, so the strip is recursive.
26	const EDGE_FIELDS: [&str; 7] =
27		["member_of", "implements", "extends", "mentions", "takes", "returns", "resolves_to"];
28	
29	/// A destination for emitted graph documents — e.g. an NDJSON writer, a batching
30	/// uploader to the graph store, or a test collector. Implementors decide how (and
31	/// whether) to batch; the emitter just streams.
32	pub trait DocumentSink {
33		/// Accept one serialized graph document.
34		fn write(&mut self, document: Value) -> Result<(), GenerateError>;
35	
36		/// Flush any buffered batch. Called once at the end of [`emit`].
37		fn flush(&mut self) -> Result<(), GenerateError> {
38			Ok(())
39		}
40	}
41	
42	/// Per-wave first-write-wins bookkeeping: re-emitting an identical body for an
43	/// `@id` is a no-op, a *different* body for the same `@id` within a wave is a
44	/// conflict. (Across waves, re-emis
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/blob_info.rs
```
1	//! Assembling the sink-ready blob information from the generated resolutions.
2	//!
3	//! This is the bridge to the registry's content-addressed blob model: it folds
4	//! the per-file digests into the canonical snapshot [`heart::ContentHash`] — the
5	//! sorted fold of per-file hashes — that postgres records as a package's identity
6	//! and freshness key, and carries the leaf references (surface IR, CST spans) the
7	//! blob manifest points at.
8	
9	use heart::content::ContentHash;
10	use ir::entry::Index;
11	
12	use crate::generate::{cst::CstSet, source_archive::SourceArchive};
13	
14	/// The assembled blob information the registry blob layer consumes: the
15	/// content-addressed source archive plus the canonical package snapshot hash.
16	pub struct BlobInfo {
17		/// The source archive (per-file content hashes, sorted by path).
18		pub archive: SourceArchive,
19		/// The canonical content hash of the whole package snapshot.
20		pub snapshot: ContentHash,
21	}
22	
23	impl BlobInfo {
24		/// Fold the generated resolutions into the canonical snapshot [`ContentHash`]
25		/// and assemble the blob info.
26		///
27		/// The hash is a deterministic fold: per-file `(path, hash)` pairs in sorted
28		/// order streamed through a [`ContentHasher`], so the same package snapshot
29		/// always yields the same hash on any machine.
30		pub fn assemble(surface: &Index, cst: &CstSet, archive: SourceArchive) -> Self {
31			// The surface and CST are derived deterministically from the same
32			// source bytes the archive digests, so they carry no extra identity;
33			// they ride along only for the manifest's leaf references.
34			let _ = (surface, cst);
35	
36			let mut hasher = ContentHash::builder();
37			for file in &archive.files {
38				// Length-prefix each path so the (path, hash) stream is injective —
39				// no split point ambiguity between neighbouring entries.
40				let path = file.path.to_string_lossy();
41				hasher.update(&(path.len() as u64).to_le_bytes());
4
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/mod.rs
```
1	//! Generate — the "runs computer" half of the indexing flow: take a materialized
2	//! package and produce the resolutions that become a blob and the graph
3	//! documents.
4	//!
5	//! - [`surface`]: the API surface (`ir::Index`), lowered by `crate::languages`;
6	//! - [`cst`]: the concrete-syntax-tree resolution (tree-sitter) as *serializable*
7	//!   resolved-reference spans (the live tree is transient — see [`cst`]);
8	//! - [`source_archive`] + [`tar`]: the condensed, content-addressed source;
9	//! - [`blob_info`]: the canonical [`heart::Generation`] + per-file digests the
10	//!   registry blob layer consumes;
11	//! - [`linked_data`]: streamed JSON-LD graph documents for the graph store.
12	//!
13	//! The output is aligned with the registry's content-addressed blob model: the
14	//! same per-file [`heart::ContentHash`]es and the canonical [`heart::Generation`]
15	//! the registry keys storage and freshness on.
16	
17	pub mod blob_info;
18	pub mod cst;
19	pub mod linked_data;
20	pub mod occurrences;
21	pub mod parse_cache;
22	pub mod resolve;
23	pub mod source_archive;
24	pub mod surface;
25	pub mod tar;
26	
27	use std::path::PathBuf;
28	
29	use heart::{ContentHash, JobKey, Toolchain};
30	use heart::package::Coordinates as PackageCoordinates;
31	use ir::entry::Index;
32	use ir::syntax::OccurrenceSet;
33	
34	pub use blob_info::BlobInfo;
35	pub use cst::CstSet;
36	pub use source_archive::{FileDigest, SourceArchive};
37	
38	use crate::generate::resolve::RESOLVER_VERSION;
39	
40	use crate::compile::producer::{self, ForgeContext, LocalForgeContext};
41	use crate::error::GenerateError;
42	
43	/// A materialized package ready to generate from: its verified identity, the
44	/// toolchain it was resolved against, and the root of its extracted source.
45	pub struct PackageInput {
46		/// The package's canonical coordinates (origin × name × version).
47		pub coordinates: PackageCoordinates,
48		/// The toolchain the source was resolved/analyzed against.
49		pub toolch
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/cst.rs
```
1	//! Generating the concrete-syntax-tree resolution (tree-sitter) for a package.
2	//!
3	//! The live tree-sitter `Tree` is C-allocated and non-serializable, and one tree
4	//! per package is the wrong granularity. So the CST resolution carried forward is
5	//! *per-file* and reduced to the serializable data the rest of the system needs:
6	//! the [`ResolvedReference`] spans that link identifiers to IR entries. The tree
7	//! itself is transient — parsed, walked for references, and dropped.
8	
9	use std::{fs, path::PathBuf};
10	
11	use arborium_tree_sitter as tree_sitter;
12	use heart::Language;
13	use ir::syntax::{walk_references, ParseError, ResolvedReference};
14	
15	use crate::{error::GenerateError, generate::PackageInput, treesitter::classify_for};
16	
17	/// One source file's CST resolution: the resolved reference spans extracted from
18	/// it. Serializable and self-contained — no live tree.
19	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
20	pub struct Cst {
21		/// The file, relative to the package root.
22		pub path: PathBuf,
23		/// The references resolved within it, in source order.
24		pub references: Vec<ResolvedReference>,
25	}
26	
27	/// The per-file CST resolution for a whole package.
28	#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
29	pub struct CstSet {
30		/// One entry per parsed source file, sorted by path for reproducibility.
31		pub files: Vec<Cst>,
32	}
33	
34	/// Map a language + file extension to an arborium grammar name.
35	///
36	/// Returns `None` when the extension is not a source file for that language.
37	fn grammar_for_extension(lang: Language, ext: &str) -> Option<&'static str> {
38		match lang {
39			Language::Rust if ext == "rs" => Some("rust"),
40			Language::Python if ext == "py" => Some("python"),
41			Language::Typescript => match ext {
42				"ts" | "mts" | "cts" => Some("typescript"),
43				"tsx" => Some("tsx"),
44				// Optional JS companions of a TypeS
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/occurrences.rs
```
1	//! The occurrences pipeline stage: parse every source file, run its
2	//! [`LanguageSpec`](crate::treesitter::spec::LanguageSpec) extractor, and
3	//! resolve + attribute the results against the surface index into an
4	//! [`OccurrenceSet`] (REFERENCES-PLAN §4.3).
5	//!
6	//! This is the surface-downstream successor to [`super::cst`]: where `cst`
7	//! produced unqualified [`ResolvedReference`](ir::syntax::ResolvedReference)
8	//! spans, this produces fully-qualified [`Occurrence`](ir::syntax::Occurrence)s
9	//! keyed by `NudoxPath` + span + confidence. The live trees are transient —
10	//! parsed, walked, resolved, and dropped.
11	
12	use std::{fs, path::PathBuf};
13	
14	use arborium_tree_sitter as tree_sitter;
15	use heart::Language;
16	use ir::{entry::Index, syntax::OccurrenceSet};
17	
18	use crate::{
19		error::GenerateError,
20		generate::{resolve::resolve, PackageInput},
21		treesitter::{
22			spec::{Extraction, PackageLayout},
23			spec_for,
24		},
25	};
26	
27	/// Map a language + file extension to an arborium grammar name (identical to
28	/// the [`super::cst`] dispatch; kept here so the two stages can diverge).
29	fn grammar_for_extension(lang: Language, ext: &str) -> Option<&'static str> {
30		match lang {
31			Language::Rust if ext == "rs" => Some("rust"),
32			Language::Python if ext == "py" => Some("python"),
33			Language::Typescript => match ext {
34				"ts" | "mts" | "cts" => Some("typescript"),
35				"tsx" => Some("tsx"),
36				"js" => Some("javascript"),
37				"jsx" => Some("tsx"),
38				_ => None,
39			},
40			Language::Go if ext == "go" => Some("go"),
41			Language::Java if ext == "java" => Some("java"),
42			Language::Nix if ext == "nix" => Some("nix"),
43			Language::CSharp if ext == "cs" => Some("c-sharp"),
44			_ => None,
45		}
46	}
47	
48	/// Directory names that never hold first-party source (vendored deps, build
49	/// output, tool caches). Extends the `cst` skip list per REFERENCES-PLAN §5.
50	fn is_skipped_dir(name: &std::ffi::OsStr)
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/source_archive.rs
```
1	//! The condensed source resolution: a content-addressed set of the package's
2	//! source files.
3	//!
4	//! Rather than one opaque tar blob, each file is hashed individually (BLAKE3 via
5	//! [`heart::ContentHash`]) so the registry can content-address, dedupe across
6	//! versions, and serve ranged reads. A tar view is still producible on demand
7	//! (see [`super::tar`]); it is not the stored representation.
8	
9	use std::{fs, io::BufRead, path::{Path, PathBuf}};
10	
11	use heart::content::ContentHash;
12	
13	use crate::{error::GenerateError, generate::PackageInput};
14	
15	/// One source file, content-addressed. Mirrors the registry's `FileEntry`, so
16	/// the generated archive maps directly onto the stored blob manifest.
17	#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
18	pub struct FileDigest {
19	    /// The file path, relative to the package root.
20	    pub path: PathBuf,
21	
22	    /// The BLAKE3 hash of the file's exact bytes.
23	    pub hash: ContentHash,
24	
25	    /// The file size in bytes.
26	    pub size: u64,
27	}
28	
29	/// The content-addressed source archive: every source file's digest, sorted by
30	/// path for a reproducible manifest fingerprint.
31	#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
32	pub struct SourceArchive {
33	    /// The per-file digests, sorted by path.
34	    pub files: Vec<FileDigest>,
35	}
36	
37	/// Walk the package source, hashing each file as it is read (bounded memory —
38	/// files stream through the hasher, never all held at once) into a sorted set of
39	/// [`FileDigest`]s.
40	///
41	/// Runs on a blocking pool (filesystem + hashing are sync CPU/IO work).
42	pub fn build(input: &PackageInput) -> Result<SourceArchive, GenerateError> {
43	    let mut files = Vec::new();
44	    let mut pending = vec![input.root.clone()];
45	
46	    while let Some(dir) = pending.pop() {
47	        for entry in fs::read_dir(&dir)? {
48	            let entry = entr
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/resolve.rs
```
1	//! Resolution + attribution: the language-agnostic engine that turns a
2	//! package's per-file [`Extraction`]s into an [`OccurrenceSet`]
3	//! (REFERENCES-PLAN §4.2).
4	//!
5	//! For each use-site it runs the resolution ladder (shadow → rooted → import →
6	//! lexical scope → package-wide → suffix → unresolved), attaching a
7	//! [`Confidence`] tier. For each span it finds the innermost enclosing
8	//! definition (containment), yielding the syntactic FQN even when that
9	//! definition is not in the surface index (principle 4: attribution never
10	//! fails). Every definition also emits a `Definition` occurrence, giving IR
11	//! items declaration coordinates for free.
12	//!
13	//! Language specifics end at the `Extraction` boundary; the only per-language
14	//! knowledge here is folding the absolute markers (`crate`/`self`/`super`).
15	
16	use std::{ops::Range, path::PathBuf};
17	
18	use heart::Language;
19	use ir::{
20		entry::{Index, NudoxPath},
21		syntax::{Confidence, FileOccurrences, Occurrence, OccurrenceSet, ReferenceKind, ResolutionStats, Role},
22	};
23	
24	use crate::{
25		graph::symtab::SymbolTable,
26		treesitter::spec::{
27			DefKind, Extraction, ImportBinding, ImportSource, RawDefinition, RawReference, ReceiverShape,
28		},
29	};
30	
31	/// Bump when the ladder, marker folding, or attribution logic changes: it keys
32	/// the occurrences pipeline stage's CAS entry (REFERENCES-PLAN §4.3).
33	pub const RESOLVER_VERSION: &str = "occ-v1";
34	
35	/// Resolve and attribute every file's extraction into the package occurrence
36	/// corpus.
37	pub fn resolve(lang: Language, files: &[(PathBuf, Extraction)], index: &Index) -> OccurrenceSet {
38		let symtab = SymbolTable::build(index);
39		let mut stats = ResolutionStats::default();
40		let mut out = Vec::with_capacity(files.len());
41	
42		for (path, extraction) in files {
43			let file = FileResolver { lang, symtab: &symtab, extraction };
44			let occurrences = file.resolve_file(&mut stats);
45			out.push
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/treesitter/spec.rs
```
1	//! The `LanguageSpec` contract: structured extraction of definitions, imports,
2	//! and qualified references from a tree-sitter parse.
3	//!
4	//! This supersedes the flat `(kind, parent_kind)` classifiers (still in
5	//! [`super`] for the legacy snippet path) with output rich enough for the
6	//! resolver (REFERENCES-PLAN §4.1): definition nesting, full qualifier chains,
7	//! import binding structure, and method-call receiver shapes.
8	//!
9	//! Each language implements one [`LanguageSpec`]; the resolver (`generate::
10	//! resolve`) is language-agnostic and consumes only the [`Extraction`] boundary.
11	
12	use std::{ops::Range, path::Path};
13	
14	use arborium_tree_sitter as tree_sitter;
15	use ir::syntax::ReferenceKind;
16	
17	// ─── Definition sites ────────────────────────────────────────────────────────
18	
19	/// The syntactic kind of a definition site, so the resolver can normalize
20	/// enclosing-frame names per language (e.g. an `impl T` frame contributes `T`).
21	#[derive(Debug, Clone, PartialEq, Eq)]
22	pub enum DefKind {
23		/// A free function.
24		Function,
25		/// A method (a function with a receiver / bound to a type or class).
26		Method,
27		/// A named data type: struct, record, enum, class-as-data.
28		Type,
29		/// A trait / protocol / interface definition.
30		Trait,
31		/// An `impl`/extension frame. `of` is the implemented trait/protocol path
32		/// spelling when present (`impl Trait for T`), else `None` (inherent impl).
33		Impl { of: Option<String> },
34		/// A module / namespace / package frame (inline `mod`, class-as-namespace).
35		Module,
36		/// A class frame that contributes its name to nested members' FQNs
37		/// (Python/TS/Java). Distinct from [`DefKind::Type`] only where a language
38		/// needs both a data shape and a namespace frame.
39		Class,
40	}
41	
42	/// One named definition site, with nesting expressed via `parent`.
43	///
44	/// Body spans are properly nested by construction (they mirror the tree), so
45	/// the 
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/treesitter/mod.rs
```
1	//! Production tree-sitter CST extraction.
2	//!
3	//! Given a symbol's source and span, finds the enclosing function, extracts a
4	//! self-contained snippet, re-parses it to an s-expression, and resolves the
5	//! references within it. Falls back gracefully (full text, no CST) on an
6	//! unsupported language or a parse failure.
7	//!
8	//! The [`spec`] submodule and per-language extractors ([`rust`], [`go`], …)
9	//! implement the richer [`spec::LanguageSpec`] contract (definitions, imports,
10	//! qualified references) that the occurrence resolver consumes; the classifiers
11	//! below remain for the legacy snippet path.
12	
13	pub mod spec;
14	
15	pub mod csharp;
16	pub mod go;
17	pub mod java;
18	pub mod nix;
19	pub mod python;
20	pub mod rust;
21	pub mod typescript;
22	
23	use std::{ops::Range, path::PathBuf};
24	
25	use arborium_tree_sitter as tree_sitter;
26	use heart::Language;
27	use ir::{
28		entry::NudoxPath,
29		syntax::{ReferenceKind, ResolvedReference, walk_references},
30	};
31	use serde::{Deserialize, Deserializer, Serialize};
32	
33	/// The structural [`spec::LanguageSpec`] extractor for `lang`.
34	pub fn spec_for(lang: Language) -> &'static dyn spec::LanguageSpec {
35		match lang {
36			Language::Rust => &rust::RustSpec,
37			Language::Python => &python::PythonSpec,
38			Language::Typescript => &typescript::TypescriptSpec,
39			Language::Go => &go::GoSpec,
40			Language::Java => &java::JavaSpec,
41			Language::Nix => &nix::NixSpec,
42			Language::CSharp => &csharp::CSharpSpec,
43		}
44	}
45	
46	// ─── Serializable primitives ─────────────────────────────────────────────────
47	
48	/// Opaque tree-sitter representation of a source chunk as raw bytes.
49	///
50	/// The concrete format is the JSON-serialized [`TreesitterPayload`].
51	#[derive(Debug, Clone, Serialize, Deserialize)]
52	pub struct TreesitterRepr(pub Vec<u8>);
53	
54	/// A byte range within a source file or buffer.
55	///
56	/// Invariant: `start <= end`. Enforced on construction and 
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/package
```
EISDIR: illegal operation on a directory, read '/Users/philocalyst/Projects/Backend/workspace/heart/package'
```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/Cargo.toml
```
1	[package]
2	name = "ir"
3	edition.workspace = true
4	version.workspace = true
5	license.workspace = true
6	
7	[lints]
8	workspace = true
9	
10	[lib]
11	# Sources live directly in the crate root (no src/ subdirectory).
12	path = "lib.rs"
13	
14	[features]
15	default = ["facet", "serde"]
16	facet = ["dep:facet"]
17	serde = ["dep:serde"]
18	
19	[dependencies]
20	# Optional: only compiled when the corresponding feature is active.
21	facet = { version = "0.42", optional = true }
22	serde = { version = "1.0", optional = true }
23	
24	# Always required.
25	arborium_tree_sitter = { package = "arborium-tree-sitter", version = "2.18" }
26	rustc_hash = { package = "rustc-hash", version = "1.1" }
27	strum = { version = "0.26", features = ["derive"] }
28	thiserror = "1.0"
29	yoke = "0.8"
30	
31	[dev-dependencies]
32	arborium = { version = "2.18", features = ["lang-rust"] }
33	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/heart/package/coordinates.rs
```
1	use crate::{PackageId, RegistryOrigin, ecosystem::Language, identity::{Id, namespace}};
2	use serde::{Deserialize, Serialize};
3	use thiserror::Error;
4	
5	use super::PackageName;
6	use crate::PackageVersion;
7	
8	/// The address of a package.
9	#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
10	pub struct Coordinates {
11	    pub origin: RegistryOrigin,
12	    pub name: PackageName,
13	    pub version: PackageVersion,
14	}
15	
16	impl Coordinates {
17	    pub const fn ecosystem(&self) -> Language {
18	        self.name.ecosystem
19	    }
20	
21	    pub fn identity_bytes(&self) -> Vec<u8> {
22	        // Length-prefixing every part makes the encoding injective — no
23	        // crafted origin/name pair can collide with a different tuple's byte
24	        // stream; the trailing NUL is a readability seam, not the mechanism.
25	        let origin = self.origin.token();
26	        let version = self.version.canonical();
27	        [origin.as_bytes(), self.name.canonical().as_bytes(), version.as_bytes()]
28	            .into_iter()
29	            .fold(Vec::new(), |mut bytes, part| {
30	                bytes.extend_from_slice(&(part.len() as u64).to_le_bytes());
31	                bytes.extend_from_slice(part);
32	                bytes.push(0);
33	                bytes
34	            })
35	    }
36	
37	    pub fn id(&self) -> PackageId {
38	        Id::from_name(&namespace::PACKAGE, &self.identity_bytes())
39	    }
40	}
41	
42	/// Raised when name and version disagree on ecosystem.
43	#[derive(Debug, Error)]
44	#[error("coordinate ecosystem mismatch: name is {name}, version is {version}")]
45	pub struct CoordinateError {
46	    pub name: Language,
47	    pub version: Language,
48	}
49	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/linked_data/schema.rs
```
1	//! The JSON-LD schema/context for the emitted linked data.
2	//!
3	//! The document *shapes* are derived from the graph model (`crate::graph::model`)
4	//! via `terminusdb-schema-derive`, so this module does not hand-write a schema.
5	//! It provides the JSON-LD `@context` that binds the model's field names to the
6	//! graph store's vocabulary, and the base IRI prefixes documents resolve against.
7	
8	use std::collections::BTreeSet;
9	
10	use serde_json::{Value, json};
11	use terminusdb_schema::ToTDBSchema;
12	
13	use crate::graph::model;
14	
15	/// The base IRI every emitted document's `@id` is minted under.
16	pub const BASE_IRI: &str = "https://nudox.org/ir/";
17	
18	/// The JSON-LD `@context` for the emitted graph documents — the field→predicate
19	/// bindings the graph store ingests against. Field names resolve against
20	/// `@schema`; the class vocabulary rides along in `@metadata`, walked from the
21	/// model's derived schema trees so it can never drift from what [`super::emit`]
22	/// serializes.
23	pub fn context() -> Value {
24		// Every emitted document type roots one schema tree; the union (each tree
25		// pulls in its transitively reachable subdocument classes) is the whole
26		// vocabulary.
27		let mut classes = BTreeSet::new();
28		classes.extend(model::Package::to_schema_tree().iter().map(|s| s.class_name().clone()));
29		classes.extend(model::PackageVersion::to_schema_tree().iter().map(|s| s.class_name().clone()));
30		classes.extend(model::Symbol::to_schema_tree().iter().map(|s| s.class_name().clone()));
31		classes.extend(model::Implementation::to_schema_tree().iter().map(|s| s.class_name().clone()));
32		classes.extend(model::Reference::to_schema_tree().iter().map(|s| s.class_name().clone()));
33	
34		json!({
35			"@type": "@context",
36			"@base": BASE_IRI,
37			"@schema": format!("{BASE_IRI}schema#"),
38			"@metadata": {
39				"model_classes": classes.into_iter().collect::<Vec<_>>(),
40			},
41		})
42	}
43	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/generate/surface.rs
```
1	//! Generating the API surface resolution (the `ir::Index`) for a package.
2	//!
3	//! One dispatch path for all six languages (DAEMON-PLAN §2.3):
4	//!
5	//! ```text
6	//! PackageInput → seal → run_producer (cas.get → cage/worker → cas.put)
7	//! ```
8	
9	use heart::{Language, PackageVersion, RegistryOrigin};
10	use ir::{
11		entry::Index,
12		pipeline::{Collected, Ir},
13	};
14	
15	use crate::compile::producer::{self, ForgeContext, Producer, SandboxKey};
16	use crate::{
17		error::GenerateError,
18		generate::{PackageInput, parse_cache},
19		languages::{
20			csharp::CSharpProducer, go::GoProducer, java::JavaProducer, nix::NixProducer,
21			python::PythonProducer, rust::RustProducer, typescript::TypescriptProducer,
22		},
23	};
24	
25	/// Lower a package's source into the collected IR, dispatching on ecosystem.
26	pub fn collect<C: ForgeContext>(
27		ctx: &C,
28		input: &PackageInput,
29	) -> Result<Ir<Collected>, GenerateError> {
30		let out = run_producer(ctx, input)?;
31		Ok(Ir::from_entries(
32			out.index.entries_by_path.into_values().collect(),
33		))
34	}
35	
36	/// Lower a package's source into the indexed API surface (the `ir::Index`).
37	pub fn build<C: ForgeContext>(ctx: &C, input: &PackageInput) -> Result<Index, GenerateError> {
38		Ok(collect(ctx, input)?.index().into_index())
39	}
40	
41	/// The per-package [`SandboxKey`] for override resolution.
42	fn package_key(input: &PackageInput) -> SandboxKey {
43		SandboxKey::package(
44			input.coordinates.origin.token().into_owned(),
45			input.coordinates.name.original().to_string(),
46			input.coordinates.version.canonical(),
47		)
48	}
49	
50	/// Seal + run the producer for the package's language — the only surface dispatch.
51	///
52	/// [`producer::run_producer`] is the sole cache client: the sealed input's job
53	/// key drives `ctx.cas().get` (hit → decoded [`ProducerOutput`]) / miss (run,
54	/// then `put`). Scratch lives in [`producer::SealedPackage`] (RAII).
55	fn run_producer<C: ForgeContext>(

```

### Read: /Users/philocalyst/Projects/Backend/workspace/ir/pipeline/parse_common.rs
```
1	use crate::{parameter::{LiteralParameter, Parameter}, ty::Type};
2	
3	pub fn output_parameters_from_type(ty: Type) -> Option<Vec<Parameter>> {
4		Some(vec![Parameter::Literal(LiteralParameter {
5			name:          String::new(),
6			r#type:        Some(ty),
7			attributes:    None,
8			default_value: None,
9			description:   None,
10		})])
11	}
12	
13	pub fn parameter_link_key(prefix: &str, idx: usize, total: usize, name: &str) -> String {
14		if !name.is_empty() {
15			format!("{prefix}.{name}")
16		} else if total == 1 {
17			prefix.to_string()
18		} else {
19			format!("{prefix}.{idx}")
20		}
21	}
22	
```

### Read: /Users/philocalyst/Projects/Backend/workspace/compiler/graph/mod.rs
```
1	//! Graph-store model: the graph-native TerminusDB projection of the compiler IR.
2	//!
3	//! The IR (`ir` crate) is the working representation; these types are its
4	//! graph form for TerminusDB. `#[derive(TerminusDBModel)]` generates both the
5	//! schema (uploaded once) and the instance JSON-LD (per document), so there is
6	//! no hand-written schema emitter or serde glue — Rust is the source of truth.
7	//! Design rationale and the query cookbook live in GRAPH-ARCHITECTURE.md at
8	//! the repo root.
9	//!
10	//! - [`model`] holds the derived document types: [`model::Symbol`] nodes with
11	//!   real link edges (`member_of`/`implements`/`extends`/`mentions`/…), the
12	//!   inline [`model::Shape`] payload, and the reified
13	//!   [`model::Implementation`] / [`model::Reference`] relation nodes.
14	//! - [`link`] is the resolution pass: name-strings → symbol IRIs, total via
15	//!   stub minting.
16	//! - [`from_ir`] projects an indexed package into a [`from_ir::GraphCorpus`]
17	//!   (`from_ir::project`).
18	
19	pub mod from_ir;
20	pub mod link;
21	pub mod model;
22	pub mod symtab;
23	
```
