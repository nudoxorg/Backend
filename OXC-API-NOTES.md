# Vendored OXC 0.139.0 API Reference

Ground truth for the vendored oxc 0.139.0 crates used by the TypeScript extractor. Extracted from source at commit `69b2dfc1810e6ad9d5508ae9a20bc773ba5aa18d`.

**Note on `preserve_parens`**: Set to `false` via `ParseOptions { preserve_parens: false, ..ParseOptions::default() }`. Default is `true`.

---

## 1. Parser & ParserReturn (`oxc_parser`)

### Parser::new
```rust
pub fn new(allocator: &'a Allocator, source_text: &'a str, source_type: SourceType) -> Self
```
Creates a new parser with a memory arena, source code, and source type (JS/TS/JSX/ESM/Script).

### Parser::with_options
```rust
pub fn with_options(mut self, options: ParseOptions) -> Self
```
Sets parsing options on the builder pattern.

### Parser::parse
```rust
pub fn parse(self) -> ParserReturn<'a>
```
Main entry point. Returns a `ParserReturn` containing the AST, diagnostics, and module metadata.

### ParseOptions
```rust
pub struct ParseOptions {
    // If true, emit ParenthesizedExpression and TSParenthesizedType nodes
    pub preserve_parens: bool,                       // default: true
    
    // Allow return statements outside functions (CommonJS files)
    pub allow_return_outside_function: bool,        // default: false
    
    // Parse regular expressions (feature-gated)
    #[cfg(feature = "regular_expression")]
    pub parse_regular_expression: bool,             // default: false
    
    // Allow V8 runtime calls like %DebugPrint()
    pub allow_v8_intrinsics: bool,                  // default: false
}
```

### ParserReturn
```rust
pub struct ParserReturn<'a> {
    /// The parsed AST (always valid structure, may have semantic errors)
    pub program: Program<'a>,

    /// ECMAScript Module Record (see §2)
    pub module_record: ModuleRecord<'a>,

    /// Syntax errors encountered (list not comprehensive; semantic errors found by analyzer)
    pub diagnostics: Diagnostics,

    /// Irregular unicode whitespaces
    pub irregular_whitespaces: Box<[Span]>,

    /// Lexed tokens in source order (only when tokens enabled in config)
    pub tokens: ArenaVec<'a, Token>,

    /// Parser panicked and terminated early (program will be empty)
    pub panicked: bool,

    /// Whether file is Flow language
    pub is_flow_language: bool,
}
```

---

## 2. ModuleRecord (`oxc_syntax::module_record`)

### ModuleRecord fields
```rust
pub struct ModuleRecord<'a> {
    /// This module has ESM syntax: `import` and `export`
    pub has_module_syntax: bool,

    /// `[[RequestedModules]]`: all module specifier strings in source order
    /// Keyed by ModuleSpecifier, valued by node occurrences
    pub requested_modules: ArenaHashMap<'a, Str<'a>, ArenaVec<'a, RequestedModule>>,

    /// `[[ImportEntries]]`: ImportEntry records derived from import statements
    pub import_entries: ArenaVec<'a, ImportEntry<'a>>,

    /// `[[LocalExportEntries]]`: ExportEntry records for local declarations
    pub local_export_entries: ArenaVec<'a, ExportEntry<'a>>,

    /// `[[IndirectExportEntries]]`: ExportEntry records for re-exported imports
    pub indirect_export_entries: ArenaVec<'a, ExportEntry<'a>>,

    /// `[[StarExportEntries]]`: ExportEntry records for `export *` (not `export * as ns`)
    pub star_export_entries: ArenaVec<'a, ExportEntry<'a>>,

    /// Local exported bindings: name -> span
    pub exported_bindings: ArenaHashMap<'a, Str<'a>, Span>,

    /// Dynamic import expressions: `import(specifier)`
    pub dynamic_imports: ArenaVec<'a, DynamicImport>,

    /// Span positions of `import.meta`
    pub import_metas: ArenaVec<'a, Span>,
}
```

### ImportEntry
```rust
pub struct ImportEntry<'a> {
    /// Span of the import statement
    pub statement_span: Span,

    /// Module request: the string after `from` (e.g. `"mod"` in `import { x } from "mod"`)
    pub module_request: NameSpan<'a>,

    /// The name exported by the module (e.g. `"foo"` in `import { foo as bar }`)
    pub import_name: ImportImportName<'a>,

    /// The name locally used (e.g. `"bar"` in `import { foo as bar }`)
    pub local_name: NameSpan<'a>,

    /// TypeScript type-only import flag (true for `import type { foo }` or `import { type foo }`)
    pub is_type: bool,
}
```

### ImportImportName
```rust
pub enum ImportImportName<'a> {
    Name(NameSpan<'a>),           // `import { x }`
    NamespaceObject,              // `import * as ns`
    Default(Span),                // default export
}
```

### ExportEntry
```rust
pub struct ExportEntry<'a> {
    /// Span of the export statement
    pub statement_span: Span,

    /// Span of the entire export entry
    pub span: Span,

    /// Module request (null for local exports)
    pub module_request: Option<NameSpan<'a>>,

    /// Import name from the module ("all" for `export * as ns`, "all-but-default" for `export *`)
    pub import_name: ExportImportName<'a>,

    /// Name used to export this binding (e.g. `"bar"` in `export { foo as bar }`)
    pub export_name: ExportExportName<'a>,

    /// Local name (null for re-exports)
    pub local_name: ExportLocalName<'a>,

    /// TypeScript `export type` flag
    pub is_type: bool,
}
```

