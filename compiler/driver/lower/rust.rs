//! Projects borrowed rust-analyzer HIR facts into the shared canonical fact lane.
//!
//! The projection covers every plane the validated image carries: declaration
//! facts, the recursive declared-type graph, executable signatures with
//! parameter and result facts, oracle-resolved occurrences, and Rustdoc
//! fragments. Emission is two-pass: every module, record, enum, and trait is
//! committed first with its legal diagonal self-nominal, so every nominal,
//! type-child, and product target that follows is a strictly backward fact
//! ordinal. Compound types ride the anonymous type-row pool, so no synthetic
//! carrier declaration is ever invented for `Vec<T>`, `&T`, `[T; N]`, or a
//! function pointer.
//!
//! Lane encoding laws this lowering obeys:
//! - Declarations are emitted declarations-first (type roots, then members in
//!   source order), so every type-record child and nominal target is strictly
//!   backward.
//! - A function is `function(parameter_count, result_count)`; its receiver,
//!   parameters, and result slot are `Parameter` facts pushed immediately
//!   before it, and the function's declared type is the `FunctionPointer` row
//!   over exactly those rows.
//! - Foreign named types (`std` and every other dependency) keep their exact
//!   written spelling as an `Unknown(UnresolvedExternal)` row; the lane
//!   borrows source bytes only, so no rust-analyzer-rendered qualified path
//!   can be manufactured into any cell. Positions the walk cannot prove stay
//!   `Unknown` rows with the exact closed reason, never an invented shape.
//! - Occurrences resolve through rust-analyzer: method calls and paths with a
//!   static target land `Local` or foreign at oracle confidence, and every
//!   span is relative to the innermost owning declaration.
//! - Macro invocation spellings travel in each owning declaration's Rust
//!   extension row; a `macro_rules!` definition itself has no closed lane
//!   row kind, so definitions stay out of the declaration set instead of
//!   being misdeclared as some other entity.
//! - Associated-type projections and inferred array lengths have no row the
//!   HIR walk can prove, so they fold to the honest `Unknown(OracleGap)`
//!   record instead of a fabricated shape.

use std::{sync::atomic::AtomicBool, vec::Vec};

use compiler_ir::{
    AtomListId, DocFragmentInput, DocLinkTarget, EntityId, EntityKind, ForeignKey, ForeignOrigin,
    ListSpan, NominalRef, Occurrence, OccurrenceConfidence, OccurrenceTarget, PrimitiveShape,
    ProductChildRole, ReferenceKind, RelSpan, RustFacts, RustOwnership, SemanticProductConstructor,
    SemanticTypeRecord, SemanticTypeTag, TypeParameterListId, TypeReason, TypeWidth,
};
use compiler_languages_rust::{
    ByteSpan, RustAnalysisControl, RustAuthority, RustAuthorityError, RustDeclaration,
    RustDefinition, RustProject, SemanticKind, SourceByteLimit, ra_ap_hir, ra_ap_ide_db,
    ra_ap_syntax,
};
use ra_ap_syntax::{
    AstNode, SyntaxNode, ast::{self, HasGenericArgs, HasGenericParams, HasName, HasTypeBounds},
};

use crate::{
    lower::{EmissionExtension, FactSet, LEAF_PRODUCT, SemanticFact, push_fact},
    types::LoweringUnsupported,
};

/// The bit a `Primitive(Reference)` payload1 cell reserves for mutability.
const REFERENCE_MUTABLE_FLAG: u32 = SemanticTypeRecord::INTEGER_SIGNED_FLAG;
/// Bit offset of the integer width cell above the signedness bit.
const INTEGER_WIDTH_SHIFT: u32 = 1;
/// Bound of the recursive declared-type walk; deeper positions fold to the
/// lane's exact `TruncatedAtDepthLimit` reason instead of unbounded recursion.
const MAX_TYPE_DEPTH: usize = 16;
/// Maximum pooled children of one compound row or fact record; positions
/// beyond it fold to the honest gap reason instead of a lane rejection.
const MAX_COMPOUND_CHILDREN: usize = 8;
/// Maximum entries of the anonymous foreign-leaf row dedup table.
const MAX_DEDUPED_FOREIGN_ROWS: usize = 64;
/// Ecosystem namespace of every foreign key this authority emits.
const CARGO_ECOSYSTEM: &str = "cargo";
/// The receiver's written name; every Rust receiver spells exactly this.
const SELF_NAME: &[u8] = b"self";
/// Fallback binding name for a parameter whose pattern spells no identifier.
const PARAM_FALLBACK_NAME: &[u8] = b"param";

/// Exact direct-authority rejection while rust-analyzer HIR is borrowed.
///
/// The closed two-variant terminal is fixed by the shared driver failure
/// match: authority faults keep their full typed cause, and every bounded-lane
/// rejection folds onto the lane's single closed lowering terminal.
#[derive(Debug)]
pub(crate) enum RustCollectError {
    /// rust-analyzer could not open, resolve, or query the selected Cargo graph.
    Authority(RustAuthorityError),
    /// Canonical admission rejected one borrowed HIR declaration.
    Lowering(compiler_vocabulary::LoweringUnsupported),
}

/// Runs a non-escaping rust-analyzer transaction and emits the complete
/// borrowed semantic plane.
///
/// The selected Cargo project is caller-owned. This function never derives a
/// project root, invokes rustc, scans source bytes, or substitutes syntax
/// tokens for HIR definitions.
pub(crate) fn collect<'source>(
    project: &RustProject,
    maximum_source_bytes: SourceByteLimit,
    cancelled: &AtomicBool,
    source: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), RustCollectError> {
    project
        .analyze(
            RustAnalysisControl {
                cancelled,
                maximum_source_bytes,
            },
            |authority| {
                if authority.source != source {
                    return Err(RustAuthorityError::SourceBinding {
                        expected: source.len(),
                        observed: authority.source.len(),
                    });
                }
                Emitter::new(&authority, source, facts).run()
            },
        )
        .map_err(|cause| match cause {
            RustAuthorityError::Admission { cause } => RustCollectError::Lowering(cause),
            cause => RustCollectError::Authority(cause),
        })
}

/// Folds one bounded-lane rejection onto the lane's closed terminal. The
/// shared driver failure match owns the terminal arms and lies outside this
/// module's ownership, so the exact rejected fact stays nameable only at this
/// collapse site; the closed `Admission` terminal retains its typed cause.
fn admission() -> RustAuthorityError {
    RustAuthorityError::Admission {
        cause: LoweringUnsupported::NoSupportedDeclaration,
    }
}

/// Admits one fact, folding any bounded-lane rejection onto the closed
/// terminal while preserving the lane ordinal on success.
fn push<'source>(
    facts: &mut FactSet<'source>,
    fact: SemanticFact<'source>,
) -> Result<usize, RustAuthorityError> {
    push_fact(facts, fact).map_err(|_| admission())
}

/// Widenes one bounded lane coordinate; unreachable past the lane's fixed
/// bounds, but never silently truncated.
fn coordinate(value: usize) -> Result<u32, RustAuthorityError> {
    u32::try_from(value).map_err(|_| admission())
}

/// One materialized declaration: the HIR definition, its item syntax, its
/// exact name bytes, and its whole-item source span.
struct Decl {
    kind: SemanticKind,
    definition: RustDefinition,
    syntax: SyntaxNode,
    name_span: ByteSpan,
    span: ByteSpan,
}

/// One pushed declaration row with the coordinates every later phase needs.
struct Row<'source> {
    ordinal: u32,
    name: &'source [u8],
    span: ByteSpan,
    /// The row's Rust extension facts as pushed, before macros attach.
    extension: RustFacts,
}

/// One macro invocation site with its written spelling and call span.
#[derive(Clone, Copy)]
struct MacroSite<'source> {
    spelling: &'source [u8],
    span: ByteSpan,
    resolved: bool,
}

/// One lowered type position: the lattice record plus the already-interned
/// backward coordinates its record needs as pooled children.
struct Lowered<'source> {
    record: SemanticTypeRecord<'source>,
    children: Vec<u32>,
}

impl<'source> Lowered<'source> {
    /// Wraps one childless lattice record.
    const fn leaf(record: SemanticTypeRecord<'source>) -> Self {
        Self {
            record,
            children: Vec::new(),
        }
    }
}