### ExportImportName
```rust
pub enum ExportImportName<'a> {
    Name(NameSpan<'a>),           // Named re-export
    All,                          // `export * as ns`
    AllButDefault,                // `export *`
    Null,                         // No ModuleSpecifier
}
```

### ExportExportName
```rust
pub enum ExportExportName<'a> {
    Name(NameSpan<'a>),           // Named export
    Default(Span),                // `export default`
    Null,                         // No export name
}
```

### ExportLocalName
```rust
pub enum ExportLocalName<'a> {
    Name(NameSpan<'a>),           // Named local binding
    Default(NameSpan<'a>),        // `export default <expr>`
    Null,                         // Not locally accessible
}
```

### NameSpan
```rust
pub struct NameSpan<'a> {
    pub name: Str<'a>,            // The identifier
    pub span: Span,               // Source location
}
```

### RequestedModule
```rust
pub struct RequestedModule {
    pub statement_span: Span,     // Import/export statement span
    pub span: Span,               // Module specifier span
    pub is_type: bool,            // `type` modifier on module request
    pub is_import: bool,          // From `import` vs `export` statement
}
```

---

## 3. Semantic / Scoping (`oxc_semantic`)

### SemanticBuilder
```rust
impl<'a> SemanticBuilder<'a> {
    /// Create with defaults (minimal setup)
    pub fn new() -> Self

    /// Create for compiler (syntax error checking, no full AstNodes store)
    pub fn new_compiler() -> Self

    /// Create for linter (full AstNodes, CFG, class table, syntax checking)
    pub fn new_linter() -> Self

    /// Enable/disable syntax error checking
    pub fn with_check_syntax_error(mut self, yes: bool) -> Self

    /// Enable/disable building the full AstNodes store (for random access)
    pub fn with_build_nodes(mut self, yes: bool) -> Self

    /// Enable/disable TypeScript enum member evaluation
    pub fn with_enum_eval(mut self, yes: bool) -> Self

    /// Build the semantic analysis
    pub fn build(self, program: &'a Program<'a>) -> SemanticBuilderReturn<'a>
}

pub struct SemanticBuilderReturn<'a> {
    pub semantic: Semantic<'a>,
    pub diagnostics: Diagnostics,
}
```

### Semantic accessors
```rust
impl<'a> Semantic<'a> {
    pub fn nodes(&self) -> &AstNodes<'a>          // Full AST node store (if built)
    pub fn scoping(&self) -> &Scoping             // Symbol/scope resolution
    pub fn scoping_mut(&mut self) -> &mut Scoping // Mutable access
    pub fn jsdoc(&self) -> &JSDocFinder<'a>       // JSDoc comments (feature-gated)
    pub fn source_text(&self) -> &'a str
    pub fn source_type(&self) -> &SourceType
    pub fn comments(&self) -> &[Comment]
    pub fn symbol_declaration(&self, symbol_id: SymbolId) -> &AstNode<'a>
    pub fn symbol_references(&self, symbol_id: SymbolId) -> impl Iterator<Item = &Reference>
}
```

### Scoping API
```rust
impl Scoping {
    /// Get all symbol IDs (iterator)
    pub fn symbol_ids(&self) -> impl Iterator<Item = SymbolId>

    /// Get all scope IDs
    pub fn scope_ids(&self) -> impl Iterator<Item = ScopeId>

    /// Get a reference by ReferenceId
    pub fn get_reference(&self, reference_id: ReferenceId) -> Option<&Reference>

    /// Look up a symbol binding in a scope by name
    pub fn get_binding(&self, scope_id: ScopeId, name: Ident<'a>) -> Option<SymbolId>

    /// Get all references resolved to a symbol
    pub fn get_resolved_references(&self, symbol_id: SymbolId) -> impl Iterator<Item = &Reference>

    /// Get the scope a symbol is declared in
    pub fn symbol_scope_id(&self, symbol_id: SymbolId) -> ScopeId

    /// Get the declaration node of a symbol
    pub fn symbol_declaration(&self, symbol_id: SymbolId) -> NodeId

    /// Get unresolved references (global/undefined)
    pub fn root_unresolved_references(&self) -> &UnresolvedReferences

    /// Root scope ID
    pub fn root_scope_id(&self) -> ScopeId

    /// Number of scopes
    pub fn scopes_len(&self) -> usize

    /// Number of symbols
    pub fn symbols_len(&self) -> usize
}
```

### Reference
```rust
pub struct Reference {
    // From oxc_syntax::reference
    // Fields:
    //   node_id: NodeId,        // The IdentifierReference AST node
    //   symbol_id: Option<SymbolId>,  // Resolved symbol (None = unresolved)
    //   flags: ReferenceFlags,  // read/write/type-only, etc.
}

impl Reference {
    pub fn node_id(&self) -> NodeId
    pub fn symbol_id(&self) -> Option<SymbolId>
    pub fn flags(&self) -> ReferenceFlags
}
```

### IdentifierReference / TSTypeName
Both expose their reference via AST fields:
- `IdentifierReference` (JS expressions): has a `reference_id` in semantic context
- `TSTypeName` (type references): similarly has reference information

Query via:
```rust
let reference_id = identifier_ref.reference_id;  // From semantic.nodes() lookup
let reference = semantic.scoping().get_reference(reference_id)?;
```

### AstNodes / AstNode / AstKind
```rust
pub struct AstNodes<'a> {
    // Random-access node store (built when with_build_nodes(true))
}

impl AstNodes<'a> {
    pub fn get_node(&self, node_id: NodeId) -> &AstNode<'a>
    pub fn iter(&self) -> impl Iterator<Item = &AstNode<'a>>
}

pub struct AstNode<'a> {
    pub kind: AstKind<'a>,  // Discriminated union of all AST node types
}

pub enum AstKind<'a> {
    // Discriminants for every AST node variant
    // e.g. IdentifierReference(&'a IdentifierReference<'a>)
}

impl AstKind<'a> {
    pub fn span(&self) -> Span  // Get span of any node
}
```

### SymbolFlags (from oxc_syntax::symbol)
```rust
pub struct SymbolFlags: u32 {
    // Bit flags for symbol properties
    // Common flags:
    // - Variable / Function / Class / Import / Export
    // - TypeScript: Type, Enum, Interface, TypeAlias, Namespace, etc.
    // - Modifiers: Const, Default, etc.
}
```

---

## 4. JSDoc (`oxc_jsdoc` via `semantic.jsdoc()`)

### JSDocFinder (accessed via Semantic::jsdoc())
```rust
pub struct JSDocFinder<'a> {
    // Opaque; accessed via methods
}

impl<'a> JSDocFinder<'a> {
    /// Get JSDoc comment(s) attached to a node by its start position
    pub fn get_one_by_node(&self, node_id: NodeId) -> Option<&JSDoc<'a>>
    pub fn get_all_by_node(&self, node_id: NodeId) -> Vec<&JSDoc<'a>>
}
```

### JSDoc
```rust
pub struct JSDoc<'a> {
    // From oxc_jsdoc::parser
    // Exposes parsed JSDoc comment with tags
}

impl<'a> JSDoc<'a> {
    /// Raw comment text (excluding /** */)
    pub fn comment(&self) -> &str

    /// Parsed tags
    pub fn tags(&self) -> impl Iterator<Item = &JSDocTag<'a>>
}
```

### JSDocTag
```rust
pub struct JSDocTag<'a> {
    pub kind: JSDocTagKind,    // @param, @returns, @type, etc.
    // ... other fields
}

impl<'a> JSDocTag<'a> {
    pub fn parsed(&self) -> Option<&JSDocTagParsed<'a>>  // Parsed tag data
    pub fn comment(&self) -> Option<&str>                 // Tag's comment text
    pub fn type_name_comment(&self) -> Option<&str>       // For @param, @returns
}
```

### Comment (for manual module doc scan)
Accessible via `semantic.comments()` or `program.comments`:
```rust
pub struct Comment {
    pub span: Span,
    pub kind: CommentKind,  // Line or Block

    // From AST:
    pub attached_to: u32,   // NodeId the comment is attached to (or u32::MAX if unattached)
}

impl Comment {
    pub fn is_jsdoc(&self) -> bool  // Starts with /**
}
```

---

## 5. oxc_ast Declaration Nodes (`crates/oxc_ast/src/ast/{js.rs,ts.rs}`)

### Declaration enum
Variants in `Statement`:
```rust
pub enum Statement<'a> {
    // Declarations:
    Declaration(Box<'a, Declaration<'a>>),
    // ... other statement types
}

pub enum Declaration<'a> {
    FunctionDeclaration(Box<'a, Function<'a>>),
    ClassDeclaration(Box<'a, Class<'a>>),
    VariableDeclaration(Box<'a, VariableDeclaration<'a>>),
    TSTypeAliasDeclaration(Box<'a, TSTypeAliasDeclaration<'a>>),
    TSInterfaceDeclaration(Box<'a, TSInterfaceDeclaration<'a>>),
    TSEnumDeclaration(Box<'a, TSEnumDeclaration<'a>>),
    TSModuleDeclaration(Box<'a, TSModuleDeclaration<'a>>),
    // ... ImportDeclaration, ExportNamedDeclaration, ExportDefaultDeclaration, etc.
}
```

### Function
```rust
pub struct Function<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub id: Option<BindingIdentifier<'a>>,      // Function name
    pub params: Box<'a, FormalParameters<'a>>,  // Parameters
    pub body: Option<Box<'a, BlockStatement<'a>>>,  // Function body
    pub return_type: Option<Box<'a, TSTypeAnnotation<'a>>>,  // Return type
    pub type_parameters: Option<Box<'a, TSTypeParameterDeclaration<'a>>>,
    pub decorators: Vec<'a, Decorator<'a>>,
    pub r#async: bool,                          // `async` keyword
    pub generator: bool,                        // `*` (generator)
    pub declare: bool,                          // `declare` (TS)
    // Fields have mutually exclusive interpretations; see flags below
}

// Flags (check these via instanceof):
// - r#async: function is async
// - generator: function is a generator
// - declare: TypeScript declare function
```