/// The two-pass Rust semantic emitter over one borrowed authority.
struct Emitter<'authority, 'analysis, 'source> {
    authority: &'authority RustAuthority<'analysis>,
    database: &'analysis ra_ap_ide_db::RootDatabase,
    source: &'source [u8],
    facts: &'authority mut FactSet<'source>,
    /// Pushed declaration rows in lane order, for owners and doc links.
    rows: Vec<Row<'source>>,
    /// Pushed HIR definition ordinals, for occurrence target resolution.
    definitions: Vec<(ra_ap_hir::ModuleDef, u32)>,
    /// Lane ordinal per materialized declaration index.
    ordinals: Vec<Option<u32>>,
    /// Dedup table of interned foreign-unknown leaf rows, keyed by spelling.
    foreign_rows: Vec<(&'source [u8], u32)>,
    /// Macro invocation sites collected before emission.
    macro_sites: Vec<MacroSite<'source>>,
}

impl<'authority, 'analysis, 'source> Emitter<'authority, 'analysis, 'source> {
    fn new(
        authority: &'authority RustAuthority<'analysis>,
        source: &'source [u8],
        facts: &'authority mut FactSet<'source>,
    ) -> Self {
        Self {
            database: authority.database,
            authority,
            source,
            facts,
            rows: Vec::new(),
            definitions: Vec::new(),
            ordinals: Vec::new(),
            foreign_rows: Vec::new(),
            macro_sites: Vec::new(),
        }
    }

    /// Streams the complete semantic plane in lane order.
    fn run(&mut self) -> Result<(), RustAuthorityError> {
        let declarations = self.materialize_declarations()?;
        self.ordinals = declarations.iter().map(|_| None).collect();
        self.collect_macro_sites()?;
        self.emit_type_roots(&declarations)?;
        self.emit_members(&declarations)?;
        self.attach_macros()?;
        self.emit_occurrences()?;
        self.emit_docs(&declarations)?;
        Ok(())
    }