### Class
```rust
pub struct Class<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub id: Option<BindingIdentifier<'a>>,      // Class name
    pub params: Vec<'a, TsClassImplements<'a>>, // Implements clauses
    pub body: Box<'a, ClassBody<'a>>,           // Class body
    pub extends: Option<Box<'a, Expression<'a>>>,  // Extends clause
    pub decorators: Vec<'a, Decorator<'a>>,
    pub type_parameters: Option<Box<'a, TSTypeParameterDeclaration<'a>>>,
    pub declare: bool,                          // `declare class`
    pub r#abstract: bool,                       // `abstract class`
    // scope_id: Cell<Option<ScopeId>>,          // Computed by semantic analysis
}
```

### ClassElement / MethodDefinition / PropertyDefinition
```rust
pub enum ClassElement<'a> {
    MethodDefinition(Box<'a, MethodDefinition<'a>>),
    PropertyDefinition(Box<'a, PropertyDefinition<'a>>),
    AccessorProperty(Box<'a, AccessorProperty<'a>>),
    StaticBlock(Box<'a, StaticBlock<'a>>),
    TSIndexSignature(Box<'a, TSIndexSignature<'a>>),
}

pub struct MethodDefinition<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub key: PropertyKey<'a>,
    pub value: Box<'a, Function<'a>>,           // Method function
    pub kind: MethodDefinitionKind,             // Method, Get, Set, Constructor
    pub computed: bool,
    pub r#static: bool,
    pub decorators: Vec<'a, Decorator<'a>>,
    // Optional flags:
    pub accessibility: Option<TSAccessibility>, // Private, Protected, Public (TS)
    pub r#override: bool,                       // `override` (TS)
}

pub struct PropertyDefinition<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub key: PropertyKey<'a>,
    pub value: Option<Box<'a, Expression<'a>>>,
    pub computed: bool,
    pub r#static: bool,
    pub r#override: bool,                       // `override` (TS)
    pub readonly: bool,
    pub declare: bool,
    pub optional: bool,
    pub decorators: Vec<'a, Decorator<'a>>,
    pub accessibility: Option<TSAccessibility>,
    pub type_annotation: Option<Box<'a, TSTypeAnnotation<'a>>>,
}
```

### FormalParameter / FormalParameters
```rust
pub struct FormalParameters<'a> {
    pub span: Span,
    pub items: Vec<'a, FormalParameter<'a>>,
    pub rest: Option<Box<'a, RestElement<'a>>>,
}

pub struct FormalParameter<'a> {
    pub span: Span,
    pub pattern: BindingPattern<'a>,
    pub accessibility: Option<TSAccessibility>,  // TS only
    pub decorators: Vec<'a, Decorator<'a>>,
    pub type_annotation: Option<Box<'a, TSTypeAnnotation<'a>>>,
    pub optional: bool,
    pub r#override: bool,                        // TS only
}
```

### BindingPattern / BindingPatternKind
```rust
pub struct BindingPattern<'a> {
    pub span: Span,
    pub kind: BindingPatternKind<'a>,
}

pub enum BindingPatternKind<'a> {
    BindingIdentifier(Box<'a, BindingIdentifier<'a>>),
    ObjectPattern(Box<'a, ObjectPattern<'a>>),
    ArrayPattern(Box<'a, ArrayPattern<'a>>),
    // ... destructuring patterns
}
```

### TSInterfaceDeclaration
```rust
pub struct TSInterfaceDeclaration<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub id: BindingIdentifier<'a>,              // Interface name
    pub type_parameters: Option<Box<'a, TSTypeParameterDeclaration<'a>>>,
    pub extends: Vec<'a, TSInterfaceHeritage<'a>>,  // Extends clauses
    pub body: Box<'a, TSInterfaceBody<'a>>,    // Members
    pub declare: bool,
    // scope_id: Cell<Option<ScopeId>>,
}

pub struct TSInterfaceBody<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub body: Vec<'a, TSSignature<'a>>,        // Members (properties, methods, index signatures)
}
```

### TSSignature (5 variants)
```rust
pub enum TSSignature<'a> {
    TSIndexSignature(Box<'a, TSIndexSignature<'a>>),         // [key: type]: type
    TSPropertySignature(Box<'a, TSPropertySignature<'a>>),   // key: type
    TSCallSignatureDeclaration(Box<'a, TSCallSignatureDeclaration<'a>>),  // (params): type
    TSConstructSignatureDeclaration(Box<'a, TSConstructSignatureDeclaration<'a>>),  // new (params): type
    TSMethodSignature(Box<'a, TSMethodSignature<'a>>),       // key(params): type | get/set
}
```

### TSEnumDeclaration / TSEnumMember
```rust
pub struct TSEnumDeclaration<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub id: BindingIdentifier<'a>,
    pub body: TSEnumBody<'a>,
    pub r#const: bool,                          // `const enum`
    pub declare: bool,
}

pub struct TSEnumBody<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub members: Vec<'a, TSEnumMember<'a>>,
    // scope_id: Cell<Option<ScopeId>>,
}

pub struct TSEnumMember<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub id: TSEnumMemberName<'a>,               // Identifier, String, or computed
    pub initializer: Option<Expression<'a>>,    // Numeric or string value
}
```

### TSTypeAliasDeclaration
```rust
pub struct TSTypeAliasDeclaration<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub id: BindingIdentifier<'a>,              // Type alias name
    pub type_parameters: Option<Box<'a, TSTypeParameterDeclaration<'a>>>,
    pub type_annotation: TSType<'a>,            // The type
    pub declare: bool,
    // scope_id: Cell<Option<ScopeId>>,
}
```

### TSModuleDeclaration / TSModuleDeclarationBody
```rust
pub struct TSModuleDeclaration<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub id: TSModuleDeclarationName<'a>,        // Identifier or StringLiteral
    pub body: Option<TSModuleDeclarationBody<'a>>,  // Nested module or block
    pub kind: TSModuleDeclarationKind,          // Module or Namespace
    pub declare: bool,
    // scope_id: Cell<Option<ScopeId>>,
}

pub enum TSModuleDeclarationKind {
    Module = 0,   // `module` or `declare module 'string'`
    Namespace = 1,  // `namespace`
}

pub enum TSModuleDeclarationBody<'a> {
    TSModuleDeclaration(Box<'a, TSModuleDeclaration<'a>>),  // Nested module
    TSModuleBlock(Box<'a, TSModuleBlock<'a>>),              // Block of statements
}
```

### VariableDeclaration / VariableDeclarator
```rust
pub struct VariableDeclaration<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub kind: VariableDeclarationKind,          // Var, Let, Const
    pub declarations: Vec<'a, VariableDeclarator<'a>>,
    pub declare: bool,                          // TypeScript `declare var`
}

pub enum VariableDeclarationKind {
    Var = 0,
    Let = 1,
    Const = 2,
}

pub struct VariableDeclarator<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub id: BindingPattern<'a>,                 // Name/destructuring
    pub init: Option<Box<'a, Expression<'a>>>, // Initializer
    pub definite: bool,                         // TypeScript definite assignment `!`
}
```

### TSTypeParameter / TSTypeParameterDeclaration
```rust
pub struct TSTypeParameter<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub name: BindingIdentifier<'a>,            // Type parameter name (e.g., `T`)
    pub constraint: Option<TSType<'a>>,         // `extends` type
    pub default: Option<TSType<'a>>,            // Default type
    pub r#in: bool,                             // `in` variance modifier
    pub out: bool,                              // `out` variance modifier
    pub r#const: bool,                          // `const` modifier (TS 5.0)
}

pub struct TSTypeParameterDeclaration<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub params: Vec<'a, TSTypeParameter<'a>>,
}
```

### Decorator
```rust
pub struct Decorator<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub expression: Expression<'a>,             // The decorator function/identifier
}
```

### TSAccessibility
```rust
pub enum TSAccessibility {
    Private = 0,
    Protected = 1,
    Public = 2,
}
```

### Common flags (on declarations)
Raw fields in Rust struct:
- `declare: bool` – TypeScript `declare` keyword
- `r#async: bool` – `async` keyword (functions)
- `generator: bool` – `*` generator (functions)
- `r#abstract: bool` – `abstract` (classes/methods)
- `r#static: bool` – `static` (class members)
- `readonly: bool` – `readonly` (properties, index signatures)
- `optional: bool` – `?` (function parameters, properties)
- `r#override: bool` – TypeScript `override`
- `r#const: bool` – TypeScript `const` (enums, type parameters)
- `r#in: bool` – TypeScript variance `in`
- `r#out: bool` – TypeScript variance `out`
- `computed: bool` – `[key]` computed member name

---

## 6. oxc_ast TSType (~37 variants)

### Full TSType enum
```rust
pub enum TSType<'a> {
    // Keyword types
    TSAnyKeyword(Box<'a, TSAnyKeyword>),
    TSBigIntKeyword(Box<'a, TSBigIntKeyword>),
    TSBooleanKeyword(Box<'a, TSBooleanKeyword>),
    TSIntrinsicKeyword(Box<'a, TSIntrinsicKeyword>),
    TSNeverKeyword(Box<'a, TSNeverKeyword>),
    TSNullKeyword(Box<'a, TSNullKeyword>),
    TSNumberKeyword(Box<'a, TSNumberKeyword>),
    TSObjectKeyword(Box<'a, TSObjectKeyword>),
    TSStringKeyword(Box<'a, TSStringKeyword>),
    TSSymbolKeyword(Box<'a, TSSymbolKeyword>),
    TSUndefinedKeyword(Box<'a, TSUndefinedKeyword>),
    TSUnknownKeyword(Box<'a, TSUnknownKeyword>),
    TSVoidKeyword(Box<'a, TSVoidKeyword>),
    TSThisType(Box<'a, TSThisType>),

    // Compound/constructed types
    TSArrayType(Box<'a, TSArrayType<'a>>),                // Type[]
    TSConditionalType(Box<'a, TSConditionalType<'a>>),    // T extends U ? X : Y
    TSConstructorType(Box<'a, TSConstructorType<'a>>),    // new (params) => Type
    TSFunctionType(Box<'a, TSFunctionType<'a>>),          // (params) => Type
    TSImportType(Box<'a, TSImportType<'a>>),              // import("mod").Type
    TSIndexedAccessType(Box<'a, TSIndexedAccessType<'a>>), // T[K]
    TSInferType(Box<'a, TSInferType<'a>>),                // infer T
    TSIntersectionType(Box<'a, TSIntersectionType<'a>>),  // A & B
    TSLiteralType(Box<'a, TSLiteralType<'a>>),            // "string", 42, true
    TSMappedType(Box<'a, TSMappedType<'a>>),              // { [K in T]: ... }
    TSNamedTupleMember(Box<'a, TSNamedTupleMember<'a>>),  // [name: Type]
    TSTemplateLiteralType(Box<'a, TSTemplateLiteralType<'a>>),  // `template-${...}`
    TSTupleType(Box<'a, TSTupleType<'a>>),                // [Type1, Type2, ...]
    TSTypeLiteral(Box<'a, TSTypeLiteral<'a>>),            // { key: Type; ... }
    TSTypeOperatorType(Box<'a, TSTypeOperator<'a>>),      // keyof T, readonly T, unique T
    TSTypePredicate(Box<'a, TSTypePredicate<'a>>),        // (x: U): x is T
    TSTypeQuery(Box<'a, TSTypeQuery<'a>>),                // typeof x, typeof import(...)
    TSTypeReference(Box<'a, TSTypeReference<'a>>),        // Foo, Foo<T>
    TSUnionType(Box<'a, TSUnionType<'a>>),                // A | B
    TSParenthesizedType(Box<'a, TSParenthesizedType<'a>>), // (Type)

    // JSDoc types
    JSDocNullableType(Box<'a, JSDocNullableType<'a>>),
    JSDocNonNullableType(Box<'a, JSDocNonNullableType<'a>>),
    JSDocUnknownType(Box<'a, JSDocUnknownType>),
}
```