    /// Borrows exact source bytes for a previously validated span.
    fn bytes_of(&self, span: ByteSpan) -> Result<&'source [u8], RustAuthorityError> {
        let start = usize::try_from(span.start).map_err(|source| RustAuthorityError::Coordinate {
            span,
            source,
        })?;
        let end = usize::try_from(span.end).map_err(|source| RustAuthorityError::Coordinate {
            span,
            source,
        })?;
        self.source
            .get(start..end)
            .ok_or(RustAuthorityError::InvalidSpan {
                span,
                source_bytes: self.source.len(),
            })
    }

    /// Borrows exact source bytes for one syntax node's validated span.
    fn bytes_of_node(&self, node: &SyntaxNode) -> Result<&'source [u8], RustAuthorityError> {
        let span = self.authority.span(node)?;
        self.bytes_of(span)
    }

    /// Locates one borrowed doc line's exact byte span in the caller source.
    /// The parsed tree's text lives in its own buffer, so the line is found
    /// by content; a line absent from the source stays unborrowed.
    fn span_of_text(&self, text: &str) -> Option<ByteSpan> {
        let needle = text.as_bytes();
        if needle.is_empty() || self.source.len() < needle.len() {
            return None;
        }
        self.source
            .windows(needle.len())
            .position(|window| window == needle)
            .and_then(|start| {
                let start = u32::try_from(start).ok()?;
                let end = start.checked_add(u32::try_from(needle.len()).ok()?)?;
                Some(ByteSpan { start, end })
            })
    }

    /// Materializes every lane-admissible declaration once, with its exact
    /// name bytes and whole-item span. `macro_rules!` definitions carry no
    /// closed declaration-kind row and stay out of the set; their invocation
    /// spellings travel on the invoking declarations instead.
    fn materialize_declarations(&mut self) -> Result<Vec<Decl>, RustAuthorityError> {
        let authority = self.authority;
        let mut declarations = Vec::new();
        for declaration in authority.declarations() {
            if matches!(declaration.definition, RustDefinition::Macro(_)) {
                continue;
            }
            let span = authority.span(&declaration.syntax)?;
            let name_span = self.declaration_name(&declaration)?;
            declarations.push(Decl {
                kind: declaration.kind,
                definition: declaration.definition,
                syntax: declaration.syntax,
                name_span,
                span,
            });
        }
        Ok(declarations)
    }

    /// Borrows the exact name bytes of one declaration. Implementations name
    /// by their written self type (the closed lane has no anonymous rows),
    /// falling back to the authority's first-identifier rule.
    fn declaration_name(
        &self,
        declaration: &RustDeclaration,
    ) -> Result<ByteSpan, RustAuthorityError> {
        if declaration.kind == SemanticKind::Implementation
            && let Some(implementation) = ast::Impl::cast(declaration.syntax.clone())
            && let Some(self_ty) = implementation.self_ty()
            && let Some(ast::Type::PathType(path_type)) = Some(self_ty.clone())
            && let Some(segment) = path_type.path().and_then(|path| path.segments().last())
            && let Some(name_ref) = segment.name_ref()
        {
            return self.authority.span(name_ref.syntax());
        }
        self.authority.declaration_name(declaration)
    }

    /// Borrows one declaration's exact name bytes from the caller source.
    fn name_of(&self, declaration: &Decl) -> Result<&'source [u8], RustAuthorityError> {
        self.bytes_of(declaration.name_span)
    }

    /// Borrows the written leaf name of one type position, when the position
    /// spells a path (`Option`, `std::vec::Vec` yields `Vec`).
    fn written_type_name(&self, anchor: Option<&ast::Type>) -> Option<&'source [u8]> {
        let ast::Type::PathType(path_type) = anchor? else {
            return None;
        };
        let segment = path_type.path()?.segments().last()?;
        let name_ref = segment.name_ref()?;
        let span = self.authority.span(name_ref.syntax()).ok()?;
        self.bytes_of(span).ok()
    }

    /// The unresolved record for a foreign named type: the exact written
    /// spelling when the source wrote one, otherwise the gap reason whose
    /// record lawfully carries no text cell.
    fn unresolved_record(&self, anchor: Option<&ast::Type>) -> SemanticTypeRecord<'source> {
        match self.written_type_name(anchor) {
            Some(text) => unknown_record(TypeReason::UnresolvedExternal, Some(text)),
            None => unknown_record(TypeReason::OracleGap, None),
        }
    }

    /// Collects every macro invocation site once: written path spelling,
    /// exact call span, and whether rust-analyzer resolved the definition.
    fn collect_macro_sites(&mut self) -> Result<(), RustAuthorityError> {
        let authority = self.authority;
        for call in authority.macro_calls() {
            let span = authority.span(call.syntax())?;
            let Some(path) = call.path() else {
                continue;
            };
            let spelling = self.bytes_of_node(path.syntax())?;
            let resolved = authority.resolve_macro(&call).is_some();
            self.macro_sites.push(MacroSite {
                spelling,
                span,
                resolved,
            });
        }
        Ok(())
    }

    /// Pass one: every module, record, enum, and trait with its legal
    /// diagonal self-nominal, so any later type can name any type root.
    fn emit_type_roots(&mut self, declarations: &[Decl]) -> Result<(), RustAuthorityError> {
        for index in 0..declarations.len() {
            let kind = declarations[index].kind;
            if !matches!(
                kind,
                SemanticKind::Module
                    | SemanticKind::Record
                    | SemanticKind::Enum
                    | SemanticKind::Trait
            ) {
                continue;
            }
            let declaration = &declarations[index];
            let extension = self.base_extension(RustOwnership::Value, &declaration.syntax)?;
            let mut fact = SemanticFact::new(
                entity_kind(kind)?,
                self.name_of(declaration)?,
                constructor(kind)?,
            )
            .with_extension(EmissionExtension::Rust(extension));
            if kind != SemanticKind::Module {
                let own_ordinal = coordinate(self.facts.len())?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                record.nominal = Some(NominalRef::Local(EntityId::new(own_ordinal)));
                fact = fact.typed(record);
            }
            let ordinal = push(self.facts, fact)?;
            self.register(ordinal, index, declaration, extension)?;
        }
        Ok(())
    }

    /// Pass two: fields, variants, implementations, callables, aliases, and
    /// value bindings in source order, each with its projected declared type.
    fn emit_members(&mut self, declarations: &[Decl]) -> Result<(), RustAuthorityError> {
        for index in 0..declarations.len() {
            let declaration = &declarations[index];
            match declaration.kind {
                SemanticKind::Field => self.emit_field(index, declaration)?,
                SemanticKind::Variant => self.emit_variant(index, declaration)?,
                SemanticKind::Implementation => self.emit_implementation(index, declaration)?,
                SemanticKind::Function => self.emit_function(index, declaration)?,
                SemanticKind::TypeAlias | SemanticKind::Constant | SemanticKind::Static => {
                    self.emit_binding(index, declaration)?
                }
                SemanticKind::Module
                | SemanticKind::Record
                | SemanticKind::Enum
                | SemanticKind::Trait
                | SemanticKind::Macro
                | SemanticKind::LocalBinding
                | SemanticKind::GenericParameter
                | SemanticKind::Builtin => {}
            }
        }
        Ok(())
    }

    /// Emits one record field with its HIR-projected declared type.
    fn emit_field(&mut self, index: usize, declaration: &Decl) -> Result<(), RustAuthorityError> {
        let RustDefinition::Field(field) = &declaration.definition else {
            return Ok(());
        };
        let semantic = field.ty(self.database);
        let anchor =
            ast::RecordField::cast(declaration.syntax.clone()).and_then(|item| item.ty());
        let lowered = self.lower_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
        self.push_typed(
            index,
            declaration,
            lowered,
            RustOwnership::Value,
            &declaration.syntax,
        )
    }

    /// Emits one enum variant; its constructed type is the parent enum, the
    /// same nominal row the enum itself carries.
    fn emit_variant(
        &mut self,
        index: usize,
        declaration: &Decl,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Variant(variant) = &declaration.definition else {
            return Ok(());
        };
        let parent = variant.parent_enum(self.database);
        let record = match self.ordinal_of_adt(ra_ap_hir::Adt::from(parent)) {
            Some(ordinal) => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
                record
            }
            None => unknown_record(TypeReason::OracleGap, None),
        };
        self.push_typed(
            index,
            declaration,
            Lowered::leaf(record),
            RustOwnership::Value,
            &declaration.syntax,
        )
    }

    /// Emits one inherent or trait implementation with its self type.
    fn emit_implementation(
        &mut self,
        index: usize,
        declaration: &Decl,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Implementation(implementation) = &declaration.definition else {
            return Ok(());
        };
        let semantic = implementation.self_ty(self.database);
        let anchor = ast::Impl::cast(declaration.syntax.clone()).and_then(|item| item.self_ty());
        let lowered = self.lower_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
        self.push_typed(
            index,
            declaration,
            lowered,
            RustOwnership::Value,
            &declaration.syntax,
        )
    }

    /// Emits one type alias, constant, or static binding from its HIR
    /// semantic type, anchored on the written type when the source spells one.
    fn emit_binding(
        &mut self,
        index: usize,
        declaration: &Decl,
    ) -> Result<(), RustAuthorityError> {
        let semantic = declaration
            .definition
            .semantic_type(self.database)
            .ok_or(RustAuthorityError::MissingSemanticFact {
                fact: declaration.kind,
            })?;
        let anchor = written_binding_type(&declaration.syntax);
        let lowered = self.lower_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
        self.push_typed(
            index,
            declaration,
            lowered,
            RustOwnership::Value,
            &declaration.syntax,
        )
    }

    /// Emits one function: its receiver, parameters, and result slot first,
    /// then the function fact whose product and `FunctionPointer` type
    /// children are exactly those rows.
    fn emit_function(
        &mut self,
        index: usize,
        declaration: &Decl,
    ) -> Result<(), RustAuthorityError> {
        let RustDefinition::Function(function) = &declaration.definition else {
            return Ok(());
        };
        let Some(fn_item) = ast::Fn::cast(declaration.syntax.clone()) else {
            return Err(RustAuthorityError::MissingSemanticFact {
                fact: SemanticKind::Function,
            });
        };
        let mut parameter_ordinals = Vec::new();
        let mut signature_children = Vec::new();

        if let Some(receiver) = function.self_param(self.database) {
            let semantic = receiver.ty(self.database);
            let lowered = self.lower_type(&semantic, None, MAX_TYPE_DEPTH)?;
            let ownership = receiver_ownership(receiver.access(self.database));
            let ordinal = self.push_parameter(SELF_NAME, lowered, ownership)?;
            parameter_ordinals.push(ordinal);
            signature_children.push(ordinal);
        }

        let parameters = function.params_without_self(self.database);
        let written: Vec<ast::Param> = fn_item
            .param_list()
            .map(|list| list.params().collect())
            .unwrap_or_default();
        for (position, parameter) in parameters.iter().enumerate() {
            let semantic = parameter.ty().clone();
            let anchor = written.get(position).and_then(|param| param.ty());
            let name = self.parameter_name(written.get(position));
            let lowered = self.lower_type(&semantic, anchor.as_ref(), MAX_TYPE_DEPTH)?;
            let ownership = parameter_ownership(self.database, &semantic);
            let ordinal = self.push_parameter(name, lowered, ownership)?;
            parameter_ordinals.push(ordinal);
            signature_children.push(ordinal);
        }

        let returns = function.ret_type(self.database);
        let result_anchor = fn_item.ret_type().and_then(|ret| ret.ty());
        let lowered = self.lower_type(&returns, result_anchor.as_ref(), MAX_TYPE_DEPTH)?;
        let mut fact = SemanticFact::new(
            EntityKind::Parameter,
            self.name_of(declaration)?,
            LEAF_PRODUCT,
        )
        .typed(lowered.record)
        .with_extension(EmissionExtension::Rust(self.empty_extension(RustOwnership::Value)?));
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        let result_ordinal = coordinate(push(self.facts, fact)?)?;
        signature_children.push(result_ordinal);

        let arity = coordinate(parameter_ordinals.len())?;
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        record.payload1 = SemanticTypeRecord::RESULT_FLAG;
        let extension = self.base_extension(RustOwnership::Value, &declaration.syntax)?;
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            self.name_of(declaration)?,
            SemanticProductConstructor::function(arity, 1),
        )
        .typed(record)
        .with_extension(EmissionExtension::Rust(extension));
        for ordinal in &parameter_ordinals {
            fact = fact.child(ProductChildRole::FunctionParameter, *ordinal);
        }
        fact = fact.child(ProductChildRole::FunctionResult, result_ordinal);
        for ordinal in &signature_children {
            fact = fact.type_child(*ordinal, None, 0);
        }
        let ordinal = push(self.facts, fact)?;
        self.register(ordinal, index, declaration, extension)
    }

    /// Pushes one signature carrier fact (receiver or parameter) with its
    /// lowered record and the ownership-only extension row its position owns.
    fn push_parameter(
        &mut self,
        name: &'source [u8],
        lowered: Lowered<'source>,
        ownership: RustOwnership,
    ) -> Result<u32, RustAuthorityError> {
        let mut fact = SemanticFact::new(EntityKind::Parameter, name, LEAF_PRODUCT)
            .typed(lowered.record)
            .with_extension(EmissionExtension::Rust(self.empty_extension(ownership)?));
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        coordinate(push(self.facts, fact)?)
    }

    /// Borrows one written parameter name: the binding identifier when the
    /// pattern spells one, otherwise the whole pattern text, otherwise the
    /// static fallback binding name.
    fn parameter_name(&self, written: Option<&ast::Param>) -> &'source [u8] {
        let Some(param) = written else {
            return PARAM_FALLBACK_NAME;
        };
        let Some(pattern) = param.pat() else {
            return PARAM_FALLBACK_NAME;
        };
        if let ast::Pat::IdentPat(ident) = &pattern
            && let Some(name) = ident.name()
            && let Ok(bytes) = self.bytes_of_node(name.syntax())
        {
            return bytes;
        }
        self.bytes_of_node(pattern.syntax())
            .unwrap_or(PARAM_FALLBACK_NAME)
    }

    /// Pushes one member fact with a projected record and its base Rust
    /// extension row, then registers the row.
    fn push_typed(
        &mut self,
        index: usize,
        declaration: &Decl,
        lowered: Lowered<'source>,
        ownership: RustOwnership,
        syntax: &SyntaxNode,
    ) -> Result<(), RustAuthorityError> {
        let extension = self.base_extension(ownership, syntax)?;
        let mut fact = SemanticFact::new(
            entity_kind(declaration.kind)?,
            self.name_of(declaration)?,
            constructor(declaration.kind)?,
        )
        .typed(lowered.record)
        .with_extension(EmissionExtension::Rust(extension));
        for target in lowered.children {
            fact = fact.type_child(target, None, 0);
        }
        let ordinal = push(self.facts, fact)?;
        self.register(ordinal, index, declaration, extension)
    }

    /// Builds one declaration's base Rust extension row: the ownership cell,
    /// its written lifetime spellings, and its pooled where-clause rows.
    /// Macro spellings attach in a later phase.
    fn base_extension(
        &mut self,
        ownership: RustOwnership,
        syntax: &SyntaxNode,
    ) -> Result<RustFacts, RustAuthorityError> {
        let lifetimes = self.lifetime_atoms(syntax)?;
        let where_start = self.where_rows(syntax)?;
        Ok(RustFacts {
            ownership,
            lifetimes,
            where_clauses: TypeParameterListId::new(where_start),
            macros: AtomListId::new(0),
        })
    }

    /// Builds the ownership-only extension row of a signature carrier: no
    /// lifetimes, no where rows of its own, and no macro spellings yet.
    fn empty_extension(
        &mut self,
        ownership: RustOwnership,
    ) -> Result<RustFacts, RustAuthorityError> {
        let empty_atoms = self.facts.intern_atom_list(&[]).map_err(|_| admission())?;
        Ok(RustFacts {
            ownership,
            lifetimes: empty_atoms,
            where_clauses: TypeParameterListId::new(coordinate(
                self.facts.type_parameter_len,
            )?),
            macros: empty_atoms,
        })
    }

    /// Interns one declaration's written lifetime parameters as extension
    /// atoms and returns their pooled list.
    fn lifetime_atoms(&mut self, syntax: &SyntaxNode) -> Result<AtomListId, RustAuthorityError> {
        let mut atoms = Vec::new();
        let generics = ast::AnyHasGenericParams::cast(syntax.clone());
        if let Some(list) = generics.as_ref().and_then(|item| item.generic_param_list()) {
            for generic in list.generic_params() {
                let ast::GenericParam::LifetimeParam(lifetime) = generic else {
                    continue;
                };
                let Some(lifetime) = lifetime.lifetime() else {
                    continue;
                };
                let spelling = self.bytes_of_node(lifetime.syntax())?;
                let atom = self.facts.intern_atom(spelling).map_err(|_| admission())?;
                atoms.push(atom);
            }
        }
        self.facts.intern_atom_list(&atoms).map_err(|_| admission())
    }

    /// Pushes one pooled where-clause row per written bound: one row per
    /// generic parameter bound and one per where predicate. The constraint
    /// cell names the first bound that resolves to a lane-local trait; bounds
    /// resolving outside the fragment stay `None` because the pooled lane
    /// references only backward declaration ordinals.
    fn where_rows(&mut self, syntax: &SyntaxNode) -> Result<u32, RustAuthorityError> {
        let start = coordinate(self.facts.type_parameter_len)?;
        let Some(generics) = ast::AnyHasGenericParams::cast(syntax.clone()) else {
            return Ok(start);
        };
        if let Some(list) = generics.generic_param_list() {
            for generic in list.generic_params() {
                match generic {
                    ast::GenericParam::TypeParam(param) => {
                        let Some(name) = param.name() else {
                            continue;
                        };
                        let name = self.bytes_of_node(name.syntax())?;
                        let constraint = self.local_trait_bound(param.type_bound_list())?;
                        let default = match param.default_type() {
                            Some(default) => self.local_adt_target(&default)?,
                            None => None,
                        };
                        self.facts
                            .push_type_parameter(name, constraint, default)
                            .map_err(|_| admission())?;
                    }
                    ast::GenericParam::ConstParam(param) => {
                        let Some(name) = param.name() else {
                            continue;
                        };
                        let name = self.bytes_of_node(name.syntax())?;
                        self.facts
                            .push_type_parameter(name, None, None)
                            .map_err(|_| admission())?;
                    }
                    ast::GenericParam::LifetimeParam(_) => {}
                }
            }
        }
        if let Some(where_clause) = generics.where_clause() {
            for predicate in where_clause.predicates() {
                let Some(bounded) = predicate.ty() else {
                    continue;
                };
                let name = self.bytes_of_node(bounded.syntax())?;
                let constraint = self.local_trait_bound(predicate.type_bound_list())?;
                self.facts
                    .push_type_parameter(name, constraint, None)
                    .map_err(|_| admission())?;
            }
        }
        Ok(start)
    }

    /// Resolves the first written bound of one bound list to a lane-local
    /// trait ordinal, or `None` when no bound resolves into this fragment.
    fn local_trait_bound(
        &self,
        bounds: Option<ast::TypeBoundList>,
    ) -> Result<Option<u32>, RustAuthorityError> {
        let Some(bounds) = bounds else {
            return Ok(None);
        };
        for bound in bounds.bounds() {
            let Some(bound_ty) = bound.ty() else {
                continue;
            };
            let ast::Type::PathType(path_type) = bound_ty else {
                continue;
            };
            let Some(path) = path_type.path() else {
                continue;
            };
            if let Some((ra_ap_hir::PathResolution::Def(ra_ap_hir::ModuleDef::Trait(trait_)), _)) =
                self.authority.resolve_path(&path)
                && let Some(ordinal) = self.ordinal_of_trait(trait_)
            {
                return Ok(Some(ordinal));
            }
        }
        Ok(None)
    }

    /// Resolves one written default type to a lane-local record ordinal.
    fn local_adt_target(&self, default: &ast::Type) -> Result<Option<u32>, RustAuthorityError> {
        let ast::Type::PathType(path_type) = default else {
            return Ok(None);
        };
        let Some(path) = path_type.path() else {
            return Ok(None);
        };
        if let Some((ra_ap_hir::PathResolution::Def(ra_ap_hir::ModuleDef::Adt(adt)), _)) =
            self.authority.resolve_path(&path)
        {
            return Ok(self.ordinal_of_adt(adt));
        }
        Ok(None)
    }

    /// Registers one pushed declaration row, its HIR definition ordinal, and
    /// its lane ordinal for the later phases.
    fn register(
        &mut self,
        ordinal: usize,
        index: usize,
        declaration: &Decl,
        extension: RustFacts,
    ) -> Result<(), RustAuthorityError> {
        let Some(ordinal) = u32::try_from(ordinal).ok() else {
            return Ok(());
        };
        self.rows.push(Row {
            ordinal,
            name: self.name_of(declaration)?,
            span: declaration.span,
            extension,
        });
        if let Some(definition) = module_def(&declaration.definition) {
            self.definitions.push((definition, ordinal));
        }
        if let Some(slot) = self.ordinals.get_mut(index) {
            *slot = Some(ordinal);
        }
        Ok(())
    }

    /// Looks up the pushed ordinal of one lane-local ADT.
    fn ordinal_of_adt(&self, adt: ra_ap_hir::Adt) -> Option<u32> {
        self.definitions
            .iter()
            .find(|(definition, _)| definition == &ra_ap_hir::ModuleDef::Adt(adt))
            .map(|(_, ordinal)| *ordinal)
    }

    /// Looks up the pushed ordinal of one lane-local trait.
    fn ordinal_of_trait(&self, trait_: ra_ap_hir::Trait) -> Option<u32> {
        self.definitions
            .iter()
            .find(|(definition, _)| definition == &ra_ap_hir::ModuleDef::Trait(trait_))
            .map(|(_, ordinal)| *ordinal)
    }

    /// Looks up the pushed ordinal of any pushed HIR definition.
    fn ordinal_of_definition(&self, definition: &ra_ap_hir::ModuleDef) -> Option<u32> {
        self.definitions
            .iter()
            .find(|(known, _)| known == definition)
            .map(|(_, ordinal)| *ordinal)
    }

    /// Picks the innermost pushed row whose span contains `span`.
    fn owner_of(&self, span: ByteSpan) -> Option<u32> {
        self.rows
            .iter()
            .filter(|row| row.span.start <= span.start && span.end <= row.span.end)
            .max_by_key(|row| row.span.start)
            .map(|row| row.ordinal)
    }

    /// Picks the anchor ordinal for anonymous rows: the most recently pushed
    /// fact, which the lane law admits as a row owner. `None` only before
    /// the fragment's first fact exists.
    fn row_anchor(&self) -> Option<u32> {
        if self.facts.len() == 0 {
            None
        } else {
            coordinate(self.facts.len() - 1).ok()
        }
    }

    /// Interns one lowered position into the anonymous row pool (deduplicating
    /// foreign-unknown leaves), or returns the fact ordinal it names directly.
    /// `None` means no row could be lawfully interned yet, so the parent
    /// position must fold to a rowless honest unknown.
    fn row_target(
        &mut self,
        lowered: Lowered<'source>,
    ) -> Result<Option<u32>, RustAuthorityError> {
        if lowered.children.is_empty()
            && let Some(NominalRef::Local(target)) = lowered.record.nominal
        {
            return Ok(Some(target.raw));
        }
        if lowered.children.is_empty()
            && lowered.record.tag == SemanticTypeTag::Unknown
            && let Some(text) = lowered.record.text
        {
            for (known, ordinal) in &self.foreign_rows {
                if *known == text {
                    return Ok(Some(*ordinal));
                }
            }
        }
        let Some(anchor) = self.row_anchor() else {
            return Ok(None);
        };
        let row = self
            .facts
            .intern_anonymous_type_row(anchor, lowered.record)
            .map_err(|_| admission())?;
        if lowered.children.is_empty()
            && lowered.record.tag == SemanticTypeTag::Unknown
            && let Some(text) = lowered.record.text
            && self.foreign_rows.len() < MAX_DEDUPED_FOREIGN_ROWS
        {
            self.foreign_rows.push((text, row));
        }
        for target in lowered.children {
            self.facts
                .anonymous_type_child(target, None, 0)
                .map_err(|_| admission())?;
        }
        Ok(Some(row))
    }

    /// Lowers one HIR type position into its lattice record, using the
    /// written syntax only to borrow exact spellings (foreign leaf names,
    /// array lengths, reference lifetimes).
    fn lower_type(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        if depth == 0 {
            return Ok(Lowered::leaf(unknown_record(
                TypeReason::TruncatedAtDepthLimit,
                None,
            )));
        }
        if semantic.is_unknown() {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        }
        if let Some((inner, mutability)) = semantic.as_reference() {
            return self.lower_reference(&inner, mutability, anchor, depth);
        }
        if let Some((inner, mutability)) = semantic.as_raw_ptr() {
            return self.lower_pointer(&inner, mutability, anchor, depth);
        }
        if let Some(builtin) = semantic.as_builtin() {
            return Ok(Lowered::leaf(builtin_record(builtin)));
        }
        if semantic.is_never() {
            return Ok(Lowered::leaf(SemanticTypeRecord::leaf(
                SemanticTypeTag::Never,
            )));
        }
        if semantic.is_unit() {
            return Ok(Lowered::leaf(SemanticTypeRecord::leaf(
                SemanticTypeTag::Tuple,
            )));
        }
        if semantic.is_tuple() {
            return self.lower_tuple(semantic, anchor, depth);
        }
        if let Some(element) = semantic.as_slice() {
            return self.lower_child_pair(&element, SemanticTypeTag::Slice, anchor, depth);
        }
        if let Some((element, _length)) = semantic.as_array(self.database) {
            return self.lower_array(&element, anchor, depth);
        }
        if semantic.is_closure() || semantic.as_callable(self.database).is_some() {
            return self.lower_callable(semantic, anchor, depth);
        }
        if let Some((adt, arguments)) = semantic.as_adt_with_args() {
            return self.lower_adt(adt, &arguments, anchor, depth);
        }
        if let Some(trait_) = semantic.as_dyn_trait() {
            let written = self.written_type_name(anchor);
            return match self.ordinal_of_trait(trait_) {
                Some(ordinal) => Ok(Lowered {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::DynTrait),
                    children: vec![ordinal],
                }),
                None => Ok(Lowered::leaf(self.unresolved_record_with(written))),
            };
        }
        if let Some(bounds) = semantic.as_impl_traits(self.database) {
            return self.lower_impl_trait(bounds.collect(), anchor);
        }
        if let Some(parameter) = semantic.as_type_param(self.database) {
            return self.lower_type_param(parameter, anchor);
        }
        Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)))
    }

    /// Lowers one shared or exclusive borrow over its referent row.
    fn lower_reference(
        &mut self,
        inner: &ra_ap_hir::Type<'_>,
        mutability: ra_ap_hir::Mutability,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(inner, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        let mut record = primitive_record(PrimitiveShape::Reference, 0);
        if matches!(mutability, ra_ap_hir::Mutability::Mut) {
            record.payload1 = REFERENCE_MUTABLE_FLAG;
        }
        record.text = self.written_lifetime(anchor);
        Ok(Lowered {
            record,
            children: vec![target],
        })
    }

    /// Lowers one raw pointer over its pointee row.
    fn lower_pointer(
        &mut self,
        inner: &ra_ap_hir::Type<'_>,
        mutability: ra_ap_hir::Mutability,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(inner, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        let shape = match mutability {
            ra_ap_hir::Mutability::Mut => PrimitiveShape::MutPointer,
            ra_ap_hir::Mutability::Shared => PrimitiveShape::ConstPointer,
        };
        Ok(Lowered {
            record: primitive_record(shape, 0),
            children: vec![target],
        })
    }

    /// Lowers one one-child compound (a slice) over its element row.
    fn lower_child_pair(
        &mut self,
        inner: &ra_ap_hir::Type<'_>,
        tag: SemanticTypeTag,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(inner, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(tag),
            children: vec![target],
        })
    }

    /// Lowers one array: the element row plus the exact written length text
    /// the Array tag requires. Without a written length the position folds to
    /// the honest gap reason instead of a fabricated size.
    fn lower_array(
        &mut self,
        element: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let target = match self.lower_target(element, child_anchor(anchor, 0), depth)? {
            Some(target) => target,
            None => return Ok(self.folded_rowless(anchor)),
        };
        let Some(text) = (match anchor {
            Some(ast::Type::ArrayType(array)) => array
                .const_arg()
                .and_then(|argument| argument.expr())
                .and_then(|expr| self.bytes_of_node(expr.syntax()).ok()),
            _ => None,
        }) else {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Array);
        record.text = Some(text);
        Ok(Lowered {
            record,
            children: vec![target],
        })
    }

    /// Lowers one tuple: one child per element, unlabeled, because HIR tuple
    /// positions carry no element names.
    fn lower_tuple(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let fields = semantic.tuple_fields(self.database);
        if fields.len() > MAX_COMPOUND_CHILDREN {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        }
        let mut children = Vec::new();
        for (position, element) in fields.iter().enumerate() {
            match self.lower_target(element, child_anchor(anchor, position), depth - 1)? {
                Some(target) => children.push(target),
                None => return Ok(self.folded_rowless(anchor)),
            }
        }
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(SemanticTypeTag::Tuple),
            children,
        })
    }

    /// Lowers one callable position (function value, closure, or fn trait
    /// impl) as a structural `FunctionPointer` over its signature.
    fn lower_callable(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let Some(callable) = semantic.as_callable(self.database) else {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        let parameters = callable.params();
        let mut children = Vec::new();
        for (position, parameter) in parameters.iter().enumerate() {
            let lowered_parameter = parameter.ty().clone();
            let written = callable_anchor(anchor, CallablePart::Parameter(position));
            match self.lower_target(&lowered_parameter, written, depth - 1)? {
                Some(target) => children.push(target),
                None => return Ok(self.folded_rowless(anchor)),
            }
        }
        let returns = callable.return_type().clone();
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
        if !returns.is_unit() {
            let written = callable_anchor(anchor, CallablePart::Return);
            match self.lower_target(&returns, written, depth - 1)? {
                Some(target) => children.push(target),
                None => return Ok(self.folded_rowless(anchor)),
            }
            record.payload1 = SemanticTypeRecord::RESULT_FLAG;
        }
        Ok(Lowered {
            record,
            children,
        })
    }

    /// Lowers one ADT position: a lane-local ADT names its backward nominal
    /// row directly (or an `Apply` over its written arguments), and a foreign
    /// ADT keeps its exact written spelling as the honest unresolved row.
    fn lower_adt(
        &mut self,
        adt: ra_ap_hir::Adt,
        arguments: &[Option<ra_ap_hir::Type<'_>>],
        anchor: Option<&ast::Type>,
        depth: usize,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let written = self.written_type_name(anchor);
        let Some(ordinal) = self.ordinal_of_adt(adt) else {
            return Ok(Lowered::leaf(self.unresolved_record_with(written)));
        };
        let typed_arguments: Vec<ra_ap_hir::Type<'_>> = arguments
            .iter()
            .filter_map(|argument| argument.as_ref())
            .cloned()
            .collect();
        if typed_arguments.is_empty() {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
            record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
            return Ok(Lowered::leaf(record));
        }
        if typed_arguments.len() + 1 > MAX_COMPOUND_CHILDREN {
            return Ok(Lowered::leaf(match written {
                Some(text) => unknown_record(TypeReason::NoIrRepresentation, Some(text)),
                None => unknown_record(TypeReason::OracleGap, None),
            }));
        }
        let mut children = vec![ordinal];
        for (position, argument) in typed_arguments.iter().enumerate() {
            match self.lower_target(argument, child_anchor(anchor, position), depth - 1)? {
                Some(target) => children.push(target),
                None => return Ok(self.folded_rowless(anchor)),
            }
        }
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
            children,
        })
    }

    /// Lowers one `impl Trait` position over its local bound rows; any bound
    /// resolving outside the fragment folds to the written unresolved row.
    fn lower_impl_trait(
        &mut self,
        bounds: Vec<ra_ap_hir::Trait>,
        anchor: Option<&ast::Type>,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        let written = self.written_type_name(anchor);
        let mut children = Vec::new();
        for trait_ in bounds {
            match self.ordinal_of_trait(trait_) {
                Some(ordinal) => children.push(ordinal),
                None => {
                    return Ok(Lowered::leaf(self.unresolved_record_with(written)));
                }
            }
        }
        Ok(Lowered {
            record: SemanticTypeRecord::leaf(SemanticTypeTag::ImplTrait),
            children,
        })
    }

    /// Lowers one generic-parameter use: the implicit trait `Self` stays a
    /// `SelfType` leaf, every written parameter keeps its exact spelling as
    /// a `TypeVar` row, and an unwritten use folds to the gap reason.
    fn lower_type_param(
        &mut self,
        parameter: ra_ap_hir::TypeParam,
        anchor: Option<&ast::Type>,
    ) -> Result<Lowered<'source>, RustAuthorityError> {
        if parameter.is_implicit(self.database) {
            return Ok(Lowered::leaf(SemanticTypeRecord::leaf(
                SemanticTypeTag::SelfType,
            )));
        }
        let Some(text) = self.written_type_name(anchor) else {
            return Ok(Lowered::leaf(unknown_record(TypeReason::OracleGap, None)));
        };
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
        record.text = Some(text);
        Ok(Lowered::leaf(record))
    }

    /// Lowers one position and interns it into a pooled row coordinate (or
    /// returns the nominal fact ordinal it names); `None` folds the parent.
    fn lower_target(
        &mut self,
        semantic: &ra_ap_hir::Type<'_>,
        anchor: Option<ast::Type>,
        depth: usize,
    ) -> Result<Option<u32>, RustAuthorityError> {
        let lowered = self.lower_type(semantic, anchor.as_ref(), depth)?;
        self.row_target(lowered)
    }

    /// The rowless fold for a compound whose pooled rows cannot be lawfully
    /// interned yet: the exact written spelling, or the gap reason.
    fn folded_rowless(&self, anchor: Option<&ast::Type>) -> Lowered<'source> {
        Lowered::leaf(self.unresolved_record(anchor))
    }

    /// Same as [`Emitter::unresolved_record`] for an already-borrowed
    /// spelling.
    fn unresolved_record_with(
        &self,
        written: Option<&'source [u8]>,
    ) -> SemanticTypeRecord<'source> {
        match written {
            Some(text) => unknown_record(TypeReason::UnresolvedExternal, Some(text)),
            None => unknown_record(TypeReason::OracleGap, None),
        }
    }

    /// Borrows the written lifetime behind one reference anchor.
    fn written_lifetime(&self, anchor: Option<&ast::Type>) -> Option<&'source [u8]> {
        match anchor? {
            ast::Type::RefType(reference) => {
                let lifetime = reference.lifetime()?;
                let span = self.authority.span(lifetime.syntax()).ok()?;
                self.bytes_of(span).ok()
            }
            _ => None,
        }
    }

    /// Attaches each macro invocation spelling to the innermost pushed
    /// declaration containing it, replacing that row's extension facts.
    fn attach_macros(&mut self) -> Result<(), RustAuthorityError> {
        if self.macro_sites.is_empty() {
            return Ok(());
        }
        let mut owners: Vec<u32> = Vec::new();
        let mut owned_atoms: Vec<Vec<u32>> = Vec::new();
        let mut spellings: Vec<(&'source [u8], u32)> = Vec::new();
        for site in &self.macro_sites {
            let Some(owner) = self.owner_of(site.span) else {
                continue;
            };
            let interned = match spellings.iter().find(|(known, _)| *known == site.spelling) {
                Some((_, atom)) => *atom,
                None => {
                    let atom = self
                        .facts
                        .intern_atom(site.spelling)
                        .map_err(|_| admission())?;
                    spellings.push((site.spelling, atom));
                    atom
                }
            };
            match owners.iter().position(|known| *known == owner) {
                Some(position) => {
                    if let Some(atoms) = owned_atoms.get_mut(position) {
                        atoms.push(interned);
                    }
                }
                None => {
                    owners.push(owner);
                    owned_atoms.push(vec![interned]);
                }
            }
        }
        for (position, owner) in owners.iter().enumerate() {
            let Some(atoms) = owned_atoms.get(position) else {
                continue;
            };
            let list = self
                .facts
                .intern_atom_list(atoms)
                .map_err(|_| admission())?;
            let Some(row) = self.rows.iter_mut().find(|row| row.ordinal == *owner) else {
                continue;
            };
            row.extension.macros = list;
            let updated = row.extension;
            let ordinal = usize::try_from(*owner).map_err(|_| admission())?;
            self.facts
                .attach_extension(ordinal, EmissionExtension::Rust(updated))
                .map_err(|_| admission())?;
        }
        Ok(())
    }

    /// Emits every oracle-resolved occurrence: method calls, top-level path
    /// references, and macro invocations, each owned by the innermost
    /// containing declaration and measured relative to that owner.
    fn emit_occurrences(&mut self) -> Result<(), RustAuthorityError> {
        let authority = self.authority;
        let method_calls: Vec<_> = authority.method_calls().collect();
        for call in &method_calls {
            let Some(name) = call.syntax.name_ref() else {
                continue;
            };
            let span = authority.span(name.syntax())?;
            let definition = call.target.map(ra_ap_hir::ModuleDef::from);
            self.emit_one_occurrence(
                span,
                ReferenceKind::MethodCall,
                definition,
                OccurrenceConfidence::Oracle,
            )?;
        }
        let paths: Vec<_> = authority.top_level_paths().collect();
        for path in &paths {
            let Some((resolution, _)) = authority.resolve_path(path) else {
                continue;
            };
            let span = authority.span(path.syntax())?;
            let kind = reference_kind(path, &resolution);
            self.emit_one_occurrence(
                span,
                kind,
                path_definition(&resolution),
                OccurrenceConfidence::Oracle,
            )?;
        }
        let sites: Vec<MacroSite<'source>> = self.macro_sites.to_vec();
        for site in &sites {
            let confidence = occurrence_confidence(site.resolved);
            self.emit_one_occurrence(
                site.span,
                ReferenceKind::MacroInvocation,
                None,
                confidence,
            )?;
        }
        Ok(())
    }

    /// Emits one occurrence fact, resolving the target through the pushed
    /// definition table and folding to a self-describing foreign key when
    /// the target lives outside this fragment. Positions with no owning
    /// declaration (module-level `use` items) stay unowned and unemitted.
    fn emit_one_occurrence(
        &mut self,
        span: ByteSpan,
        kind: ReferenceKind,
        definition: Option<ra_ap_hir::ModuleDef>,
        confidence: OccurrenceConfidence,
    ) -> Result<(), RustAuthorityError> {
        let Some(owner) = self.owner_of(span) else {
            return Ok(());
        };
        let owner_span = self
            .rows
            .iter()
            .find(|row| row.ordinal == owner)
            .map(|row| row.span);
        let Some(owner_span) = owner_span else {
            return Ok(());
        };
        let written = self.bytes_of(span)?;
        let target = match definition.and_then(|found| self.ordinal_of_definition(&found)) {
            Some(ordinal) => OccurrenceTarget::Local(EntityId::new(ordinal)),
            None => foreign_universe(written)?,
        };
        let relative = relative_span(span, owner_span)?;
        self.facts
            .push_occurrence(
                owner,
                Occurrence {
                    target,
                    kind,
                    confidence,
                    span: relative,
                },
            )
            .map_err(|_| admission())
    }

    /// Streams every declaration's Rustdoc into borrowed fragments: prose
    /// lines, fenced code, and intra-doc links whose target names a pushed
    /// declaration link locally.
    fn emit_docs(&mut self, declarations: &[Decl]) -> Result<(), RustAuthorityError> {
        for index in 0..declarations.len() {
            let Some(Some(owner)) = self.ordinals.get(index).copied() else {
                continue;
            };
            let declaration = &declarations[index];
            let mut lines: Vec<&'source [u8]> = Vec::new();
            {
                let authority = self.authority;
                let emitter = &*self;
                authority.visit_documentation(&declaration.syntax, |line| {
                    if let Some(span) = emitter.span_of_text(line)
                        && let Ok(bytes) = emitter.bytes_of(span)
                    {
                        lines.push(bytes);
                    }
                });
            }
            self.push_doc_lines(owner, &lines)?;
        }
        Ok(())
    }

    /// Pushes the borrowed doc lines of one declaration as fragments.
    fn push_doc_lines(
        &mut self,
        owner: u32,
        lines: &[&'source [u8]],
    ) -> Result<(), RustAuthorityError> {
        let mut fragments: Vec<DocFragmentInput<'source>> = Vec::new();
        let mut inside_fence = false;
        for line in lines {
            if line.is_empty() {
                if !matches!(fragments.last(), Some(DocFragmentInput::SoftBreak)) {
                    fragments.push(DocFragmentInput::SoftBreak);
                }
                continue;
            }
            if line.starts_with(b"```") {
                inside_fence = !inside_fence;
                continue;
            }
            if inside_fence {
                fragments.push(DocFragmentInput::Code(line));
                continue;
            }
            split_links(line, &mut fragments);
        }
        let locals: Vec<(&'source [u8], u32)> = self
            .rows
            .iter()
            .map(|row| (row.name, row.ordinal))
            .collect();
        for fragment in fragments.iter().copied().map(|fragment| resolve_link(fragment, &locals)) {
            self.facts.push_doc(owner, fragment).map_err(|_| admission())?;
        }
        Ok(())
    }
}

/// Which written position of a callable anchor one child lowers from.
#[derive(Clone, Copy)]
enum CallablePart {
    /// The parameter at this position.
    Parameter(usize),
    /// The return type.
    Return,
}

/// Maps one resolution state onto the confidence lattice: a macro the oracle
/// resolved is oracle tier, an unresolved spelling stays syntactic.
const fn occurrence_confidence(resolved: bool) -> OccurrenceConfidence {
    if resolved {
        OccurrenceConfidence::Oracle
    } else {
        OccurrenceConfidence::Syntactic
    }
}

/// Extracts the written syntax anchor of one binding declaration.
fn written_binding_type(syntax: &SyntaxNode) -> Option<ast::Type> {
    if let Some(item) = ast::Const::cast(syntax.clone()) {
        return item.ty();
    }
    if let Some(item) = ast::Static::cast(syntax.clone()) {
        return item.ty();
    }
    ast::TypeAlias::cast(syntax.clone()).and_then(|item| item.ty())
}

/// Extracts the written child anchor at one position of a parent anchor,
/// or `None` when the parent shape does not match the HIR position.
fn child_anchor(anchor: Option<&ast::Type>, position: usize) -> Option<ast::Type> {
    let anchor = anchor?;
    match anchor {
        ast::Type::PathType(path_type) => {
            let arguments = path_type
                .path()?
                .segment()?
                .generic_arg_list()?
                .generic_args();
            arguments
                .filter_map(|argument| match argument {
                    ast::GenericArg::TypeArg(type_argument) => type_argument.ty(),
                    _ => None,
                })
                .nth(position)
        }
        ast::Type::RefType(reference) => (position == 0).then(|| reference.ty()).flatten(),
        ast::Type::PtrType(pointer) => (position == 0).then(|| pointer.ty()).flatten(),
        ast::Type::ArrayType(array) => (position == 0).then(|| array.ty()).flatten(),
        ast::Type::SliceType(slice) => (position == 0).then(|| slice.ty()).flatten(),
        ast::Type::TupleType(tuple) => tuple.fields().nth(position),
        _ => None,
    }
}

/// Extracts the written anchor of one callable parameter or return position.
fn callable_anchor(anchor: Option<&ast::Type>, part: CallablePart) -> Option<ast::Type> {
    let ast::Type::FnPtrType(pointer) = anchor? else {
        return None;
    };
    match part {
        CallablePart::Return => pointer.ret_type().and_then(|ret| ret.ty()),
        CallablePart::Parameter(position) => {
            let params = pointer.param_list()?;
            let mut seen = 0usize;
            for param in params.params() {
                let Some(ty) = param.ty() else {
                    continue;
                };
                if seen == position {
                    return Some(ty);
                }
                seen += 1;
            }
            None
        }
    }
}

/// Maps one HIR receiver access onto the ownership lattice.
const fn receiver_ownership(access: ra_ap_hir::Access) -> RustOwnership {
    match access {
        ra_ap_hir::Access::Shared => RustOwnership::SharedBorrow,
        ra_ap_hir::Access::Exclusive => RustOwnership::MutableBorrow,
        ra_ap_hir::Access::Owned => RustOwnership::Value,
    }
}

/// Maps one parameter binding onto the ownership lattice: borrows keep their
/// shared or mutable cell, by-value bindings of copy types are values, and
/// every other by-value binding moves.
fn parameter_ownership(
    database: &ra_ap_ide_db::RootDatabase,
    semantic: &ra_ap_hir::Type<'_>,
) -> RustOwnership {
    if let Some((_, mutability)) = semantic.as_reference() {
        return match mutability {
            ra_ap_hir::Mutability::Mut => RustOwnership::MutableBorrow,
            ra_ap_hir::Mutability::Shared => RustOwnership::SharedBorrow,
        };
    }
    if semantic.is_copy(database) {
        RustOwnership::Value
    } else {
        RustOwnership::Moved
    }
}

/// Maps one closed declaration kind onto the entity lattice.
const fn entity_kind(kind: SemanticKind) -> Result<EntityKind, RustAuthorityError> {
    match kind {
        SemanticKind::Module => Ok(EntityKind::Module),
        SemanticKind::Function => Ok(EntityKind::Function),
        SemanticKind::Field => Ok(EntityKind::Field),
        SemanticKind::Record => Ok(EntityKind::Record),
        SemanticKind::Enum => Ok(EntityKind::Enum),
        SemanticKind::Trait => Ok(EntityKind::Trait),
        SemanticKind::Implementation => Ok(EntityKind::Implementation),
        SemanticKind::TypeAlias => Ok(EntityKind::Alias),
        SemanticKind::Constant => Ok(EntityKind::Constant),
        SemanticKind::Static => Ok(EntityKind::Static),
        SemanticKind::Variant => Ok(EntityKind::Variant),
        SemanticKind::Macro
        | SemanticKind::LocalBinding
        | SemanticKind::GenericParameter
        | SemanticKind::Builtin => Err(RustAuthorityError::MissingSemanticFact { fact: kind }),
    }
}

/// Maps one closed declaration kind onto its product constructor.
const fn constructor(
    kind: SemanticKind,
) -> Result<SemanticProductConstructor, RustAuthorityError> {
    match kind {
        SemanticKind::Function => Ok(SemanticProductConstructor::function(0, 0)),
        SemanticKind::Record
        | SemanticKind::Module
        | SemanticKind::Field
        | SemanticKind::Implementation => Ok(SemanticProductConstructor::PRODUCT),
        SemanticKind::Enum => Ok(SemanticProductConstructor::UNION),
        SemanticKind::Trait => Ok(SemanticProductConstructor::INTERSECTION),
        SemanticKind::TypeAlias
        | SemanticKind::Constant
        | SemanticKind::Static
        | SemanticKind::Variant => Ok(LEAF_PRODUCT),
        SemanticKind::Macro
        | SemanticKind::LocalBinding
        | SemanticKind::GenericParameter
        | SemanticKind::Builtin => Err(RustAuthorityError::MissingSemanticFact { fact: kind }),
    }
}

/// Maps one pushed HIR definition onto its `ModuleDef` key.
fn module_def(definition: &RustDefinition) -> Option<ra_ap_hir::ModuleDef> {
    match definition {
        RustDefinition::Field(_)
        | RustDefinition::Implementation(_)
        | RustDefinition::Macro(_) => None,
        RustDefinition::Variant(variant) => Some(ra_ap_hir::ModuleDef::from(*variant)),
        RustDefinition::Module(module) => Some(ra_ap_hir::ModuleDef::from(*module)),
        RustDefinition::Trait(trait_) => Some(ra_ap_hir::ModuleDef::from(*trait_)),
        RustDefinition::Function(function) => Some(ra_ap_hir::ModuleDef::from(*function)),
        RustDefinition::Record(adt) | RustDefinition::Enum(adt) => {
            Some(ra_ap_hir::ModuleDef::from(*adt))
        }
        RustDefinition::TypeAlias(alias) => Some(ra_ap_hir::ModuleDef::from(*alias)),
        RustDefinition::Constant(constant) => Some(ra_ap_hir::ModuleDef::from(*constant)),
        RustDefinition::Static(static_) => Some(ra_ap_hir::ModuleDef::from(*static_)),
    }
}

/// Categorizes one resolved path reference: imports, type positions, calls,
/// and value uses, by the path's written syntax position and resolution.
fn reference_kind(path: &ast::Path, resolution: &ra_ap_hir::PathResolution) -> ReferenceKind {
    if let Some(parent) = path.syntax().parent()
        && parent.kind() == ra_ap_syntax::SyntaxKind::USE_TREE
    {
        return ReferenceKind::Import;
    }
    match resolution {
        ra_ap_hir::PathResolution::Def(ra_ap_hir::ModuleDef::Function(_)) => {
            if is_call_position(path) {
                ReferenceKind::FunctionCall
            } else {
                ReferenceKind::VariableUse
            }
        }
        ra_ap_hir::PathResolution::Def(
            ra_ap_hir::ModuleDef::Adt(_)
            | ra_ap_hir::ModuleDef::Trait(_)
            | ra_ap_hir::ModuleDef::TypeAlias(_)
            | ra_ap_hir::ModuleDef::BuiltinType(_)
            | ra_ap_hir::ModuleDef::Module(_),
        ) => ReferenceKind::TypeReference,
        _ => ReferenceKind::VariableUse,
    }
}

/// True when the path sits in the callee position of a call expression.
fn is_call_position(path: &ast::Path) -> bool {
    let Some(parent) = path.syntax().parent() else {
        return false;
    };
    let Some(ast::Expr::PathExpr(path_expr)) = ast::Expr::cast(parent) else {
        return false;
    };
    let Some(grandparent) = path_expr.syntax().parent() else {
        return false;
    };
    let Some(call) = ast::CallExpr::cast(grandparent) else {
        return false;
    };
    let Some(callee) = call.expr() else {
        return false;
    };
    callee.syntax().text_range() == path_expr.syntax().text_range()
}

/// Extracts the resolved definition of one path, when it names a declaration.
const fn path_definition(resolution: &ra_ap_hir::PathResolution) -> Option<ra_ap_hir::ModuleDef> {
    match resolution {
        ra_ap_hir::PathResolution::Def(module_def) => Some(*module_def),
        _ => None,
    }
}

/// Builds one self-describing foreign key outside every package lineage,
/// carrying the exact written spelling of the reference site.
fn foreign_universe<'source>(
    written: &'source [u8],
) -> Result<OccurrenceTarget<'source>, RustAuthorityError> {
    let path = core::str::from_utf8(written).unwrap_or("");
    let usable = if path.is_empty() { "unresolved" } else { path };
    let key = ForeignKey::new(
        ForeignOrigin::Universe {
            ecosystem: CARGO_ECOSYSTEM,
        },
        usable,
        path,
        None,
    )
    .map_err(|_| admission())?;
    Ok(OccurrenceTarget::Foreign(key))
}