### Key type structures

#### TSTypeName
```rust
pub enum TSTypeName<'a> {
    IdentifierReference(Box<'a, IdentifierReference<'a>>),  // Foo
    QualifiedName(Box<'a, TSQualifiedName<'a>>),            // Foo.Bar
    ThisExpression(Box<'a, ThisExpression>),                // this
}
```

#### TSTupleElement
```rust
pub enum TSTupleElement<'a> {
    TSOptionalType(Box<'a, TSOptionalType<'a>>),            // Type?
    TSRestType(Box<'a, TSRestType<'a>>),                    // ...Type
    // TSType variants inherited via #[ast] macro
    INHERIT(TSType<'a>),
}
```

#### TSLiteral
```rust
pub enum TSLiteral<'a> {
    BooleanLiteral(Box<'a, BooleanLiteral>),
    NumericLiteral(Box<'a, NumericLiteral<'a>>),
    BigIntLiteral(Box<'a, BigIntLiteral<'a>>),
    StringLiteral(Box<'a, StringLiteral<'a>>),
    TemplateLiteral(Box<'a, TemplateLiteral<'a>>),
    UnaryExpression(Box<'a, UnaryExpression<'a>>),          // For negative numbers
}
```

#### TSMappedType
```rust
pub struct TSMappedType<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub key: BindingIdentifier<'a>,             // `P` in `[P in keyof T]`
    pub constraint: TSType<'a>,                 // `keyof T`
    pub name_type: Option<TSType<'a>>,          // `as` clause (remapping)
    pub type_annotation: Option<TSType<'a>>,    // The member type
    pub optional: Option<TSMappedTypeModifierOperator>,  // `?`, `+?`, `-?`
    pub readonly: Option<TSMappedTypeModifierOperator>,  // `readonly`, `+readonly`, `-readonly`
    // scope_id: Cell<Option<ScopeId>>,
}

pub enum TSMappedTypeModifierOperator {
    True = 0,   // `?` or `readonly`
    Plus = 1,   // `+?` or `+readonly`
    Minus = 2,  // `-?` or `-readonly`
}
```

#### TSConditionalType
```rust
pub struct TSConditionalType<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub check_type: TSType<'a>,                 // T
    pub extends_type: TSType<'a>,               // extends U
    pub true_type: TSType<'a>,                  // ? TrueType
    pub false_type: TSType<'a>,                 // : FalseType
    // scope_id: Cell<Option<ScopeId>>,
}
```

#### TSIndexedAccessType
```rust
pub struct TSIndexedAccessType<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub object_type: TSType<'a>,                // T
    pub index_type: TSType<'a>,                 // K
}
```

#### TSTypeOperator
```rust
pub struct TSTypeOperator<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub operator: TSTypeOperatorOperator,       // keyof, unique, readonly
    pub type_annotation: TSType<'a>,
}

pub enum TSTypeOperatorOperator {
    Keyof = 0,
    Unique = 1,
    Readonly = 2,
}
```

#### TSTypeQuery
```rust
pub struct TSTypeQuery<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub expr_name: TSTypeQueryExprName<'a>,     // typeof Foo or typeof import(...)
    pub type_arguments: Option<Box<'a, TSTypeParameterInstantiation<'a>>>,
}
```

#### TSTypePredicate
```rust
pub struct TSTypePredicate<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub parameter_name: TSTypePredicateName<'a>, // x or this
    pub asserts: bool,                           // `asserts` keyword present?
    pub type_annotation: Option<Box<'a, TSTypeAnnotation<'a>>>,  // is Type
}
```

#### TSImportType
```rust
pub struct TSImportType<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub source: StringLiteral<'a>,              // import('foo')
    pub options: Option<Box<'a, ObjectExpression<'a>>>,  // Assertions: { assert: { type: 'json' } }
    pub qualifier: Option<TSImportTypeQualifier<'a>>,    // .foo.bar qualifier
    pub type_arguments: Option<Box<'a, TSTypeParameterInstantiation<'a>>>,  // <T>
}
```

#### TSTemplateLiteralType
```rust
pub struct TSTemplateLiteralType<'a> {
    pub node_id: Cell<NodeId>,
    pub span: Span,
    pub quasis: Vec<'a, TemplateLiteral<'a>>,  // Template parts
    pub types: Vec<'a, TSType<'a>>,            // Type placeholders
}
```

---

## 7. oxc_ast Expression (for inference, OXC-PLAN §1.2)

### Expression enum (relevant variants)
```rust
pub enum Expression<'a> {
    // Literals
    BooleanLiteral(Box<'a, BooleanLiteral>),
    NumericLiteral(Box<'a, NumericLiteral<'a>>),
    BigIntLiteral(Box<'a, BigIntLiteral<'a>>),
    StringLiteral(Box<'a, StringLiteral<'a>>),
    TemplateLiteral(Box<'a, TemplateLiteral<'a>>),
    RegExpLiteral(Box<'a, RegExpLiteral<'a>>),
    NullLiteral(Box<'a, NullLiteral>),

    // Identifiers & References
    Identifier(Box<'a, IdentifierReference<'a>>),

    // Functions
    FunctionExpression(Box<'a, Function<'a>>),
    ArrowFunctionExpression(Box<'a, ArrowFunctionExpression<'a>>),

    // Objects & Arrays
    ObjectExpression(Box<'a, ObjectExpression<'a>>),
    ArrayExpression(Box<'a, ArrayExpression<'a>>),

    // Type-related expressions
    TSAsExpression(Box<'a, TSAsExpression<'a>>),          // x as Type
    TSSatisfiesExpression(Box<'a, TSSatisfiesExpression<'a>>),  // x satisfies Type
    TSNonNullExpression(Box<'a, TSNonNullExpression<'a>>),      // x!

    // Control flow
    ConditionalExpression(Box<'a, ConditionalExpression<'a>>),  // x ? y : z
    BinaryExpression(Box<'a, BinaryExpression<'a>>),    // x + y, x && y
    UnaryExpression(Box<'a, UnaryExpression<'a>>),      // -x, !x, typeof x, void x, delete x

    // Updates
    UpdateExpression(Box<'a, UpdateExpression<'a>>),    // ++x, x++

    // Async/await
    AwaitExpression(Box<'a, AwaitExpression<'a>>),      // await x

    // Other
    NewExpression(Box<'a, NewExpression<'a>>),          // new Foo()
    CallExpression(Box<'a, CallExpression<'a>>),
    MemberExpression(Box<'a, MemberExpression<'a>>),
    // ... many more
}
```

### Getting source text from span
```rust
// Span methods:
pub struct Span {
    pub start: u32,  // Byte offset in source
    pub end: u32,
}

impl Span {
    pub fn source_text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start as usize..self.end as usize]
    }
}

// Usage:
let expr_text = expr_span.source_text(semantic.source_text());
```

---

## 8. oxc_isolated_declarations & oxc_resolver

### IsolatedDeclarations
```rust
use oxc_isolated_declarations::{IsolatedDeclarations, IsolatedDeclarationsOptions};

pub struct IsolatedDeclarations<'a> {
    // Opaque
}

impl<'a> IsolatedDeclarations<'a> {
    pub fn new(allocator: &'a Allocator, options: IsolatedDeclarationsOptions) -> Self

    pub fn build(&self, program: &Program<'a>) -> IsolatedDeclarationsReturn<'a>
}

pub struct IsolatedDeclarationsOptions {
    pub strip_internal: bool,  // Strip `@internal` JSDoc
}

pub struct IsolatedDeclarationsReturn<'a> {
    pub program: Program<'a>,  // Transformed program with isolated .d.ts declarations
    pub errors: Diagnostics,
}
```