/// Projects one absolute occurrence span onto its owner's span start. The
/// owner filter proved containment, so both subtractions stay in range.
fn relative_span(span: ByteSpan, owner: ByteSpan) -> Result<RelSpan, RustAuthorityError> {
    let start = span.start.checked_sub(owner.start).ok_or_else(admission)?;
    let end = span.end.checked_sub(owner.start).ok_or_else(admission)?;
    RelSpan::new(start, end).map_err(|_| admission())
}

/// One primitive row with the frozen `repr(u32)` shape discriminant.
#[expect(
    clippy::as_conversions,
    reason = "the lattice freezes PrimitiveShape as a repr(u32) wire discriminant"
)]
fn primitive_record(shape: PrimitiveShape, payload1: u32) -> SemanticTypeRecord<'static> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::Primitive,
        payload0: shape as u32,
        payload1,
        text: None,
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// One exact-width integer row, packed per the frozen integer-cell encoding.
fn integer_record(width: TypeWidth, signed: bool) -> SemanticTypeRecord<'static> {
    let mut payload1 = width.to_cell() << INTEGER_WIDTH_SHIFT;
    if signed {
        payload1 |= SemanticTypeRecord::INTEGER_SIGNED_FLAG;
    }
    primitive_record(PrimitiveShape::Integer, payload1)
}