### Resolver & ResolveOptions
```rust
use oxc_resolver::{Resolver, ResolveOptions, Resolution, ResolveError, ResolveContext};

pub struct Resolver {
    // Opaque
}

impl Resolver {
    pub fn new(options: ResolveOptions) -> Self
    pub fn resolve<P: AsRef<Path>>(&self, directory: P, specifier: &str) 
        -> Result<Resolution, ResolveError>
    pub fn resolve_file<P: AsRef<Path>>(&self, file: P, specifier: &str) 
        -> Result<Resolution, ResolveError>
    pub fn resolve_with_context<P: AsRef<Path>>(
        &self, 
        directory: P, 
        specifier: &str, 
        tsconfig: Option<&TsConfig>, 
        resolve_context: &mut ResolveContext
    ) -> Result<Resolution, ResolveError>
}

pub struct ResolveOptions {
    pub cwd: Option<PathBuf>,
    pub tsconfig: Option<TsconfigDiscovery>,
    pub alias: Alias,
    pub alias_fields: Vec<Vec<String>>,
    pub condition_names: Vec<String>,              // e.g., ["node", "import"]
    pub enforce_extension: EnforceExtension,
    pub exports_fields: Vec<Vec<String>>,          // default: [["exports"]]
    pub imports_fields: Vec<Vec<String>>,          // default: [["imports"]]
    pub extension_alias: Vec<(String, Vec<String>)>,
    pub extensions: Vec<String>,                   // default: [".js", ".json", ".node"]
    pub fallback: Alias,
    pub fully_specified: bool,                     // default: false
    pub main_fields: Vec<String>,                  // default: ["main"]
    pub main_files: Vec<String>,                   // default: ["index"]
    pub modules: Vec<String>,                      // default: ["node_modules"]
    pub resolve_to_context: bool,
    pub prefer_relative: bool,
    pub prefer_absolute: bool,
    pub restrictions: Vec<Restriction>,
    pub roots: Vec<PathBuf>,
    pub symlinks: bool,                            // default: true
    pub node_path: bool,                           // default: true (read NODE_PATH)
    pub builtin_modules: bool,                     // default: false
    pub module_type: bool,                         // default: false
    pub allow_package_exports_in_directory_resolve: bool,  // default: false
    #[cfg(feature = "yarn_pnp")]
    pub yarn_pnp: bool,
}

impl ResolveOptions {
    pub fn default() -> Self
    pub fn with_condition_names(self, names: &[&str]) -> Self
    pub fn with_extension<S: Into<String>>(self, extension: S) -> Self
    pub fn with_main_field<S: Into<String>>(self, field: S) -> Self
    pub fn with_builtin_modules(self, flag: bool) -> Self
    pub fn with_fully_specified(self, flag: bool) -> Self
    pub fn with_symbolic_link(self, flag: bool) -> Self
    // ... many more builders
}
```

### Resolution & ResolveContext
```rust
pub struct Resolution {
    pub path: PathBuf,
    pub query: Option<String>,                     // ?query (includes ?)
    pub fragment: Option<String>,                  // #fragment (includes #)
    pub package_json: Option<Arc<PackageJson>>,
    pub module_type: Option<ModuleType>,           // ESM/CommonJS/JSON/WASM/Addon
}

impl Resolution {
    pub fn path(&self) -> &Path
    pub fn into_path_buf(self) -> PathBuf
    pub fn query(&self) -> Option<&str>
    pub fn fragment(&self) -> Option<&str>
    pub fn full_path(&self) -> PathBuf                      // path + query + fragment
    pub fn package_json(&self) -> Option<&Arc<PackageJson>>
    pub fn module_type(&self) -> Option<ModuleType>
}

pub enum ModuleType {
    Module,     // ESM (.mjs or "type": "module")
    CommonJs,   // CJS (.cjs or "type": "commonjs")
    Json,       // .json
    Wasm,       // .wasm
    Addon,      // .node
}

pub struct ResolveContext {
    pub file_dependencies: FxHashSet<PathBuf>,     // Files found on filesystem
    pub missing_dependencies: FxHashSet<PathBuf>,  // Files not found
}
```

### ResolveError enum
```rust
pub enum ResolveError {
    Ignored(PathBuf),                             // Marked `false` in package.json browser field
    NotFound(String),                             // Module not found
    MatchedAliasNotFound(String, String),         // Alias matched but resolved path not found
    TsconfigNotFound(PathBuf),
    TsconfigSelfReference(PathBuf),
    TsconfigCircularExtend(CircularPathBufs),
    IOError(IOError),
    PathNotSupported(PathBuf),                    // DOS device path, etc.
    Builtin { resolved: String, is_runtime_module: bool },  // Node builtin module (node:fs, etc.)
    ExtensionAlias(String, String, PathBuf),
    Specifier(SpecifierError),
    Json(JSONError),
    InvalidModuleSpecifier(String, PathBuf),
    InvalidPackageTarget(String, String, PathBuf),
    PackagePathNotExported { subpath: String, package_path: PathBuf, package_json_path: PathBuf, conditions: ConditionNames },
    InvalidPackageConfig(PathBuf),
    InvalidPackageConfigDefault(PathBuf),
    InvalidPackageConfigDirectory(PathBuf),
    PackageImportNotDefined(String, PathBuf),
    Unimplemented(&'static str),
    Recursion,
    #[cfg(feature = "yarn_pnp")]
    FailedToFindYarnPnpManifest(PathBuf),
    #[cfg(feature = "yarn_pnp")]
    YarnPnpError(pnp::Error),
}

impl ResolveError {
    pub const fn is_ignore(&self) -> bool  // true for Ignored variant
}
```

---

## Summary: Key Entry Points

1. **Parse**: `Parser::new(allocator, src, source_type).parse()` → `ParserReturn`
2. **Analyze**: `SemanticBuilder::new().build(&program)` → `Semantic` with scoping
3. **Find exports**: Access `parser_return.module_record.{local_export_entries, import_entries, ...}`
4. **Resolve symbols**: Use `semantic.scoping().get_binding()`, `get_reference()`, `symbol_references()`
5. **Walk AST**: Iterate `semantic.nodes()` (if built) or use visitor pattern
6. **Resolve modules**: `Resolver::new(options).resolve(dir, specifier)` → `Resolution`
7. **JSDoc**: Access via `semantic.jsdoc().get_one_by_node(node_id)`

---

**Version**: oxc 0.139.0  
**Compiled**: From commit 69b2dfc1810e6ad9d5508ae9a20bc773ba5aa18d  
**Resolver**: oxc_resolver 11.23.0 (independently versioned)