/// Maps one HIR builtin onto its exact scalar row by its exact spelling,
/// because the HIR facade wraps the builtin without width introspection.
fn builtin_record(builtin: ra_ap_hir::BuiltinType) -> SemanticTypeRecord<'static> {
    let name = builtin.name();
    match name.as_str() {
        "bool" => primitive_record(PrimitiveShape::Bool, 0),
        "char" => primitive_record(PrimitiveShape::Char, 0),
        "str" => primitive_record(PrimitiveShape::Str, 0),
        "isize" => integer_record(TypeWidth::Arch, true),
        "usize" => integer_record(TypeWidth::Arch, false),
        "i8" => integer_record(TypeWidth::Fixed(8), true),
        "i16" => integer_record(TypeWidth::Fixed(16), true),
        "i32" => integer_record(TypeWidth::Fixed(32), true),
        "i64" => integer_record(TypeWidth::Fixed(64), true),
        "i128" => integer_record(TypeWidth::Fixed(128), true),
        "u8" => integer_record(TypeWidth::Fixed(8), false),
        "u16" => integer_record(TypeWidth::Fixed(16), false),
        "u32" => integer_record(TypeWidth::Fixed(32), false),
        "u64" => integer_record(TypeWidth::Fixed(64), false),
        "u128" => integer_record(TypeWidth::Fixed(128), false),
        "f16" => primitive_record(PrimitiveShape::Float, TypeWidth::Fixed(16).to_cell()),
        "f32" => primitive_record(PrimitiveShape::Float, TypeWidth::Fixed(32).to_cell()),
        "f64" => primitive_record(PrimitiveShape::Float, TypeWidth::Fixed(64).to_cell()),
        "f128" => primitive_record(PrimitiveShape::Float, TypeWidth::Fixed(128).to_cell()),
        _ => unknown_record(TypeReason::OracleGap, None),
    }
}

/// One honest unknown row; the text cell is present exactly when the reason
/// demands its spelling, per the closed lattice's cell laws.
#[expect(
    clippy::as_conversions,
    reason = "the lattice freezes TypeReason as a repr(u32) wire discriminant"
)]
fn unknown_record(reason: TypeReason, text: Option<&[u8]>) -> SemanticTypeRecord<'_> {
    SemanticTypeRecord {
        tag: SemanticTypeTag::Unknown,
        payload0: reason as u32,
        payload1: 0,
        text,
        text2: None,
        nominal: None,
        children: ListSpan::new(0, 0),
    }
}

/// Splits one doc line into prose runs and intra-doc links; a link target
/// naming a pushed declaration resolves in [`Emitter::push_doc_lines`].
fn split_links<'source>(line: &'source [u8], fragments: &mut Vec<DocFragmentInput<'source>>) {
    let mut cursor = 0usize;
    while let Some(at) = find(line, b"[`", cursor) {
        let Some(open) = at.checked_add(2) else {
            break;
        };
        let Some(close) = find(line, b"`]", open).and_then(|close| close.checked_add(2)) else {
            break;
        };
        if at > cursor
            && let Some(prose) = line.get(cursor..at)
        {
            fragments.push(DocFragmentInput::Text(prose));
        }
        if let Some(name) = line.get(open..close - 2).filter(|name| !name.is_empty()) {
            fragments.push(DocFragmentInput::Link {
                label: name,
                target: DocLinkTarget::Foreign {
                    ecosystem: CARGO_ECOSYSTEM.as_bytes(),
                    path: name,
                },
            });
        }
        cursor = close;
    }
    if let Some(rest) = line.get(cursor..)
        && !rest.is_empty()
    {
        fragments.push(DocFragmentInput::Text(rest));
    }
}

/// Rewrites a doc link whose target names a pushed declaration to the exact
/// local entity row; every other target keeps its written spelling.
fn resolve_link<'source>(
    fragment: DocFragmentInput<'source>,
    rows: &[(&'source [u8], u32)],
) -> DocFragmentInput<'source> {
    let DocFragmentInput::Link { label, target } = fragment else {
        return fragment;
    };
    let DocLinkTarget::Foreign { path, .. } = target else {
        return DocFragmentInput::Link { label, target };
    };
    match rows.iter().find(|(known, _)| *known == path) {
        Some((_, ordinal)) => DocFragmentInput::Link {
            label,
            target: DocLinkTarget::Local(EntityId::new(*ordinal)),
        },
        None => DocFragmentInput::Link { label, target },
    }
}

/// Finds the first position of `needle` in `haystack` at or after `from`.
fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    let Some(last) = haystack.len().checked_sub(needle.len()) else {
        return None;
    };
    let mut at = from;
    while at <= last {
        let Some(window) = haystack.get(at..at.checked_add(needle.len())?) else {
            return None;
        };
        if window == needle {
            return Some(at);
        }
        at += 1;
    }
    None
}
