//! Projects validated `go/packages` authority images through the shared
//! canonical fact lane at complete semantic fidelity.
//!
//! Every plane the image carries is projected: declaration facts, the
//! recursive type graph, executable signatures with parameter and result
//! carrier facts, resolved call occurrences, doc fragments, and the Go
//! extension pool (signatures, type parameters, fields, method sets, build
//! constraints). Never recovers Go facts by scanning source text or a native
//! parser fallback.
//!
//! Emission is two-pass, declarations-first. Pass one commits every named
//! type (its recursive terminal is the legal diagonal self-nominal) and then
//! every alias, so pass two — fields, interface methods, methods, package
//! functions, constants, and variables — projects every type reference
//! against backward fact ordinals only. Constrained declarations, docs, and
//! references follow in passes three through five.
//!
//! Compound types are interned in the anonymous type-row pool, never as
//! carrier facts. The frozen wire orders anonymous rows before fact rows and
//! rejects forward child targets, which yields one projection law this
//! module obeys everywhere: an anonymous row may reference only earlier
//! anonymous rows, while fact rows may reference anonymous rows and earlier
//! facts. A named reference reached at anonymous depth therefore folds to a
//! typed `Unknown` row retaining the name spelling, and a generic
//! application can be carried only as a fact's own root record.
//!
//! Positions the lane cannot host fold to typed `Unknown` rows that keep
//! their reason cell: universe and foreign nominals (`error`, `comparable`,
//! other packages), unconstrained inline interfaces, and depth-limit
//! truncation. Image rows carry no parameter-name plane, so carrier facts
//! use Go's blank identifier; receiver spelling and pointer-receiver bits
//! have no `GoFacts` cell and stay image-only facts.

use core::str;

use compiler_ir::{
    AtomListId, DocFragmentInput, EntityId, EntityKind, EntityListId, ForeignKey, ForeignKeyFault,
    ForeignOrigin, GoFacts, GoSignature, NominalRef, Occurrence, OccurrenceConfidence,
    OccurrenceTarget, PackageLineage, PackageLineageFault, ProductChildRole, ReferenceKind,
    RelSpan, RelSpanFault, SemanticProductConstructor, SemanticTypeRecord, SemanticTypeTag,
    TypeListId, TypeParameterListId, TypeReason, TypeWidth,
};
use compiler_languages_go::{
    ChanDir, Declaration, DeclarationKind, DocOwner, GoImage, ImageError, MemberKind, TypeRowKind,
    parse_constraint_blob,
};
use compiler_vocabulary::LoweringUnsupported;
use sha2::{Digest, Sha256};

use crate::lower::{
    EmissionExtension, FactFault, FactSet, LEAF_PRODUCT, MAX_REF_LIST_ELEMENTS, SemanticFact,
    push_fact,
};

/// Exact rejection while borrowing one validated Go authority image.
///
/// The shared driver failure match owns the terminal arms and lies outside
/// this module's ownership, so this enum keeps exactly the three variants
/// the frozen match already names; every projection fault folds onto
/// [`GoCollectError::Lowering`] at the boundary with its operands retained
/// in [`ProjectionFault`].
#[derive(Debug)]
pub(crate) enum GoCollectError {
    /// The fixed binary image failed structural or checksum validation.
    Image(ImageError),
    /// The image's producer-bound source digest differs from the compile source.
    SourceBinding {
        /// SHA-256 of the exact compile request bytes.
        expected: [u8; 32],
        /// SHA-256 lent by the Go authority image header.
        observed: [u8; 32],
    },
    /// The bounded canonical lane cannot admit every authority fact.
    Lowering(LoweringUnsupported),
}

/// Exact projection fault retained until the collect boundary folds it into
/// the lane's closed terminal. Operands stay named so the fold site remains
/// typed; widening [`GoCollectError`] requires extending the frozen driver
/// failure match and is recorded as a lane criticism in the module review.
#[derive(Debug)]
enum ProjectionFault<'image> {
    /// The recursive type graph exceeded the producer depth budget.
    Depth,
    /// No pushed fact existed to own an anonymous compound row.
    Anchor,
    /// A pooled field or method list exceeded its bounded width.
    ListCapacity,
    /// A same-package reference named no declared function.
    OrphanTarget {
        /// The unresolved target spelling.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        function: &'image [u8],
    },
    /// A foreign key could not be built for a resolved external target.
    ForeignKey(
        /// The exact foreign-key rejection.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        ForeignKeyFault,
    ),
    /// The `go` package lineage was rejected.
    Lineage(
        /// The exact lineage rejection.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        PackageLineageFault,
    ),
    /// An image atom was not UTF-8 although the image validated its planes.
    Utf8,
    /// A reference span was inverted although the image proved containment.
    Span(
        /// The exact relative-span rejection.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        RelSpanFault,
    ),
    /// A doc, method, or member row named no pushed owner fact.
    OrphanOwner {
        /// The image row index whose owner was never pushed.
        #[expect(
            dead_code,
            reason = "operands are retained for typed diagnostics; the collect boundary folds every class to the lane's closed terminal"
        )]
        owner: u32,
    },
}

/// Folds one projection fault into the lane's closed terminal. The shared
/// driver failure match owns the terminal arms and is outside this module's
/// ownership, so operand-preserving Go terminals stay folded here.
fn terminal(fault: ProjectionFault<'_>) -> GoCollectError {
    let _ = fault;
    GoCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration)
}

/// Folds one bounded-lane fact rejection into the lane's closed terminal.
fn lane_terminal(fault: FactFault) -> GoCollectError {
    let _ = fault;
    GoCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration)
}

/// Admits one fact and returns its proven backward ordinal.
fn push<'source>(
    facts: &mut FactSet<'source>,
    fact: SemanticFact<'source>,
) -> Result<u32, GoCollectError> {
    let ordinal = push_fact(facts, fact).map_err(GoCollectError::Lowering)?;
    u32::try_from(ordinal)
        .map_err(|_| GoCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration))
}

/// Producer depth budget of the recursive type graph. Go type cycles always
/// pass through a named intermediary, so a structural cycle is hostile and
/// the budget is the typed defense.
const DEPTH_LIMIT: usize = 64;

/// Carrier-fact name for the unnamed parameters and results the image's
/// type plane carries no spellings for.
const UNNAMED: &[u8] = b"_";

/// Fixed array text cell; the exact length travels in the payload cells.
const ARRAY_BRACKET: &[u8] = b"[]";

/// `PrimitiveShape::Integer` wire cell.
const SHAPE_INTEGER: u32 = 0;
/// `PrimitiveShape::Float` wire cell.
const SHAPE_FLOAT: u32 = 1;
/// `PrimitiveShape::Bool` wire cell.
const SHAPE_BOOL: u32 = 2;
/// `PrimitiveShape::Str` wire cell.
const SHAPE_STR: u32 = 4;
/// `PrimitiveShape::MutPointer` wire cell.
const SHAPE_MUT_POINTER: u32 = 5;
/// `PrimitiveShape::Builtin` wire cell.
const SHAPE_BUILTIN: u32 = 8;

/// Integer signedness bit below the shifted width cell.
const INTEGER_SIGNED_FLAG: u32 = 1;
/// Bit offset of the integer width cell above the signedness bit.
const INTEGER_WIDTH_SHIFT: u32 = 1;

/// Channel base spellings by image direction cell.
const CHAN_BOTH: &[u8] = b"chan";
const CHAN_SEND: &[u8] = b"chan<-";
const CHAN_RECV: &[u8] = b"<-chan";

/// Go ecosystem name of every foreign package lineage.
const ECOSYSTEM: &str = "go";

/// Streams the complete Go semantic plane — declarations, recursive types,
/// signatures, occurrences, docs, and the Go extension pool — into the
/// shared fact lane.
pub(crate) fn collect<'source>(
    source: &'source [u8],
    image_bytes: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), GoCollectError> {
    let image = GoImage::open(image_bytes).map_err(GoCollectError::Image)?;
    let expected = Sha256::digest(source).into();
    let observed = image.source_digest();
    if observed != expected {
        return Err(GoCollectError::SourceBinding { expected, observed });
    }
    // Intern the three empty pooled lists first so the default
    // `::new(0)` coordinates always name genuine empty lists.
    let _ = facts.intern_atom_list(&[]).map_err(lane_terminal)?;
    let _ = facts.intern_type_list(&[]).map_err(lane_terminal)?;
    let _ = facts.intern_entity_list(&[]).map_err(lane_terminal)?;

    let mut go = Projector::new(image, facts);
    // Pass one: named types, then aliases — the backward anchors every
    // later type reference resolves against.
    for index in 0..go.image.declaration_count() {
        let declaration = go.image.declaration(index).map_err(GoCollectError::Image)?;
        if declaration.kind == DeclarationKind::Type {
            go.named_type(index, &declaration)?;
        }
    }
    for index in 0..go.image.declaration_count() {
        let declaration = go.image.declaration(index).map_err(GoCollectError::Image)?;
        if declaration.kind == DeclarationKind::Alias {
            go.alias(&declaration)?;
        }
    }
    // Pass two: member planes, executables, and values in producer order.
    for index in 0..go.image.declaration_count() {
        let declaration = go.image.declaration(index).map_err(GoCollectError::Image)?;
        match declaration.kind {
            DeclarationKind::Type => go.type_family(index, &declaration)?,
            DeclarationKind::Function => go.function(&declaration)?,
            DeclarationKind::Constant | DeclarationKind::Static => go.value(&declaration)?,
            DeclarationKind::Alias => {}
        }
    }
    // Pass three: declarations excluded by build constraints.
    go.constraints()?;
    // Pass four: documentation fragments.
    go.docs()?;
    // Pass five: resolved call occurrences.
    go.occurrences()?;
    Ok(())
}

/// Maps one closed declaration kind onto the canonical entity kind.
const fn entity_kind(kind: DeclarationKind) -> EntityKind {
    match kind {
        DeclarationKind::Type => EntityKind::Record,
        DeclarationKind::Alias => EntityKind::Alias,
        DeclarationKind::Function => EntityKind::Function,
        DeclarationKind::Constant => EntityKind::Constant,
        DeclarationKind::Static => EntityKind::Static,
    }
}

const fn constructor(kind: EntityKind) -> SemanticProductConstructor {
    match kind {
        EntityKind::Function => SemanticProductConstructor::function(0, 0),
        EntityKind::Record => SemanticProductConstructor::PRODUCT,
        EntityKind::Trait => SemanticProductConstructor::INTERSECTION,
        EntityKind::Enum => SemanticProductConstructor::UNION,
        EntityKind::Constant
        | EntityKind::Module
        | EntityKind::Field
        | EntityKind::Alias
        | EntityKind::Implementation
        | EntityKind::Variant
        | EntityKind::Static
        | EntityKind::Reexport
        | EntityKind::Parameter => LEAF_PRODUCT,
    }
}

/// The typed unknown record for one reason and optional spelling. `Unknown`
/// rows whose reason carries a spelling require the text cell; the others
/// forbid it.
const fn unknown_record(reason: TypeReason, spelling: Option<&[u8]>) -> SemanticTypeRecord<'_> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Unknown);
    record.payload0 = match reason {
        TypeReason::Unannotated => 0,
        TypeReason::DynamicallyTyped => 1,
        TypeReason::UnresolvedLocalName => 2,
        TypeReason::UnresolvedExternal => 3,
        TypeReason::TruncatedAtDepthLimit => 4,
        TypeReason::OracleGap => 5,
        TypeReason::NoIrRepresentation => 6,
    };
    record.text = spelling;
    record
}

/// The recursive terminal: a nominal row naming the fact's own ordinal.
const fn nominal_record(ordinal: u32) -> SemanticTypeRecord<'static> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
    record.nominal = Some(NominalRef::Local(EntityId::new(ordinal)));
    record
}

/// One projected type prepared for a fact: the lattice record plus its
/// ordered child coordinates with their anonymous-record labels.
struct RootType<'image> {
    record: SemanticTypeRecord<'image>,
    children: Vec<TypeChild<'image>>,
}

/// One projected child coordinate: an anonymous row or a backward fact.
struct TypeChild<'image> {
    target: u32,
    name: Option<&'image [u8]>,
    flags: u8,
}

impl<'image> RootType<'image> {
    /// Wraps one childless lattice record.
    const fn leaf(record: SemanticTypeRecord<'image>) -> Self {
        Self {
            record,
            children: Vec::new(),
        }
    }

    /// Commits the projection onto one fact under construction.
    fn attach(self, fact: SemanticFact<'image>) -> SemanticFact<'image> {
        let mut fact = fact.typed(self.record);
        for child in self.children {
            fact = fact.type_child(child.target, child.name, child.flags);
        }
        fact
    }
}

/// One anonymous type row under construction: the lattice record plus the
/// child coordinates appended into the pooled lane before interning.
struct AnonRow<'image> {
    record: SemanticTypeRecord<'image>,
    children: Vec<TypeChild<'image>>,
}

/// The two-pass Go projector over one validated authority image.
struct Projector<'x, 'source> {
    image: GoImage<'source>,
    facts: &'x mut FactSet<'source>,
    /// The image's package import path, the local side of every named
    /// reference resolution.
    package: &'source [u8],
    /// Lane ordinal per image declaration index.
    declaration_ordinals: Vec<Option<u32>>,
    /// Lane ordinal per image method row index.
    method_ordinals: Vec<Option<u32>>,
    /// Lane ordinal per image member row index.
    member_ordinals: Vec<Option<u32>>,
    /// Declared names to already-pushed fact ordinals.
    names: Vec<(&'source [u8], u32)>,
    /// Memoized anonymous-context coordinates per image type row.
    anonymous: Vec<Option<u32>>,
}

impl<'x, 'source> Projector<'x, 'source> {
    fn new(image: GoImage<'source>, facts: &'x mut FactSet<'source>) -> Self {
        Self {
            package: image
                .declaration(0)
                .map(|declaration| declaration.package)
                .unwrap_or(&[]),
            declaration_ordinals: vec![None; image.declaration_count()],
            method_ordinals: vec![None; image.method_count()],
            member_ordinals: vec![None; image.member_count()],
            anonymous: vec![None; image.type_count()],
            image,
            facts,
            names: Vec::new(),
        }
    }

    /// Records one pushed declaration name for later resolution.
    fn record_name(&mut self, name: &'source [u8], ordinal: u32) {
        self.names.push((name, ordinal));
    }

    /// Resolves one declared name to its pushed fact ordinal.
    fn lookup(&self, name: &[u8]) -> Option<u32> {
        self.names
            .iter()
            .find(|(known, _)| *known == name)
            .map(|(_, ordinal)| *ordinal)
    }

    /// The already-pushed fact that owns anonymous rows interned for the
    /// fact currently being built.
    fn anchor(&self) -> Result<u32, ProjectionFault<'source>> {
        u32::try_from(self.facts.len())
            .ok()
            .and_then(|len| len.checked_sub(1))
            .ok_or(ProjectionFault::Anchor)
    }

    /// Pass one: one named type with its recursive diagonal self-nominal.
    fn named_type(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let own = u32::try_from(self.facts.len())
            .map_err(|_| GoCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration))?;
        let fact = SemanticFact::new(
            EntityKind::Record,
            declaration.name,
            SemanticProductConstructor::PRODUCT,
        )
        .typed(nominal_record(own));
        let ordinal = push(self.facts, fact)?;
        self.declaration_ordinals[index] = Some(ordinal);
        self.record_name(declaration.name, ordinal);
        Ok(())
    }

    /// Pass one: one alias whose declared type is its projected target.
    /// Targets referencing later declarations fold to typed unknowns
    /// because the lane admits strictly backward references only.
    fn alias(&mut self, declaration: &Declaration<'source>) -> Result<(), GoCollectError> {
        let root = self.root(declaration.type_root, TypeReason::OracleGap)?;
        let fact = root.attach(SemanticFact::new(
            EntityKind::Alias,
            declaration.name,
            constructor(EntityKind::Alias),
        ));
        let ordinal = push(self.facts, fact)?;
        self.record_name(declaration.name, ordinal);
        Ok(())
    }

    /// Pass two: one named type's member planes — its type parameters, its
    /// struct fields or interface method signatures, its methods, and the
    /// Go extension row binding them to the type fact.
    fn type_family(
        &mut self,
        index: usize,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        let Some(type_ordinal) = self.declaration_ordinals[index] else {
            return Err(terminal(ProjectionFault::OrphanOwner {
                owner: u32::try_from(index).unwrap_or(u32::MAX),
            }));
        };
        let parameter_start = self.facts.type_parameter_len.try_into().unwrap_or(u32::MAX);
        self.type_parameters(index)?;
        let mut fields = Vec::new();
        let mut interface_methods = Vec::new();
        if let Some(root_cell) = declaration.type_root {
            let row = self
                .image
                .type_row(index_of(root_cell))
                .map_err(GoCollectError::Image)?;
            match row.kind {
                TypeRowKind::Struct => self.fields(&row, &mut fields)?,
                TypeRowKind::Interface => self.interface_methods(&row, &mut interface_methods)?,
                _ => {}
            }
        }
        let mut methods = Vec::new();
        for method_index in 0..self.image.method_count() {
            let method = self
                .image
                .method(method_index)
                .map_err(GoCollectError::Image)?;
            if usize::try_from(method.owner).is_ok_and(|owner| owner != index) {
                continue;
            }
            let receiver_start = self.facts.type_parameter_len.try_into().unwrap_or(u32::MAX);
            for name in blank_separated(method.receiver_type_params) {
                self.facts
                    .push_type_parameter(name, None, None)
                    .map_err(lane_terminal)?;
            }
            let ordinal = self.executable(method.name, method.type_root, receiver_start)?;
            methods.push(ordinal);
            self.method_ordinals[method_index] = Some(ordinal);
        }
        let fields_list = self.entity_list(&fields)?;
        let method_set = self.entity_list(&methods)?;
        self.facts
            .attach_extension(
                usize::try_from(type_ordinal).unwrap_or(usize::MAX),
                EmissionExtension::Go(GoFacts {
                    signature: GoSignature {
                        parameters: TypeListId::new(0),
                        results: TypeListId::new(0),
                        variadic: false,
                    },
                    type_parameters: TypeParameterListId::new(parameter_start),
                    fields: fields_list,
                    method_set,
                    build_constraints: AtomListId::new(0),
                }),
            )
            .map_err(lane_terminal)?;
        Ok(())
    }

    /// Pushes the pooled type-parameter rows of one declaration. A
    /// constraint resolves to its fact ordinal only when it is a pure local
    /// named row; inline interface constraints have no fact to name and
    /// stay empty on the pooled row.
    fn type_parameters(&mut self, index: usize) -> Result<(), GoCollectError> {
        for parameter_index in 0..self.image.type_parameter_count() {
            let row = self
                .image
                .type_parameter(parameter_index)
                .map_err(GoCollectError::Image)?;
            if usize::try_from(row.owner).is_ok_and(|owner| owner != index) {
                continue;
            }
            let constraint = row
                .constraint
                .and_then(|root| self.image.type_row(index_of(root)).ok())
                .filter(|row| {
                    matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias)
                        && row.children.1 == 0
                        && row.package == self.package
                })
                .and_then(|row| self.lookup(row.name));
            self.facts
                .push_type_parameter(row.name, constraint, None)
                .map_err(lane_terminal)?;
        }
        Ok(())
    }

    /// Pushes one fact per struct field, projecting each field type as the
    /// field fact's root record.
    fn fields(
        &mut self,
        row: &compiler_languages_go::TypeRow<'source>,
        fields: &mut Vec<u32>,
    ) -> Result<(), GoCollectError> {
        for member_index in member_run(row) {
            let member = self
                .image
                .member(member_index)
                .map_err(GoCollectError::Image)?;
            if member.kind != MemberKind::Field {
                continue;
            }
            let root = self.root(member.type_root, TypeReason::OracleGap)?;
            let fact = root.attach(SemanticFact::new(
                EntityKind::Field,
                member.name,
                LEAF_PRODUCT,
            ));
            let ordinal = push(self.facts, fact)?;
            fields.push(ordinal);
            self.member_ordinals[member_index] = Some(ordinal);
        }
        Ok(())
    }

    /// Pushes one function fact per interface method signature.
    fn interface_methods(
        &mut self,
        row: &compiler_languages_go::TypeRow<'source>,
        methods: &mut Vec<u32>,
    ) -> Result<(), GoCollectError> {
        for member_index in member_run(row) {
            let member = self
                .image
                .member(member_index)
                .map_err(GoCollectError::Image)?;
            if member.kind != MemberKind::Method {
                continue;
            }
            let start = self.facts.type_parameter_len.try_into().unwrap_or(u32::MAX);
            let ordinal = self.executable(member.name, member.type_root, start)?;
            methods.push(ordinal);
            self.member_ordinals[member_index] = Some(ordinal);
        }
        Ok(())
    }

    /// Pass two: one package-level function.
    fn function(&mut self, declaration: &Declaration<'source>) -> Result<(), GoCollectError> {
        let parameter_start = self.facts.type_parameter_len.try_into().unwrap_or(u32::MAX);
        self.type_parameters_fn(declaration)?;
        let ordinal = self.executable(declaration.name, declaration.type_root, parameter_start)?;
        self.record_name(declaration.name, ordinal);
        Ok(())
    }

    /// Pushes the pooled type-parameter rows of one function declaration.
    fn type_parameters_fn(
        &mut self,
        declaration: &Declaration<'source>,
    ) -> Result<(), GoCollectError> {
        for index in 0..self.image.declaration_count() {
            let declared = self
                .image
                .declaration(index)
                .map_err(GoCollectError::Image)?;
            if declared.name != declaration.name || declared.kind != declaration.kind {
                continue;
            }
            return self.type_parameters(index);
        }
        Ok(())
    }

    /// Pass two: one constant or variable with its projected declared type.
    fn value(&mut self, declaration: &Declaration<'source>) -> Result<(), GoCollectError> {
        let kind = entity_kind(declaration.kind);
        let root = self.root(declaration.type_root, TypeReason::Unannotated)?;
        let fact = root.attach(SemanticFact::new(kind, declaration.name, constructor(kind)));
        let ordinal = push(self.facts, fact)?;
        self.record_name(declaration.name, ordinal);
        Ok(())
    }

    /// Pass three: one fact per declaration excluded by a build constraint,
    /// carrying the normalized constraint expression on its Go extension
    /// row.
    fn constraints(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.constraint_count() {
            let row = self
                .image
                .constraint(index)
                .map_err(GoCollectError::Image)?;
            let exported =
                parse_constraint_blob(row.exported, row.exported_count).ok_or_else(|| {
                    terminal(ProjectionFault::OrphanOwner {
                        owner: u32::try_from(index).unwrap_or(u32::MAX),
                    })
                })?;
            let atom = self
                .facts
                .intern_atom(row.constraint)
                .map_err(lane_terminal)?;
            let list = self
                .facts
                .intern_atom_list(core::slice::from_ref(&atom))
                .map_err(lane_terminal)?;
            for declaration in exported {
                let kind = entity_kind(declaration.kind);
                let fact = SemanticFact::new(kind, declaration.name, constructor(kind))
                    .typed(unknown_record(TypeReason::OracleGap, None))
                    .with_extension(EmissionExtension::Go(GoFacts {
                        signature: GoSignature {
                            parameters: TypeListId::new(0),
                            results: TypeListId::new(0),
                            variadic: false,
                        },
                        type_parameters: TypeParameterListId::new(0),
                        fields: EntityListId::new(0),
                        method_set: EntityListId::new(0),
                        build_constraints: list,
                    }));
                push(self.facts, fact)?;
            }
        }
        Ok(())
    }

    /// Pass four: one text run per documentation line, soft-break
    /// separated, owned by the pushed fact of the row's owner.
    fn docs(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.doc_count() {
            let row = self.image.doc(index).map_err(GoCollectError::Image)?;
            let owner = match row.owner_kind {
                DocOwner::Declaration => self
                    .declaration_ordinals
                    .get(index_of(row.owner))
                    .copied()
                    .flatten(),
                DocOwner::Method => self
                    .method_ordinals
                    .get(index_of(row.owner))
                    .copied()
                    .flatten(),
                DocOwner::Member => self
                    .member_ordinals
                    .get(index_of(row.owner))
                    .copied()
                    .flatten(),
            }
            .ok_or_else(|| terminal(ProjectionFault::OrphanOwner { owner: row.owner }))?;
            push_doc_lines(self.facts, owner, row.text).map_err(lane_terminal)?;
        }
        Ok(())
    }

    /// Pass five: one resolved call occurrence per reference row, local
    /// when the target declares in this package and otherwise a foreign
    /// `go` package key, both at oracle confidence over owner-relative
    /// spans.
    fn occurrences(&mut self) -> Result<(), GoCollectError> {
        for index in 0..self.image.reference_count() {
            let row = self.image.reference(index).map_err(GoCollectError::Image)?;
            let owner = if row.owner_is_declaration {
                self.declaration_ordinals
                    .get(index_of(row.owner_row))
                    .copied()
                    .flatten()
            } else {
                self.method_ordinals
                    .get(index_of(row.owner_row))
                    .copied()
                    .flatten()
            }
            .ok_or_else(|| {
                terminal(ProjectionFault::OrphanOwner {
                    owner: row.owner_row,
                })
            })?;
            let target = if row.target_package.is_empty() {
                let ordinal = self.lookup(row.target).ok_or_else(|| {
                    terminal(ProjectionFault::OrphanTarget {
                        function: row.target,
                    })
                })?;
                OccurrenceTarget::Local(EntityId::new(ordinal))
            } else {
                let package = str::from_utf8(row.target_package)
                    .map_err(|_| terminal(ProjectionFault::Utf8))?;
                let function =
                    str::from_utf8(row.target).map_err(|_| terminal(ProjectionFault::Utf8))?;
                let lineage = PackageLineage::new(ECOSYSTEM, package)
                    .map_err(ProjectionFault::Lineage)
                    .map_err(terminal)?;
                let key = ForeignKey::new(
                    ForeignOrigin::Package(lineage),
                    function,
                    function,
                    Some(EntityKind::Function),
                )
                .map_err(ProjectionFault::ForeignKey)
                .map_err(terminal)?;
                OccurrenceTarget::Foreign(key)
            };
            let span = RelSpan::new(row.relative.0, row.relative.1)
                .map_err(ProjectionFault::Span)
                .map_err(terminal)?;
            self.facts
                .push_occurrence(
                    owner,
                    Occurrence {
                        target,
                        kind: ReferenceKind::FunctionCall,
                        confidence: OccurrenceConfidence::Oracle,
                        span,
                    },
                )
                .map_err(lane_terminal)?;
        }
        Ok(())
    }

    /// Pushes one executable fact: parameter and result carrier facts
    /// first, then the function fact whose product and function-pointer
    /// children are exactly those carriers, with its signature on the Go
    /// extension row.
    fn executable(
        &mut self,
        name: &'source [u8],
        signature: Option<u32>,
        parameter_start: u32,
    ) -> Result<u32, GoCollectError> {
        let mut carriers: Vec<TypeChild<'source>> = Vec::new();
        let mut parameters = Vec::new();
        let mut results = Vec::new();
        let mut variadic = false;
        let record = match signature {
            None => unknown_record(TypeReason::OracleGap, None),
            Some(row_index) => {
                let row = self
                    .image
                    .type_row(index_of(row_index))
                    .map_err(GoCollectError::Image)?;
                variadic = row.variadic;
                let children = self.row_children(&row)?;
                let param_count = usize::try_from(row.param_count)
                    .unwrap_or(0)
                    .min(children.len());
                for child in children.iter().take(param_count) {
                    let ordinal = self.carrier(*child)?;
                    parameters.push(ordinal);
                    carriers.push(TypeChild {
                        target: ordinal,
                        name: None,
                        flags: 0,
                    });
                }
                for child in children.iter().skip(param_count) {
                    let ordinal = self.carrier(*child)?;
                    results.push(ordinal);
                    carriers.push(TypeChild {
                        target: ordinal,
                        name: None,
                        flags: 0,
                    });
                }
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
                if !results.is_empty() {
                    record.payload1 = SemanticTypeRecord::RESULT_FLAG;
                }
                record
            }
        };
        let parameter_list = self
            .facts
            .intern_type_list(&parameters)
            .map_err(lane_terminal)?;
        let result_list = self
            .facts
            .intern_type_list(&results)
            .map_err(lane_terminal)?;
        let arity = u32::try_from(parameters.len())
            .map_err(|_| GoCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration))?;
        let results_count = u32::try_from(results.len())
            .map_err(|_| GoCollectError::Lowering(LoweringUnsupported::NoSupportedDeclaration))?;
        let mut fact = SemanticFact::new(
            EntityKind::Function,
            name,
            SemanticProductConstructor::function(arity, results_count),
        )
        .typed(record)
        .with_extension(EmissionExtension::Go(GoFacts {
            signature: GoSignature {
                parameters: parameter_list,
                results: result_list,
                variadic,
            },
            type_parameters: TypeParameterListId::new(parameter_start),
            fields: EntityListId::new(0),
            method_set: EntityListId::new(0),
            build_constraints: AtomListId::new(0),
        }));
        for ordinal in parameters {
            fact = fact.child(ProductChildRole::FunctionParameter, ordinal);
        }
        for ordinal in results {
            fact = fact.child(ProductChildRole::FunctionResult, ordinal);
        }
        for carrier in carriers {
            fact = fact.type_child(carrier.target, carrier.name, carrier.flags);
        }
        let ordinal = push(self.facts, fact)?;
        Ok(ordinal)
    }

    /// Pushes one parameter or result carrier fact whose root record is the
    /// projected parameter type.
    fn carrier(&mut self, row_index: u32) -> Result<u32, GoCollectError> {
        let root = self.root(Some(row_index), TypeReason::OracleGap)?;
        let fact = root.attach(SemanticFact::new(
            EntityKind::Parameter,
            UNNAMED,
            LEAF_PRODUCT,
        ));
        push(self.facts, fact)
    }

    /// Projects one optional type-row coordinate as a fact's root record.
    /// A missing coordinate folds to the caller's typed unknown reason.
    fn root(
        &mut self,
        cell: Option<u32>,
        missing: TypeReason,
    ) -> Result<RootType<'source>, GoCollectError> {
        let Some(row_index) = cell else {
            return Ok(RootType::leaf(unknown_record(missing, None)));
        };
        self.root_row(row_index, DEPTH_LIMIT)
    }

    /// Projects one image type row in fact-root context: the row's record
    /// becomes the fact's own record and its children may name fact
    /// ordinals as well as anonymous rows.
    fn root_row(
        &mut self,
        row_index: u32,
        depth: usize,
    ) -> Result<RootType<'source>, GoCollectError> {
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth));
        }
        let row = self
            .image
            .type_row(index_of(row_index))
            .map_err(GoCollectError::Image)?;
        match row.kind {
            TypeRowKind::Basic => Ok(RootType::leaf(self.basic_leaf(&row))),
            TypeRowKind::Named | TypeRowKind::Alias => self.named_root(&row),
            TypeRowKind::TypeParam => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
                record.text = Some(row.name);
                Ok(RootType::leaf(record))
            }
            TypeRowKind::Invalid => Ok(RootType::leaf(unknown_record(TypeReason::OracleGap, None))),
            TypeRowKind::Pointer => {
                let children = self.row_children(&row)?;
                self.unary_root(
                    SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                        .with_shape(SHAPE_MUT_POINTER),
                    children.first().copied(),
                    depth,
                )
            }
            TypeRowKind::Slice => {
                let children = self.row_children(&row)?;
                self.unary_root(
                    SemanticTypeRecord::leaf(SemanticTypeTag::Slice),
                    children.first().copied(),
                    depth,
                )
            }
            TypeRowKind::Array => {
                let children = self.row_children(&row)?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Array);
                record.text = Some(ARRAY_BRACKET);
                let length = row.length;
                record.payload0 = u32::try_from(length & 0xFFFF_FFFF).unwrap_or(u32::MAX);
                record.payload1 = u32::try_from((length >> 32) & 0xFFFF_FFFF).unwrap_or(u32::MAX);
                self.unary_root(record, children.first().copied(), depth)
            }
            TypeRowKind::Map => {
                let children = self.row_children(&row)?;
                let mut projected = RootType {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                    children: Vec::new(),
                };
                let base = self.builtin_row(b"map").map_err(lane_terminal)?;
                projected.children.push(TypeChild {
                    target: base,
                    name: None,
                    flags: 0,
                });
                for child in children {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Chan => {
                let children = self.row_children(&row)?;
                let mut projected = RootType {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                    children: Vec::new(),
                };
                let spelling = match row.dir {
                    ChanDir::Both => CHAN_BOTH,
                    ChanDir::Send => CHAN_SEND,
                    ChanDir::Recv => CHAN_RECV,
                };
                let base = self.builtin_row(spelling).map_err(lane_terminal)?;
                projected.children.push(TypeChild {
                    target: base,
                    name: None,
                    flags: 0,
                });
                for child in children {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Func => {
                let children = self.row_children(&row)?;
                let param_count = usize::try_from(row.param_count)
                    .unwrap_or(0)
                    .min(children.len());
                let mut projected = RootType {
                    record: {
                        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
                        if children.len() > param_count {
                            record.payload1 = SemanticTypeRecord::RESULT_FLAG;
                        }
                        record
                    },
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Struct => {
                let mut projected = RootType {
                    record: anonymous_record(ANON_STRUCT),
                    children: Vec::new(),
                };
                for member_index in member_run(&row) {
                    let member = self
                        .image
                        .member(member_index)
                        .map_err(GoCollectError::Image)?;
                    let target = self.coordinate(member.type_root, depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: Some(member.name),
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Interface => {
                if row.members.1 == 0 && row.children.1 == 0 {
                    return Ok(RootType::leaf(SemanticTypeRecord::leaf(
                        SemanticTypeTag::Any,
                    )));
                }
                let mut projected = RootType {
                    record: anonymous_record(ANON_INTERFACE),
                    children: Vec::new(),
                };
                for embedded in embedded_run(&row) {
                    let target = self.coordinate(Some(embedded), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: Some(embedded_name(&row)),
                        flags: 0,
                    });
                }
                for member_index in member_run(&row) {
                    let member = self
                        .image
                        .member(member_index)
                        .map_err(GoCollectError::Image)?;
                    if member.kind != MemberKind::Method {
                        continue;
                    }
                    let target = self.coordinate(member.type_root, depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: Some(member.name),
                        flags: 0,
                    });
                }
                Ok(projected)
            }
            TypeRowKind::Union | TypeRowKind::Tuple => {
                let tag = match row.kind {
                    TypeRowKind::Union => SemanticTypeTag::Union,
                    _ => SemanticTypeTag::Tuple,
                };
                let children = self.row_children(&row)?;
                let mut projected = RootType {
                    record: SemanticTypeRecord::leaf(tag),
                    children: Vec::new(),
                };
                // The lattice's union children own no approximation cell,
                // so the image's tilde flags stay behind; the term shapes
                // are exact.
                for child in children {
                    let target = self.coordinate(Some(child), depth - 1)?;
                    projected.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                Ok(projected)
            }
        }
    }

    /// Projects one unary root: exactly one child coordinate or the typed
    /// depth-less unknown when the image row lost its element.
    fn unary_root(
        &mut self,
        record: SemanticTypeRecord<'source>,
        child: Option<u32>,
        depth: usize,
    ) -> Result<RootType<'source>, GoCollectError> {
        let mut projected = RootType {
            record,
            children: Vec::new(),
        };
        let target = self.coordinate(child, depth - 1)?;
        projected.children.push(TypeChild {
            target,
            name: None,
            flags: 0,
        });
        Ok(projected)
    }

    /// Projects one named or alias row in fact-root context: a pure local
    /// name becomes its nominal fact ordinal, a local application carries
    /// the base fact and its projected arguments, and every foreign or
    /// universe name folds to the typed unknown that retains its spelling.
    fn named_root(
        &mut self,
        row: &compiler_languages_go::TypeRow<'source>,
    ) -> Result<RootType<'source>, GoCollectError> {
        let local = row.package == self.package
            && !row.package.is_empty()
            && self.lookup(row.name).is_some();
        if local {
            let base = self.lookup(row.name).unwrap_or_default();
            if row.children.1 == 0 {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Nominal);
                record.nominal = Some(NominalRef::Local(EntityId::new(base)));
                return Ok(RootType::leaf(record));
            }
            let children = self.row_children(row)?;
            let mut projected = RootType {
                record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                children: vec![TypeChild {
                    target: base,
                    name: None,
                    flags: 0,
                }],
            };
            for child in children {
                let target = self.coordinate(Some(child), DEPTH_LIMIT - 1)?;
                projected.children.push(TypeChild {
                    target,
                    name: None,
                    flags: 0,
                });
            }
            return Ok(projected);
        }
        let reason = if row.package.is_empty() {
            if row.name == b"any" {
                return Ok(RootType::leaf(SemanticTypeRecord::leaf(
                    SemanticTypeTag::Any,
                )));
            }
            TypeReason::UnresolvedExternal
        } else {
            TypeReason::UnresolvedExternal
        };
        Ok(RootType::leaf(unknown_record(reason, Some(row.name))))
    }

    /// Projects one optional type-row coordinate as a child coordinate: a
    /// local named reference stays a fact ordinal, everything else lives in
    /// the anonymous pool.
    fn coordinate(&mut self, cell: Option<u32>, depth: usize) -> Result<u32, GoCollectError> {
        let Some(row_index) = cell else {
            let anchor = self.anchor().map_err(terminal)?;
            return self
                .intern_anonymous(
                    anchor,
                    &AnonRow {
                        record: unknown_record(TypeReason::OracleGap, None),
                        children: Vec::new(),
                    },
                )
                .map_err(lane_terminal);
        };
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth));
        }
        let row = self
            .image
            .type_row(index_of(row_index))
            .map_err(GoCollectError::Image)?;
        let local_named = matches!(row.kind, TypeRowKind::Named | TypeRowKind::Alias)
            && row.package == self.package
            && !row.package.is_empty()
            && self.lookup(row.name).is_some();
        if local_named && row.children.1 == 0 {
            return Ok(self.lookup(row.name).unwrap_or_default());
        }
        self.project_anonymous(index_of(row_index), depth)
    }

    /// Projects one image type row in anonymous context, memoized per row:
    /// the result is an anonymous row coordinate whose subtree references
    /// only earlier anonymous rows, because the frozen wire orders the pool
    /// before the fact rows and rejects forward child targets.
    fn project_anonymous(&mut self, row_index: usize, depth: usize) -> Result<u32, GoCollectError> {
        if let Some(memoized) = self.anonymous.get(row_index).copied().flatten() {
            return Ok(memoized);
        }
        if depth == 0 {
            return Err(terminal(ProjectionFault::Depth));
        }
        let anchor = self.anchor().map_err(terminal)?;
        let row = self
            .image
            .type_row(row_index)
            .map_err(GoCollectError::Image)?;
        let built = match row.kind {
            TypeRowKind::Basic => AnonRow {
                record: self.basic_leaf(&row),
                children: Vec::new(),
            },
            TypeRowKind::Named | TypeRowKind::Alias => AnonRow {
                record: unknown_record(TypeReason::NoIrRepresentation, Some(row.name)),
                children: Vec::new(),
            },
            TypeRowKind::TypeParam => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::TypeVar);
                record.text = Some(row.name);
                AnonRow {
                    record,
                    children: Vec::new(),
                }
            }
            TypeRowKind::Invalid => AnonRow {
                record: unknown_record(TypeReason::OracleGap, None),
                children: Vec::new(),
            },
            TypeRowKind::Pointer => {
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Primitive)
                        .with_shape(SHAPE_MUT_POINTER),
                    children: Vec::new(),
                };
                if let Some(child) = children.first() {
                    let target = self.project_anonymous(index_of(*child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Slice => {
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Slice),
                    children: Vec::new(),
                };
                if let Some(child) = children.first() {
                    let target = self.project_anonymous(index_of(*child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Array => {
                let children = self.row_children(&row)?;
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Array);
                record.text = Some(ARRAY_BRACKET);
                record.payload0 = u32::try_from(row.length & 0xFFFF_FFFF).unwrap_or(u32::MAX);
                record.payload1 =
                    u32::try_from((row.length >> 32) & 0xFFFF_FFFF).unwrap_or(u32::MAX);
                let mut built = AnonRow {
                    record,
                    children: Vec::new(),
                };
                if let Some(child) = children.first() {
                    let target = self.project_anonymous(index_of(*child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Map => {
                let children = self.row_children(&row)?;
                let base = self.builtin_row(b"map").map_err(lane_terminal)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                    children: vec![TypeChild {
                        target: base,
                        name: None,
                        flags: 0,
                    }],
                };
                for child in children {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Chan => {
                let children = self.row_children(&row)?;
                let spelling = match row.dir {
                    ChanDir::Both => CHAN_BOTH,
                    ChanDir::Send => CHAN_SEND,
                    ChanDir::Recv => CHAN_RECV,
                };
                let base = self.builtin_row(spelling).map_err(lane_terminal)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(SemanticTypeTag::Apply),
                    children: vec![TypeChild {
                        target: base,
                        name: None,
                        flags: 0,
                    }],
                };
                for child in children {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Func => {
                let children = self.row_children(&row)?;
                let param_count = usize::try_from(row.param_count)
                    .unwrap_or(0)
                    .min(children.len());
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::FunctionPointer);
                if children.len() > param_count {
                    record.payload1 = SemanticTypeRecord::RESULT_FLAG;
                }
                let mut built = AnonRow {
                    record,
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Struct => {
                let mut built = AnonRow {
                    record: anonymous_record(ANON_STRUCT),
                    children: Vec::new(),
                };
                for member_index in member_run(&row) {
                    let member = self
                        .image
                        .member(member_index)
                        .map_err(GoCollectError::Image)?;
                    let target = match member.type_root {
                        Some(root) => self.project_anonymous(index_of(root), depth - 1)?,
                        None => self.coordinate(None, depth - 1)?,
                    };
                    built.children.push(TypeChild {
                        target,
                        name: Some(member.name),
                        flags: 0,
                    });
                }
                built
            }
            TypeRowKind::Interface => {
                if row.members.1 == 0 && row.children.1 == 0 {
                    AnonRow {
                        record: SemanticTypeRecord::leaf(SemanticTypeTag::Any),
                        children: Vec::new(),
                    }
                } else {
                    let mut built = AnonRow {
                        record: anonymous_record(ANON_INTERFACE),
                        children: Vec::new(),
                    };
                    for embedded in embedded_run(&row) {
                        let target = self.project_anonymous(index_of(embedded), depth - 1)?;
                        built.children.push(TypeChild {
                            target,
                            name: Some(embedded_name(&row)),
                            flags: 0,
                        });
                    }
                    for member_index in member_run(&row) {
                        let member = self
                            .image
                            .member(member_index)
                            .map_err(GoCollectError::Image)?;
                        if member.kind != MemberKind::Method {
                            continue;
                        }
                        let target = match member.type_root {
                            Some(root) => self.project_anonymous(index_of(root), depth - 1)?,
                            None => self.coordinate(None, depth - 1)?,
                        };
                        built.children.push(TypeChild {
                            target,
                            name: Some(member.name),
                            flags: 0,
                        });
                    }
                    built
                }
            }
            TypeRowKind::Union | TypeRowKind::Tuple => {
                let tag = match row.kind {
                    TypeRowKind::Union => SemanticTypeTag::Union,
                    _ => SemanticTypeTag::Tuple,
                };
                let children = self.row_children(&row)?;
                let mut built = AnonRow {
                    record: SemanticTypeRecord::leaf(tag),
                    children: Vec::new(),
                };
                for child in children {
                    let target = self.project_anonymous(index_of(child), depth - 1)?;
                    built.children.push(TypeChild {
                        target,
                        name: None,
                        flags: 0,
                    });
                }
                built
            }
        };
        let coordinate = self
            .intern_anonymous(anchor, &built)
            .map_err(lane_terminal)?;
        if let Some(slot) = self.anonymous.get_mut(row_index) {
            *slot = Some(coordinate);
        }
        Ok(coordinate)
    }

    /// Appends one anonymous row's children into the pooled lane and interns
    /// the row under the given owner fact.
    fn intern_anonymous(&mut self, anchor: u32, row: &AnonRow<'source>) -> Result<u32, FactFault> {
        for child in &row.children {
            self.facts
                .anonymous_type_child(child.target, child.name, child.flags)?;
        }
        self.facts.intern_anonymous_type_row(anchor, row.record)
    }

    /// Interns one synthetic builtin leaf row (`map`, channel directions).
    fn builtin_row(&mut self, spelling: &'source [u8]) -> Result<u32, FactFault> {
        let anchor = u32::try_from(self.facts.len()).unwrap_or(u32::MAX);
        let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
        record.payload0 = SHAPE_BUILTIN;
        record.text = Some(spelling);
        self.facts.intern_anonymous_type_row(anchor, record)
    }

    /// The exact lattice cells of one basic row: exact integer and float
    /// widths, `byte`/`rune` aliases folded to their underlying widths,
    /// complex spellings on the builtin shape, and every other universe
    /// name — `error` among them — on the typed unknown that retains it.
    fn basic_leaf(
        &self,
        row: &compiler_languages_go::TypeRow<'source>,
    ) -> SemanticTypeRecord<'source> {
        let signed = |width: u32| {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = SHAPE_INTEGER;
            record.payload1 = (width << INTEGER_WIDTH_SHIFT) | INTEGER_SIGNED_FLAG;
            record
        };
        let unsigned = |width: u32| {
            let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
            record.payload0 = SHAPE_INTEGER;
            record.payload1 = width << INTEGER_WIDTH_SHIFT;
            record
        };
        match row.name {
            b"bool" => SemanticTypeRecord::leaf(SemanticTypeTag::Primitive).with_shape(SHAPE_BOOL),
            b"string" => SemanticTypeRecord::leaf(SemanticTypeTag::Primitive).with_shape(SHAPE_STR),
            b"int" => signed(TypeWidth::Arch.to_cell()),
            b"uint" | b"uintptr" => unsigned(TypeWidth::Arch.to_cell()),
            b"int8" => signed(8),
            b"int16" => signed(16),
            b"int32" | b"rune" => signed(32),
            b"int64" => signed(64),
            b"uint8" | b"byte" => unsigned(8),
            b"uint16" => unsigned(16),
            b"uint32" => unsigned(32),
            b"uint64" => unsigned(64),
            b"float32" => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_FLOAT;
                record.payload1 = TypeWidth::Fixed(32).to_cell();
                record
            }
            b"float64" => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_FLOAT;
                record.payload1 = TypeWidth::Fixed(64).to_cell();
                record
            }
            b"complex64" | b"complex128" => {
                let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::Primitive);
                record.payload0 = SHAPE_BUILTIN;
                record.text = Some(row.name);
                record
            }
            _ => unknown_record(TypeReason::UnresolvedExternal, Some(row.name)),
        }
    }

    /// The pooled child-run row indices of one type row, image order.
    fn row_children(
        &self,
        row: &compiler_languages_go::TypeRow<'source>,
    ) -> Result<Vec<u32>, GoCollectError> {
        let start = index_of(row.children.0);
        let count = usize::try_from(row.children.1).unwrap_or(0);
        let mut children = Vec::with_capacity(count);
        for offset in 0..count {
            let (target, _) = self
                .image
                .type_child(start + offset)
                .map_err(GoCollectError::Image)?;
            children.push(target);
        }
        Ok(children)
    }

    /// Interns one pooled entity list under its bounded width.
    fn entity_list(&mut self, ordinals: &[u32]) -> Result<EntityListId, GoCollectError> {
        if ordinals.len() > MAX_REF_LIST_ELEMENTS {
            return Err(terminal(ProjectionFault::ListCapacity));
        }
        self.facts
            .intern_entity_list(ordinals)
            .map_err(lane_terminal)
    }
}

/// `AnonRecordForm::Struct` and `::Interface` wire cells.
const ANON_STRUCT: u32 = 0;
const ANON_INTERFACE: u32 = 1;

/// The anonymous-record leaf record for one form cell.
const fn anonymous_record(form: u32) -> SemanticTypeRecord<'static> {
    let mut record = SemanticTypeRecord::leaf(SemanticTypeTag::AnonymousRecord);
    record.payload0 = form;
    record
}

/// `PrimitiveShape` cells attached to a primitive leaf record.
trait WithShape {
    fn with_shape(self, shape: u32) -> Self;
}

impl WithShape for SemanticTypeRecord<'_> {
    fn with_shape(mut self, shape: u32) -> Self {
        self.payload0 = shape;
        self
    }
}

/// The member-run row indices of one struct or interface row.
fn member_run(row: &compiler_languages_go::TypeRow<'_>) -> Vec<usize> {
    let start = index_of(row.members.0);
    let count = usize::try_from(row.members.1).unwrap_or(0);
    (start..start + count).collect()
}

/// The embedded-run row indices of one interface row.
fn embedded_run(row: &compiler_languages_go::TypeRow<'_>) -> Vec<u32> {
    let start = index_of(row.children.0);
    let count = usize::try_from(row.children.1).unwrap_or(0);
    (start..start + count)
        .map(|offset| u32::try_from(offset).unwrap_or(u32::MAX))
        .collect()
}

/// The first embedded row's spelling; anonymous embeddeds carry none and
/// take the blank identifier.
fn embedded_name<'image>(row: &compiler_languages_go::TypeRow<'image>) -> &'image [u8] {
    if row.name.is_empty() {
        UNNAMED
    } else {
        row.name
    }
}

/// Splits one validated NUL-separated name blob into its non-empty parts.
fn blank_separated(blob: &[u8]) -> Vec<&[u8]> {
    blob.split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .collect()
}

/// Converts one validated u32 coordinate to its row index.
fn index_of(coordinate: u32) -> usize {
    usize::try_from(coordinate).unwrap_or(usize::MAX)
}

/// Pushes one documentation text run as lines with soft breaks between
/// them; empty lines contribute only their breaks.
fn push_doc_lines<'source>(
    facts: &mut FactSet<'source>,
    owner: u32,
    text: &'source [u8],
) -> Result<(), FactFault> {
    let mut line_start = 0_usize;
    while line_start < text.len() {
        let line_end = text
            .get(line_start..)
            .unwrap_or(&[])
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(text.len(), |at| line_start + at);
        let line = text.get(line_start..line_end).unwrap_or(&[]);
        if !line.is_empty() {
            facts.push_doc(owner, DocFragmentInput::Text(line))?;
        }
        if line_end == text.len() {
            break;
        }
        facts.push_doc(owner, DocFragmentInput::SoftBreak)?;
        line_start = line_end + 1;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use compiler_ir::{FragmentView, LanguageExtensionWireFact, SourceIdentity};
    use compiler_vocabulary::{CompileRecipeFact, LanguageProfile, NativeTool, Stage};
    use heart_identity::{ContentId, SourceFactDomain, ToolchainDomain};

    const HEADER_BYTES: usize = 116;
    const NONE: u32 = u32::MAX;
    const IMAGE_DOMAIN: &[u8] = b"nudox.go.authority.image.sha256.v3\0";
    const FILE: &[u8] = b"main.go";
    const PACKAGE: &[u8] = b"example.com/demo";
    const SPAN_END: u32 = 256;

    /// Fixture declaration-kind tags.
    const KIND_TYPE: u8 = 1;
    const KIND_ALIAS: u8 = 2;
    const KIND_FUNC: u8 = 3;
    const KIND_CONST: u8 = 4;
    const KIND_VAR: u8 = 5;

    /// Fixture type-row discriminants.
    const ROW_BASIC: u8 = 0;
    const ROW_NAMED: u8 = 1;
    const ROW_TYPE_PARAM: u8 = 3;
    const ROW_POINTER: u8 = 4;
    const ROW_SLICE: u8 = 5;
    const ROW_ARRAY: u8 = 6;
    const ROW_MAP: u8 = 7;
    const ROW_CHAN: u8 = 8;
    const ROW_FUNC: u8 = 9;
    const ROW_STRUCT: u8 = 10;
    const ROW_INTERFACE: u8 = 11;

    /// Fixture doc owner kinds.
    const DOC_DECLARATION: u8 = 0;
    const DOC_MEMBER: u8 = 2;

    #[derive(Debug, thiserror::Error)]
    enum TestError {
        #[error("collection rejected the authority image: {0:?}")]
        Collect(GoCollectError),
        #[error("lane admission rejected the fact set: {0:?}")]
        Admission(crate::lower::AdmissionFault),
        #[error("fragment validation rejected the bytes: {0:?}")]
        Validate(compiler_ir::FragmentError),
        #[error("type fact cursor rejected: {0:?}")]
        TypeFact(compiler_ir::TypeFactFault),
        #[error("occurrence cursor rejected: {0:?}")]
        Occurrence(compiler_ir::OccurrenceFault),
        #[error("documentation cursor rejected: {0:?}")]
        Doc(compiler_ir::DocFactFault),
        #[error("fixture scalar conversion failed: {0}")]
        Num(core::num::TryFromIntError),
        #[error("expected {0}")]
        Missing(&'static str),
        #[error("committed bytes changed")]
        Tail,
    }

    impl From<GoCollectError> for TestError {
        fn from(error: GoCollectError) -> Self {
            Self::Collect(error)
        }
    }

    impl From<compiler_ir::TypeFactFault> for TestError {
        fn from(error: compiler_ir::TypeFactFault) -> Self {
            Self::TypeFact(error)
        }
    }

    impl From<compiler_ir::OccurrenceFault> for TestError {
        fn from(error: compiler_ir::OccurrenceFault) -> Self {
            Self::Occurrence(error)
        }
    }

    impl From<compiler_ir::DocFactFault> for TestError {
        fn from(error: compiler_ir::DocFactFault) -> Self {
            Self::Doc(error)
        }
    }

    impl From<crate::lower::AdmissionFault> for TestError {
        fn from(error: crate::lower::AdmissionFault) -> Self {
            Self::Admission(error)
        }
    }

    impl From<compiler_ir::FragmentError> for TestError {
        fn from(error: compiler_ir::FragmentError) -> Self {
            Self::Validate(error)
        }
    }

    impl From<core::num::TryFromIntError> for TestError {
        fn from(error: core::num::TryFromIntError) -> Self {
            Self::Num(error)
        }
    }

    #[derive(Clone, Copy)]
    struct Cell {
        offset: u32,
        length: u32,
    }

    /// One Go authority image under construction, encoded exactly like the
    /// vendored producer: canonical planes, fixed header, and the
    /// domain-separated source-bound checksum.
    #[derive(Clone, Default)]
    struct Fixture {
        atom_bytes: Vec<u8>,
        declarations: Vec<DeclRow>,
        types: Vec<TypeRowF>,
        methods: Vec<MethodF>,
        parameters: Vec<ParameterF>,
        members: Vec<MemberF>,
        docs: Vec<DocF>,
        references: Vec<RefF>,
        constraints: Vec<ConstraintF>,
        children: Vec<u32>,
    }

    #[derive(Clone)]
    struct DeclRow {
        kind: u8,
        name: Cell,
        package: Cell,
        type_root: Option<u32>,
    }

    #[derive(Clone)]
    struct TypeRowF {
        kind: u8,
        dir: u8,
        variadic: u8,
        name: Cell,
        package: Cell,
        length: i64,
        param_count: u32,
        children: (u32, u32),
        members: (u32, u32),
    }

    #[derive(Clone)]
    struct MethodF {
        owner: u32,
        name: Cell,
        type_root: Option<u32>,
        receiver: Cell,
        receiver_params: (Cell, u32),
    }

    #[derive(Clone)]
    struct ParameterF {
        owner: u32,
        name: Cell,
        constraint: Option<u32>,
    }

    #[derive(Clone)]
    struct MemberF {
        owner: u32,
        kind: u8,
        name: Cell,
        type_root: Option<u32>,
    }

    #[derive(Clone)]
    struct DocF {
        owner_kind: u8,
        owner: u32,
        text: Cell,
    }

    #[derive(Clone)]
    struct RefF {
        owner: u32,
        target: Cell,
        target_package: Cell,
        receiver: Cell,
        start: u32,
        end: u32,
    }

    #[derive(Clone)]
    struct ConstraintF {
        file: Cell,
        constraint: Cell,
        blob: Cell,
        count: u32,
    }

    impl Fixture {
        fn new() -> Self {
            Self::default()
        }

        fn atom(&mut self, text: &[u8]) -> Cell {
            let offset = u32::try_from(self.atom_bytes.len()).unwrap_or(u32::MAX);
            self.atom_bytes.extend_from_slice(text);
            Cell {
                offset,
                length: u32::try_from(text.len()).unwrap_or(u32::MAX),
            }
        }

        fn blob(&mut self, records: &[(u8, &[u8])]) -> (Cell, u32) {
            let cell = self.atom(&[]);
            let count = u32::try_from(records.len()).unwrap_or(u32::MAX);
            for (kind, name) in records {
                self.atom_bytes.push(*kind);
                self.atom_bytes.extend_from_slice(name);
                self.atom_bytes.push(0);
            }
            (
                Cell {
                    offset: cell.offset,
                    length: u32::try_from(
                        self.atom_bytes.len() - usize::try_from(cell.offset).unwrap_or(0),
                    )
                    .unwrap_or(u32::MAX),
                },
                count,
            )
        }

        fn push_type_row(&mut self, row: TypeRowF) -> u32 {
            let index = u32::try_from(self.types.len()).unwrap_or(u32::MAX);
            self.types.push(row);
            index
        }

        fn start_row(&mut self, kind: u8) -> u32 {
            let index = self.push_type_row(TypeRowF {
                kind,
                dir: 0,
                variadic: 0,
                name: Cell {
                    offset: 0,
                    length: 0,
                },
                package: Cell {
                    offset: 0,
                    length: 0,
                },
                length: 0,
                param_count: 0,
                children: (u32::try_from(self.children.len()).unwrap_or(u32::MAX), 0),
                members: (0, 0),
            });
            index
        }

        fn add_child(&mut self, row_index: u32, target: u32) {
            let row = &mut self.types[index_of(row_index)];
            row.children.1 += 1;
            self.children.push(target);
        }

        fn add_member(&mut self, row_index: u32, member: MemberF) {
            let row = &mut self.types[index_of(row_index)];
            if row.members.1 == 0 {
                row.members.0 = u32::try_from(self.members.len()).unwrap_or(u32::MAX);
            }
            row.members.1 += 1;
            self.members.push(member);
        }

        fn basic(&mut self, name: &[u8]) -> u32 {
            let spelled = self.atom(name);
            let index = self.start_row(ROW_BASIC);
            self.types[index_of(index)].name = spelled;
            index
        }

        fn named(&mut self, package: &[u8], name: &[u8], arguments: &[u32]) -> u32 {
            let spelled = self.atom(name);
            let package = self.atom(package);
            let index = self.start_row(ROW_NAMED);
            let row = &mut self.types[index_of(index)];
            row.name = spelled;
            row.package = package;
            for argument in arguments {
                self.add_child(index, *argument);
            }
            index
        }

        fn type_param_row(&mut self, name: &[u8]) -> u32 {
            let spelled = self.atom(name);
            let index = self.start_row(ROW_TYPE_PARAM);
            self.types[index_of(index)].name = spelled;
            index
        }

        fn unary(&mut self, kind: u8, child: u32) -> u32 {
            let index = self.start_row(kind);
            self.add_child(index, child);
            index
        }

        fn array(&mut self, length: i64, child: u32) -> u32 {
            let index = self.unary(ROW_ARRAY, child);
            self.types[index_of(index)].length = length;
            index
        }

        fn chan(&mut self, dir: u8, child: u32) -> u32 {
            let index = self.unary(ROW_CHAN, child);
            self.types[index_of(index)].dir = dir;
            index
        }

        fn map(&mut self, key: u32, value: u32) -> u32 {
            let index = self.start_row(ROW_MAP);
            self.add_child(index, key);
            self.add_child(index, value);
            index
        }

        fn func(&mut self, parameters: &[u32], results: &[u32], variadic: bool) -> u32 {
            let index = self.start_row(ROW_FUNC);
            let row = &mut self.types[index_of(index)];
            row.variadic = u8::from(variadic);
            row.param_count = u32::try_from(parameters.len()).unwrap_or(u32::MAX);
            for parameter in parameters {
                self.add_child(index, *parameter);
            }
            for result in results {
                self.add_child(index, *result);
            }
            index
        }

        fn field(&mut self, owner: u32, name: &[u8], type_root: Option<u32>) {
            let spelled = self.atom(name);
            self.add_member(
                owner,
                MemberF {
                    owner,
                    kind: 0,
                    name: spelled,
                    type_root,
                },
            );
        }

        fn interface_method(&mut self, owner: u32, name: &[u8], type_root: Option<u32>) {
            let spelled = self.atom(name);
            self.add_member(
                owner,
                MemberF {
                    owner,
                    kind: 1,
                    name: spelled,
                    type_root,
                },
            );
        }

        fn declaration(&mut self, kind: u8, name: &[u8], type_root: Option<u32>) -> usize {
            let spelled = self.atom(name);
            let package = self.atom(PACKAGE);
            self.declarations.push(DeclRow {
                kind,
                name: spelled,
                package,
                type_root,
            });
            self.declarations.len() - 1
        }

        fn method(&mut self, owner: usize, name: &[u8], type_root: Option<u32>) -> usize {
            let spelled = self.atom(name);
            let receiver = self.atom(b"t");
            self.methods.push(MethodF {
                owner: u32::try_from(owner).unwrap_or(u32::MAX),
                name: spelled,
                type_root,
                receiver,
                receiver_params: (
                    Cell {
                        offset: 0,
                        length: 0,
                    },
                    0,
                ),
            });
            self.methods.len() - 1
        }

        fn type_parameter(&mut self, owner: usize, name: &[u8], constraint: Option<u32>) {
            let spelled = self.atom(name);
            self.parameters.push(ParameterF {
                owner: u32::try_from(owner).unwrap_or(u32::MAX),
                name: spelled,
                constraint,
            });
        }

        fn doc(&mut self, owner_kind: u8, owner: u32, text: &[u8]) {
            let borrowed = self.atom(text);
            self.docs.push(DocF {
                owner_kind,
                owner,
                text: borrowed,
            });
        }

        fn reference(
            &mut self,
            owner: u32,
            receiver: &[u8],
            target: &[u8],
            target_package: &[u8],
            start: u32,
            end: u32,
        ) {
            let target = self.atom(target);
            let package = self.atom(target_package);
            let receiver = self.atom(receiver);
            self.references.push(RefF {
                owner,
                target,
                target_package: package,
                receiver,
                start,
                end,
            });
        }

        fn constraint(&mut self, file: &[u8], constraint: &[u8], exported: &[(u8, &[u8])]) {
            let file = self.atom(file);
            let spelled = self.atom(constraint);
            let (blob, count) = self.blob(exported);
            self.constraints.push(ConstraintF {
                file,
                constraint: spelled,
                blob,
                count,
            });
        }

        fn file_cell(&self) -> Cell {
            self.atom_cell(FILE)
        }

        fn atom_cell(&self, text: &[u8]) -> Cell {
            let offset = self
                .atom_bytes
                .windows(text.len())
                .position(|window| window == text)
                .map_or(u32::MAX, |at| u32::try_from(at).unwrap_or(u32::MAX));
            Cell {
                offset,
                length: u32::try_from(text.len()).unwrap_or(u32::MAX),
            }
        }

        fn encode(&self, source: &[u8]) -> Result<Vec<u8>, TestError> {
            let count = |length: usize| u32::try_from(length).map_err(TestError::from);
            let cell =
                |borrowed: Cell| (borrowed.offset.to_le_bytes(), borrowed.length.to_le_bytes());
            let file = self.file_cell();
            let mut declarations = Vec::new();
            for row in &self.declarations {
                let (name, name_len) = cell(row.name);
                let (package, package_len) = cell(row.package);
                declarations.extend_from_slice(&[row.kind, 1, 0, 0]);
                declarations.extend_from_slice(&name);
                declarations.extend_from_slice(&name_len);
                declarations.extend_from_slice(&package);
                declarations.extend_from_slice(&package_len);
                declarations.extend_from_slice(&row.type_root.unwrap_or(NONE).to_le_bytes());
                declarations.extend_from_slice(&0_u32.to_le_bytes());
                declarations.extend_from_slice(&SPAN_END.to_le_bytes());
                declarations.extend_from_slice(&file.offset.to_le_bytes());
                declarations.extend_from_slice(&file.length.to_le_bytes());
            }
            let mut types = Vec::new();
            for row in &self.types {
                let (name, name_len) = cell(row.name);
                let (package, package_len) = cell(row.package);
                types.extend_from_slice(&[row.kind, row.dir, row.variadic, 0]);
                types.extend_from_slice(&name);
                types.extend_from_slice(&name_len);
                types.extend_from_slice(&package);
                types.extend_from_slice(&package_len);
                types.extend_from_slice(&row.length.to_le_bytes());
                types.extend_from_slice(&row.children.0.to_le_bytes());
                types.extend_from_slice(&row.children.1.to_le_bytes());
                types.extend_from_slice(&row.members.0.to_le_bytes());
                types.extend_from_slice(&row.members.1.to_le_bytes());
                types.extend_from_slice(&0_u32.to_le_bytes());
                types.extend_from_slice(&row.param_count.to_le_bytes());
            }
            let mut methods = Vec::new();
            for row in &self.methods {
                let (name, name_len) = cell(row.name);
                let (receiver, receiver_len) = cell(row.receiver);
                let (blob, blob_len) = cell(row.receiver_params.0);
                methods.extend_from_slice(&row.owner.to_le_bytes());
                methods.extend_from_slice(&[1, 0, 0, 0]);
                methods.extend_from_slice(&name);
                methods.extend_from_slice(&name_len);
                methods.extend_from_slice(&row.type_root.unwrap_or(NONE).to_le_bytes());
                methods.extend_from_slice(&receiver);
                methods.extend_from_slice(&receiver_len);
                methods.extend_from_slice(&blob);
                methods.extend_from_slice(&blob_len);
                methods.extend_from_slice(&row.receiver_params.1.to_le_bytes());
                methods.extend_from_slice(&0_u32.to_le_bytes());
                methods.extend_from_slice(&0_u32.to_le_bytes());
                methods.extend_from_slice(&0_u32.to_le_bytes());
                methods.extend_from_slice(&SPAN_END.to_le_bytes());
                methods.extend_from_slice(&file.offset.to_le_bytes());
                methods.extend_from_slice(&file.length.to_le_bytes());
            }
            let mut parameters = Vec::new();
            for row in &self.parameters {
                let (name, name_len) = cell(row.name);
                parameters.extend_from_slice(&row.owner.to_le_bytes());
                parameters.extend_from_slice(&name);
                parameters.extend_from_slice(&name_len);
                parameters.extend_from_slice(&row.constraint.unwrap_or(NONE).to_le_bytes());
            }
            let mut members = Vec::new();
            for row in &self.members {
                let (name, name_len) = cell(row.name);
                members.extend_from_slice(&row.owner.to_le_bytes());
                members.extend_from_slice(&[row.kind, 0, 1, 0]);
                members.extend_from_slice(&name);
                members.extend_from_slice(&name_len);
                members.extend_from_slice(&row.type_root.unwrap_or(NONE).to_le_bytes());
                members.extend_from_slice(&0_u32.to_le_bytes());
                members.extend_from_slice(&0_u32.to_le_bytes());
                members.extend_from_slice(&0_u32.to_le_bytes());
            }
            let mut docs = Vec::new();
            for row in &self.docs {
                let (text, text_len) = cell(row.text);
                docs.extend_from_slice(&[row.owner_kind, 0, 0, 0]);
                docs.extend_from_slice(&row.owner.to_le_bytes());
                docs.extend_from_slice(&text);
                docs.extend_from_slice(&text_len);
            }
            let mut references = Vec::new();
            for row in &self.references {
                let (target, target_len) = cell(row.target);
                let (package, package_len) = cell(row.target_package);
                let (receiver, receiver_len) = cell(row.receiver);
                references.extend_from_slice(&row.owner.to_le_bytes());
                references.extend_from_slice(&target);
                references.extend_from_slice(&target_len);
                references.extend_from_slice(&package);
                references.extend_from_slice(&package_len);
                references.extend_from_slice(&row.start.to_le_bytes());
                references.extend_from_slice(&row.end.to_le_bytes());
                references.extend_from_slice(&file.offset.to_le_bytes());
                references.extend_from_slice(&file.length.to_le_bytes());
                references.extend_from_slice(&receiver);
                references.extend_from_slice(&receiver_len);
                references.extend_from_slice(&0_u32.to_le_bytes());
            }
            let mut constraints = Vec::new();
            for row in &self.constraints {
                let (file, file_len) = cell(row.file);
                let (constraint, constraint_len) = cell(row.constraint);
                let (blob, blob_len) = cell(row.blob);
                constraints.extend_from_slice(&file);
                constraints.extend_from_slice(&file_len);
                constraints.extend_from_slice(&constraint);
                constraints.extend_from_slice(&constraint_len);
                constraints.extend_from_slice(&blob);
                constraints.extend_from_slice(&blob_len);
                constraints.extend_from_slice(&row.count.to_le_bytes());
            }
            let mut children = Vec::new();
            for target in &self.children {
                children.extend_from_slice(&target.to_le_bytes());
                children.extend_from_slice(&0_u32.to_le_bytes());
            }
            let sections = [
                declarations,
                types,
                methods,
                parameters,
                members,
                docs,
                references,
                constraints,
                children,
                self.atom_bytes.clone(),
            ];
            let counts = [
                count(self.declarations.len())?,
                count(self.types.len())?,
                count(self.methods.len())?,
                count(self.parameters.len())?,
                count(self.members.len())?,
                count(self.docs.len())?,
                count(self.references.len())?,
                count(self.constraints.len())?,
            ];
            let body = sections.iter().map(Vec::len).sum::<usize>();
            let mut image = vec![0_u8; HEADER_BYTES];
            image[..4].copy_from_slice(b"NGAI");
            image[4..6].copy_from_slice(&3_u16.to_le_bytes());
            image[6..8].copy_from_slice(
                &u16::try_from(HEADER_BYTES)
                    .map_err(TestError::from)?
                    .to_le_bytes(),
            );
            image[8..12].copy_from_slice(&counts[0].to_le_bytes());
            image[12..16].copy_from_slice(&count(self.atom_bytes.len())?.to_le_bytes());
            image[16..20].copy_from_slice(&count(body)?.to_le_bytes());
            image[20..52].copy_from_slice(Sha256::digest(source).as_slice());
            image[84..88].copy_from_slice(&counts[1].to_le_bytes());
            image[88..92].copy_from_slice(&counts[2].to_le_bytes());
            image[92..96].copy_from_slice(&counts[3].to_le_bytes());
            image[96..100].copy_from_slice(&counts[4].to_le_bytes());
            image[100..104].copy_from_slice(&counts[5].to_le_bytes());
            image[104..108].copy_from_slice(&counts[6].to_le_bytes());
            image[108..112].copy_from_slice(&counts[7].to_le_bytes());
            let mut digest = Sha256::new();
            digest.update(IMAGE_DOMAIN);
            digest.update(&image[..52]);
            digest.update(&image[84..HEADER_BYTES]);
            for section in &sections {
                digest.update(section);
            }
            image[52..84].copy_from_slice(&digest.finalize());
            let mut complete = image;
            for section in &sections {
                complete.extend_from_slice(section);
            }
            Ok(complete)
        }
    }

    /// Lows one fixture image and writes the committed fragment, proving the
    /// untouched output tail stayed unchanged.
    fn lower(fix: &Fixture, source: &[u8]) -> Result<Vec<u8>, TestError> {
        let image = fix.encode(source)?;
        let mut facts = FactSet::new();
        collect(source, &image, &mut facts)?;
        let identity = SourceIdentity {
            identity: ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            byte_len: u32::try_from(source.len()).map_err(TestError::from)?,
        };
        let recipe = CompileRecipeFact::derive(
            LanguageProfile::Go(compiler_vocabulary::GoVersion::Go125),
            Stage::LowerIr,
            NativeTool::GoCompiler,
            ContentId::<SourceFactDomain>::from_canonical_bytes(source),
            ContentId::<ToolchainDomain>::from_canonical_bytes(b"go-authority-toolchain"),
        );
        let mut output = vec![0xa5_u8; 65_536];
        let length = crate::lower::admit(&facts, identity, recipe, recipe.profile, &mut output)
            .map_err(TestError::from)?
            .len();
        if !output[length..].iter().all(|byte| *byte == 0xa5) {
            return Err(TestError::Tail);
        }
        output.truncate(length);
        Ok(output)
    }

    /// Borrows one validated type-fact row by its wire ordinal.
    fn row<'fragment>(
        view: &FragmentView<'fragment>,
        ordinal: usize,
    ) -> Result<compiler_ir::DecodedTypeFact<'fragment>, TestError> {
        let mut cursor = view.type_facts().ok_or(TestError::Missing("type facts"))?;
        cursor
            .nth(ordinal)
            .ok_or(TestError::Missing("type row"))?
            .map_err(TestError::from)
    }

    /// Decodes one little-endian payload word.
    fn word(payload: &[u8], at: usize) -> Result<u32, TestError> {
        let bytes = payload.get(at..at + 4).ok_or(TestError::Missing("word"))?;
        let raw: [u8; 4] = bytes.try_into().map_err(|_| TestError::Missing("word"))?;
        Ok(u32::from_le_bytes(raw))
    }

    /// Decodes the Go extension row of one fact: a 16-byte header, one
    /// 20-byte directory per plane (Go is the third), then the row table and
    /// the 28-byte fact pool.
    fn go_extension(
        view: &FragmentView<'_>,
        ordinal: usize,
    ) -> Result<compiler_ir::GoFacts, TestError> {
        let payload = view
            .language_extension_payload()
            .ok_or(TestError::Missing("extension section"))?;
        let directory = 16 + 2 * 20;
        let rows = usize::try_from(word(payload, directory + 4)?).map_err(|_| TestError::Tail)?;
        let facts = word(payload, directory + 8)?;
        let offset =
            usize::try_from(word(payload, directory + 12)?).map_err(|_| TestError::Tail)?;
        if facts == 0 {
            return Err(TestError::Missing("go fact pool"));
        }
        let row_ordinal = word(payload, offset + ordinal * 4)?;
        if row_ordinal == NONE {
            return Err(TestError::Missing("go extension row"));
        }
        let at =
            offset + rows * 4 + usize::try_from(row_ordinal).map_err(|_| TestError::Tail)? * 28;
        compiler_ir::GoFacts::decode(payload, at).ok_or(TestError::Missing("go facts"))
    }

    /// Decodes one pooled reference list from the pools payload, skipping
    /// the type-parameter section and the two earlier lanes.
    fn pooled_list(
        view: &FragmentView<'_>,
        lane: usize,
        ordinal: usize,
    ) -> Result<Vec<u32>, TestError> {
        let pool = view
            .extension_pool_payload()
            .ok_or(TestError::Missing("pool payload"))?;
        let mut cursor = 4_usize;
        let parameters = usize::try_from(word(pool, 0)?).map_err(|_| TestError::Tail)?;
        for _ in 0..parameters {
            let length = usize::try_from(word(pool, cursor + 1)?).map_err(|_| TestError::Tail)?;
            cursor += 5 + length + 10;
        }
        for index in 0..=lane {
            let lists = usize::try_from(word(pool, cursor)?).map_err(|_| TestError::Tail)?;
            cursor += 4;
            for list in 0..lists {
                let length = usize::try_from(word(pool, cursor)?).map_err(|_| TestError::Tail)?;
                cursor += 4;
                if index == lane && list == ordinal {
                    let mut elements = Vec::new();
                    for position in 0..length {
                        elements.push(word(pool, cursor + position * 4)?);
                    }
                    return Ok(elements);
                }
                cursor += length * 4;
            }
        }
        Err(TestError::Missing("pooled list"))
    }

    #[test]
    fn empty_image_admits_the_schema1_fragment_without_semantic_sections() -> Result<(), TestError>
    {
        let fix = Fixture::new();
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        if view.type_facts().is_some() || view.occurrences().is_some() || view.docs().is_some() {
            return Err(TestError::Missing("absent semantic sections"));
        }
        Ok(())
    }

    #[test]
    fn one_named_type_commits_its_diagonal_self_nominal() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        fix.declaration(KIND_TYPE, b"Node", None);
        let bytes = lower(&fix, b"package demo\ntype Node struct{}\n")?;
        let view = FragmentView::validate(&bytes)?;
        let only = row(&view, 0)?;
        if only.owner.raw != 0
            || only.record.tag != SemanticTypeTag::Nominal
            || only.record.nominal != Some(NominalRef::Local(EntityId::new(0)))
        {
            return Err(TestError::Missing("diagonal self nominal"));
        }
        if row(&view, 1).is_ok() {
            return Err(TestError::Missing("single row"));
        }
        Ok(())
    }

    #[test]
    fn recursive_pointer_field_targets_the_backward_nominal() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let node = fix.declaration(KIND_TYPE, b"Node", None);
        let int = fix.basic(b"int");
        let _ = int;
        let named = fix.named(PACKAGE, b"Node", &[]);
        let pointer = fix.unary(ROW_POINTER, named);
        let struct_row = fix.start_row(ROW_STRUCT);
        fix.field(struct_row, b"next", Some(pointer));
        fix.declarations[node].type_root = Some(struct_row);
        let bytes = lower(&fix, b"package demo\ntype Node struct{ next *Node }\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 Node, 1 next. The pointer child names fact 0 directly.
        let field = row(&view, 1)?;
        let children = field_children(&view, &field)?;
        if field.record.tag != SemanticTypeTag::Primitive
            || field.record.payload0 != SHAPE_MUT_POINTER
            || children != vec![0]
        {
            return Err(TestError::Missing("backward pointer field"));
        }
        // Falsifier: retyping the field to a foreign named row folds to the
        // typed unknown that retains the spelling.
        let mut mutated = fix.clone();
        let foreign = mutated.named(b"example.com/other", b"Other", &[]);
        let member_index = mutated
            .members
            .iter()
            .position(|member| member.name.length == 4)
            .ok_or(TestError::Missing("member row"))?;
        mutated.members[member_index].type_root = Some(foreign);
        let other = lower(&mutated, b"package demo\ntype Node struct{ next *Node }\n")?;
        if other == bytes {
            return Err(TestError::Tail);
        }
        let view = FragmentView::validate(&other)?;
        let field = row(&view, 1)?;
        if field.record.tag != SemanticTypeTag::Unknown
            || field.record.text != Some(b"Other".as_slice())
        {
            return Err(TestError::Missing("foreign field fold"));
        }
        Ok(())
    }

    /// Decodes the local child coordinates of one type-fact row from the
    /// raw payload.
    fn field_children<'fragment>(
        view: &FragmentView<'fragment>,
        fact: &compiler_ir::DecodedTypeFact<'fragment>,
    ) -> Result<Vec<u32>, TestError> {
        let payload = view
            .type_fact_payload()
            .ok_or(TestError::Missing("type payload"))?;
        let mut cursor = 4_usize;
        let rows = usize::try_from(word(payload, 0)?).map_err(|_| TestError::Tail)?;
        let mut positions = Vec::new();
        for _ in 0..rows {
            cursor += 4 + 1 + 4 + 4;
            for cell in 0..2 {
                match payload.get(cursor).copied() {
                    Some(0) => cursor += 1,
                    Some(1) => {
                        let length = usize::try_from(word(payload, cursor + 1)?)
                            .map_err(|_| TestError::Tail)?;
                        cursor += 5 + length;
                    }
                    _ => return Err(TestError::Missing("text cell")),
                }
                if cell == 0 {
                    break;
                }
            }
            let nominal = payload
                .get(cursor)
                .copied()
                .ok_or(TestError::Missing("nominal"))?;
            cursor += 1;
            if nominal == 1 {
                cursor += 4;
            } else if nominal == 2 {
                cursor += 16 + 4;
            }
            cursor += 8;
        }
        let start = usize::try_from(fact.record.children.start).map_err(|_| TestError::Tail)?;
        let length = usize::try_from(fact.record.children.length).map_err(|_| TestError::Tail)?;
        for _ in 0..start {
            // Local tag + u32 + empty name cell + flags.
            cursor += 1 + 4 + 1 + 1;
        }
        for _ in 0..length {
            if payload.get(cursor).copied() != Some(0) {
                return Err(TestError::Missing("local child"));
            }
            positions.push(word(payload, cursor + 1)?);
            cursor += 1 + 4 + 1 + 1;
        }
        Ok(positions)
    }

    #[test]
    fn primitives_commit_exact_width_signedness_and_builtin_cells() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let spellings: [(&[u8], &[u8]); 9] = [
            (b"A", b"int"),
            (b"B", b"uint8"),
            (b"C", b"byte"),
            (b"D", b"rune"),
            (b"E", b"float32"),
            (b"F", b"float64"),
            (b"G", b"complex64"),
            (b"H", b"bool"),
            (b"I", b"string"),
        ];
        for (name, spelling) in spellings {
            let basic = fix.basic(spelling);
            fix.declaration(KIND_VAR, name, Some(basic));
        }
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let arch = |signed: bool| (TypeWidth::Arch.to_cell()) | u32::from(signed);
        let expected: [(u32, u32, u32); 9] = [
            (SHAPE_INTEGER, arch(true), 0),
            (SHAPE_INTEGER, 8 << INTEGER_WIDTH_SHIFT, 0),
            (SHAPE_INTEGER, 8 << INTEGER_WIDTH_SHIFT, 0),
            (SHAPE_INTEGER, (32 << INTEGER_WIDTH_SHIFT) | 1, 0),
            (SHAPE_FLOAT, TypeWidth::Fixed(32).to_cell(), 0),
            (SHAPE_FLOAT, TypeWidth::Fixed(64).to_cell(), 0),
            (SHAPE_BUILTIN, 0, 0),
            (SHAPE_BOOL, 0, 0),
            (SHAPE_STR, 0, 0),
        ];
        for (index, (shape, payload1, _)) in expected.iter().enumerate() {
            let fact = row(&view, index)?;
            if fact.record.tag != SemanticTypeTag::Primitive
                || fact.record.payload0 != *shape
                || fact.record.payload1 != *payload1
            {
                return Err(TestError::Missing("primitive cells"));
            }
        }
        let complex_row = row(&view, 6)?;
        if complex_row.record.text != Some(b"complex64".as_slice()) {
            return Err(TestError::Missing("complex builtin spelling"));
        }
        Ok(())
    }

    #[test]
    fn slices_arrays_maps_and_channels_project_their_shapes() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let string = fix.basic(b"string");
        let boolean = fix.basic(b"bool");
        let slice = fix.unary(ROW_SLICE, int);
        let pointer = fix.unary(ROW_POINTER, int);
        let array = fix.array(3, int);
        let map = fix.map(string, boolean);
        let channel = fix.chan(1, int);
        fix.declaration(KIND_VAR, b"S", Some(slice));
        fix.declaration(KIND_VAR, b"P", Some(pointer));
        fix.declaration(KIND_VAR, b"Arr", Some(array));
        fix.declaration(KIND_VAR, b"M", Some(map));
        fix.declaration(KIND_VAR, b"Ch", Some(channel));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Anonymous pool: 0 int, 1 map base, 2 string, 3 bool, 4 chan<- base.
        let slice_row = row(&view, 5)?;
        if slice_row.record.tag != SemanticTypeTag::Slice
            || field_children(&view, &slice_row)? != vec![0]
        {
            return Err(TestError::Missing("slice row"));
        }
        let pointer_row = row(&view, 6)?;
        if pointer_row.record.payload0 != SHAPE_MUT_POINTER
            || field_children(&view, &pointer_row)? != vec![0]
        {
            return Err(TestError::Missing("pointer row"));
        }
        let array_row = row(&view, 7)?;
        if array_row.record.tag != SemanticTypeTag::Array
            || array_row.record.text != Some(b"[]".as_slice())
            || array_row.record.payload0 != 3
            || array_row.record.payload1 != 0
        {
            return Err(TestError::Missing("array length cells"));
        }
        let map_row = row(&view, 8)?;
        if map_row.record.tag != SemanticTypeTag::Apply
            || field_children(&view, &map_row)? != vec![1, 2, 3]
        {
            return Err(TestError::Missing("map application"));
        }
        let base = row(&view, 1)?;
        if base.record.payload0 != SHAPE_BUILTIN || base.record.text != Some(b"map".as_slice()) {
            return Err(TestError::Missing("map base spelling"));
        }
        let chan_row = row(&view, 9)?;
        if chan_row.record.tag != SemanticTypeTag::Apply
            || field_children(&view, &chan_row)? != vec![4, 0]
        {
            return Err(TestError::Missing("channel application"));
        }
        let chan_base = row(&view, 4)?;
        if chan_base.record.text != Some(b"chan<-".as_slice()) {
            return Err(TestError::Missing("send-only base spelling"));
        }
        Ok(())
    }

    #[test]
    fn signatures_commit_carriers_results_and_variadic_flag() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let string = fix.basic(b"string");
        let error = fix.named(b"", b"error", &[]);
        let brew = fix.func(&[int, string], &[int, error], false);
        fix.declaration(KIND_FUNC, b"Brew", Some(brew));
        let slice = fix.unary(ROW_SLICE, int);
        let variadic = fix.func(&[slice], &[], true);
        fix.declaration(KIND_FUNC, b"V", Some(variadic));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 int carrier, 1 string carrier, 2 int result carrier,
        // 3 error result carrier, 4 Brew, 5 xs carrier, 6 V.
        for ordinal in 0..4 {
            let carrier = row(&view, ordinal)?;
            if carrier.record.tag == SemanticTypeTag::FunctionPointer {
                return Err(TestError::Missing("carrier row"));
            }
        }
        let error_carrier = row(&view, 3)?;
        if error_carrier.record.tag != SemanticTypeTag::Unknown
            || error_carrier.record.text != Some(b"error".as_slice())
        {
            return Err(TestError::Missing("universe error fold"));
        }
        let brew = row(&view, 4)?;
        if brew.record.tag != SemanticTypeTag::FunctionPointer
            || brew.record.payload1 != SemanticTypeRecord::RESULT_FLAG
            || field_children(&view, &brew)? != vec![0, 1, 2, 3]
        {
            return Err(TestError::Missing("brew function pointer"));
        }
        let brew_facts = go_extension(&view, 4)?;
        if brew_facts.signature.parameters.raw != 1
            || brew_facts.signature.results.raw != 2
            || brew_facts.signature.variadic
        {
            return Err(TestError::Missing("brew signature lists"));
        }
        let variadic_row = row(&view, 6)?;
        if variadic_row.record.payload1 != 0 {
            return Err(TestError::Missing("void variadic result flag"));
        }
        let variadic_facts = go_extension(&view, 6)?;
        if !variadic_facts.signature.variadic || variadic_facts.signature.results.raw != 0 {
            return Err(TestError::Missing("variadic signature fact"));
        }
        Ok(())
    }

    #[test]
    fn struct_fields_and_interface_methods_join_the_go_extension() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let string = fix.basic(b"string");
        let error = fix.named(b"", b"error", &[]);
        let node = fix.declaration(KIND_TYPE, b"Node", None);
        let node_struct = fix.start_row(ROW_STRUCT);
        fix.field(node_struct, b"Name", Some(string));
        fix.field(node_struct, b"Size", Some(int));
        fix.declarations[node].type_root = Some(node_struct);
        let get = fix.func(&[], &[int], false);
        fix.method(node, b"Get", Some(get));
        let iface = fix.declaration(KIND_TYPE, b"Store", None);
        let iface_row = fix.start_row(ROW_INTERFACE);
        let put = fix.func(&[int], &[error], false);
        fix.interface_method(iface_row, b"Put", Some(put));
        fix.declarations[iface].type_root = Some(iface_row);
        let empty = fix.declaration(KIND_TYPE, b"Bag", None);
        let empty_row = fix.start_row(ROW_INTERFACE);
        fix.declarations[empty].type_root = Some(empty_row);
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 Node, 1 Store, 2 Bag, 3 Name, 4 Size, 5 int result, 6 Get,
        // 7 int param, 8 error result, 9 Put.
        let node_facts = go_extension(&view, 0)?;
        if node_facts.fields.raw != 1 || pooled_list(&view, 2, 1)? != vec![3, 4] {
            return Err(TestError::Missing("node field list"));
        }
        if node_facts.method_set.raw != 2 || pooled_list(&view, 2, 2)? != vec![6] {
            return Err(TestError::Missing("node method set"));
        }
        let store_facts = go_extension(&view, 1)?;
        if store_facts.method_set.raw != 3 || pooled_list(&view, 2, 3)? != vec![9] {
            return Err(TestError::Missing("store method set"));
        }
        let get_row = row(&view, 6)?;
        if get_row.record.tag != SemanticTypeTag::FunctionPointer
            || get_row.record.payload1 != SemanticTypeRecord::RESULT_FLAG
            || field_children(&view, &get_row)? != vec![5]
        {
            return Err(TestError::Missing("get signature row"));
        }
        let put_row = row(&view, 9)?;
        if put_row.record.payload1 != SemanticTypeRecord::RESULT_FLAG
            || field_children(&view, &put_row)? != vec![7, 8]
        {
            return Err(TestError::Missing("put signature row"));
        }
        let bag_facts = go_extension(&view, 2)?;
        if bag_facts.fields.raw != 0 || bag_facts.method_set.raw != 0 {
            return Err(TestError::Missing("empty bag extension"));
        }
        Ok(())
    }

    #[test]
    fn generic_named_roots_apply_base_and_arguments() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let pair = fix.declaration(KIND_TYPE, b"Pair", None);
        let pair_struct = fix.start_row(ROW_STRUCT);
        let param = fix.type_param_row(b"K");
        fix.field(pair_struct, b"Head", Some(param));
        fix.declarations[pair].type_root = Some(pair_struct);
        fix.type_parameter(pair, b"K", None);
        let applied = fix.named(PACKAGE, b"Pair", &[int]);
        fix.declaration(KIND_VAR, b"P", Some(applied));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Wire rows: 0 int (anonymous), 1 Pair, 2 Head (TypeVar K), 3 P.
        let head = row(&view, 2)?;
        if head.record.tag != SemanticTypeTag::TypeVar || head.record.text != Some(b"K".as_slice())
        {
            return Err(TestError::Missing("type parameter reference"));
        }
        let applied_row = row(&view, 3)?;
        if applied_row.record.tag != SemanticTypeTag::Apply
            || field_children(&view, &applied_row)? != vec![1, 0]
        {
            return Err(TestError::Missing("pair application children"));
        }
        let parameters = pooled_type_parameters(&view)?;
        if parameters.len() != 1 || parameters[0].0 != b"K".as_slice() || parameters[0].1.is_some()
        {
            return Err(TestError::Missing("pooled parameter row"));
        }
        Ok(())
    }

    /// Decodes every pooled type parameter's (name, constraint) from the
    /// pools payload.
    fn pooled_type_parameters<'a>(
        view: &'a FragmentView<'a>,
    ) -> Result<Vec<(&'a [u8], Option<u32>)>, TestError> {
        let pool = view
            .extension_pool_payload()
            .ok_or(TestError::Missing("pool payload"))?;
        let count = usize::try_from(word(pool, 0)?).map_err(|_| TestError::Tail)?;
        let mut cursor = 4_usize;
        let mut parameters = Vec::new();
        for _ in 0..count {
            if pool.get(cursor).copied() != Some(1) {
                return Err(TestError::Missing("parameter presence"));
            }
            let length = usize::try_from(word(pool, cursor + 1)?).map_err(|_| TestError::Tail)?;
            let name = pool
                .get(cursor + 5..cursor + 5 + length)
                .ok_or(TestError::Missing("parameter name"))?;
            cursor += 5 + length;
            let constraint = match pool.get(cursor).copied() {
                Some(0) => {
                    cursor += 1;
                    None
                }
                Some(1) => {
                    let raw = word(pool, cursor + 1)?;
                    cursor += 5;
                    Some(raw)
                }
                _ => return Err(TestError::Missing("constraint cell")),
            };
            if pool.get(cursor).copied() != Some(0) {
                return Err(TestError::Missing("default cell"));
            }
            cursor += 1;
            parameters.push((name, constraint));
        }
        Ok(parameters)
    }

    #[test]
    fn nested_named_references_inside_anonymous_rows_fold_with_spelling() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        fix.declaration(KIND_TYPE, b"Node", None);
        let named = fix.named(PACKAGE, b"Node", &[]);
        let slice = fix.unary(ROW_SLICE, named);
        let channel = fix.chan(0, slice);
        fix.declaration(KIND_VAR, b"W", Some(channel));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 Node, 1 W. Anonymous pool: 0 chan base, 1 Node fold,
        // 2 slice. The wire forbids anonymous-to-fact references, so the
        // nested named slice element folds to the typed unknown that
        // retains its spelling while the chan and slice shapes stay exact.
        let channel_row = row(&view, 1)?;
        if channel_row.record.tag != SemanticTypeTag::Apply
            || field_children(&view, &channel_row)? != vec![0, 2]
        {
            return Err(TestError::Missing("channel over slice"));
        }
        let folded = row_by_payload_text(&view, b"Node")?;
        if folded.record.tag != SemanticTypeTag::Unknown
            || folded.record.payload0 != TypeReason::NoIrRepresentation as u32
            || folded.record.text != Some(b"Node".as_slice())
        {
            return Err(TestError::Missing("anonymous named fold"));
        }
        Ok(())
    }

    /// Scans the type-fact payload for the row at the given anonymous wire
    /// ordinal carrying the given text cell.
    fn row_by_payload_text<'fragment>(
        view: &FragmentView<'fragment>,
        text: &[u8],
    ) -> Result<compiler_ir::DecodedTypeFact<'fragment>, TestError> {
        let mut cursor = view.type_facts().ok_or(TestError::Missing("type facts"))?;
        while let Some(fact) = cursor.next() {
            let fact = fact.map_err(TestError::from)?;
            if fact.record.text == Some(text) {
                return Ok(fact);
            }
        }
        Err(TestError::Missing("folded row"))
    }

    #[test]
    fn aliases_project_their_target_records() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let int = fix.basic(b"int");
        let b_decl = fix.declaration(KIND_TYPE, b"B", None);
        let named = fix.named(PACKAGE, b"B", &[]);
        let a_decl = fix.declaration(KIND_ALIAS, b"A", Some(named));
        let _ = a_decl;
        let c_decl = fix.declaration(KIND_ALIAS, b"C", Some(int));
        let _ = c_decl;
        let _ = b_decl;
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 B, 1 A, 2 C.
        let alias = row(&view, 1)?;
        if alias.record.tag != SemanticTypeTag::Nominal
            || alias.record.nominal != Some(NominalRef::Local(EntityId::new(0)))
        {
            return Err(TestError::Missing("alias nominal target"));
        }
        let primitive = row(&view, 2)?;
        if primitive.record.tag != SemanticTypeTag::Primitive
            || primitive.record.payload0 != SHAPE_INTEGER
        {
            return Err(TestError::Missing("alias primitive target"));
        }
        Ok(())
    }

    #[test]
    fn empty_interface_values_commit_the_any_leaf() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let iface = fix.start_row(ROW_INTERFACE);
        fix.declaration(KIND_VAR, b"X", Some(iface));
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let any_row = row(&view, 0)?;
        if any_row.record.tag != SemanticTypeTag::Any {
            return Err(TestError::Missing("any leaf"));
        }
        Ok(())
    }

    #[test]
    fn docs_become_text_and_softbreak_fragments() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let node = fix.declaration(KIND_TYPE, b"Node", None);
        let named = fix.named(PACKAGE, b"Node", &[]);
        let struct_row = fix.start_row(ROW_STRUCT);
        fix.field(struct_row, b"next", Some(named));
        fix.declarations[node].type_root = Some(struct_row);
        fix.doc(
            DOC_DECLARATION,
            u32::try_from(node).map_err(TestError::from)?,
            b"First line.\nSecond line.",
        );
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let mut docs = view.docs().ok_or(TestError::Missing("docs"))?;
        let expected: [DocFragmentInput<'_>; 3] = [
            DocFragmentInput::Text(b"First line."),
            DocFragmentInput::SoftBreak,
            DocFragmentInput::Text(b"Second line."),
        ];
        for fragment in expected {
            let fact = docs.next().ok_or(TestError::Missing("doc fact"))??;
            if fact.owner.raw != 0 || fact.fragment != fragment {
                return Err(TestError::Missing("doc fragment"));
            }
        }
        if docs.next().is_some() {
            return Err(TestError::Missing("exact doc facts"));
        }
        Ok(())
    }

    #[test]
    fn calls_resolve_local_and_foreign_go_lineage_targets() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let brew = fix.declaration(KIND_FUNC, b"Brew", None);
        let pour = fix.declaration(KIND_FUNC, b"Pour", None);
        let _ = pour;
        let node = fix.declaration(KIND_TYPE, b"T", None);
        let get = fix.method(node, b"Get", None);
        let _ = get;
        fix.reference(
            u32::try_from(brew).map_err(TestError::from)?,
            b"",
            b"Pour",
            b"",
            32,
            36,
        );
        fix.reference(
            u32::try_from(node).map_err(TestError::from)?,
            b"t",
            b"Pour",
            b"",
            48,
            52,
        );
        fix.reference(
            u32::try_from(brew).map_err(TestError::from)?,
            b"",
            b"Print",
            b"fmt",
            64,
            69,
        );
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        // Facts: 0 T (pass one), 1 Brew, 2 Pour, 3 Get.
        let mut occurrences = view
            .occurrences()
            .ok_or(TestError::Missing("occurrences"))?;
        let local = occurrences
            .next()
            .ok_or(TestError::Missing("local call"))??;
        if local.owner.raw != 1
            || local.occurrence.target != OccurrenceTarget::Local(EntityId::new(2))
            || local.occurrence.confidence != OccurrenceConfidence::Oracle
            || local.occurrence.span.start != 32
            || local.occurrence.span.end != 36
        {
            return Err(TestError::Missing("local call fact"));
        }
        let method_call = occurrences
            .next()
            .ok_or(TestError::Missing("method call"))??;
        if method_call.owner.raw != 3 {
            return Err(TestError::Missing("method owner ordinal"));
        }
        let foreign = occurrences
            .next()
            .ok_or(TestError::Missing("foreign call"))??;
        let OccurrenceTarget::Foreign(key) = foreign.occurrence.target else {
            return Err(TestError::Missing("foreign target"));
        };
        let ForeignOrigin::Package(lineage) = key.origin else {
            return Err(TestError::Missing("package origin"));
        };
        if lineage.ecosystem != ECOSYSTEM || lineage.name != "fmt" || key.path != "Print" {
            return Err(TestError::Missing("go fmt lineage"));
        }
        if occurrences.next().is_some() {
            return Err(TestError::Missing("exact occurrences"));
        }
        Ok(())
    }

    #[test]
    fn constrained_declarations_carry_their_constraint_atoms() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        fix.constraint(b"windows.go", b"windows", &[(KIND_TYPE, b"WinType")]);
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let facts = go_extension(&view, 0)?;
        if facts.build_constraints.raw != 1 {
            return Err(TestError::Missing("constraint atom list"));
        }
        let listed = pooled_list(&view, 0, 1)?;
        if listed != vec![0] {
            return Err(TestError::Missing("constraint atom coordinate"));
        }
        Ok(())
    }

    #[test]
    fn capacity_beyond_the_lane_rejects_exactly() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        for index in 0..(crate::lower::MAX_EMISSION_FACTS + 1) {
            let mut spelling = b"t".to_vec();
            spelling.extend_from_slice(index.to_string().as_bytes());
            let name = fix.atom(&spelling);
            fix.declarations.push(DeclRow {
                kind: KIND_TYPE,
                name,
                package: Cell {
                    offset: 0,
                    length: 0,
                },
                type_root: None,
            });
        }
        let image = fix.encode(b"package demo\n")?;
        let mut facts = FactSet::new();
        match collect(b"package demo\n", &image, &mut facts) {
            Err(GoCollectError::Lowering(
                compiler_vocabulary::LoweringUnsupported::NoSupportedDeclaration,
            )) => {}
            Err(other) => return Err(TestError::Collect(other)),
            Ok(()) => return Err(TestError::Missing("capacity rejection")),
        }
        Ok(())
    }

    #[test]
    fn member_docs_reach_their_pushed_field_facts() -> Result<(), TestError> {
        let mut fix = Fixture::new();
        let named = fix.named(PACKAGE, b"int", &[]);
        let int = fix.basic(b"int");
        let _ = named;
        let node = fix.declaration(KIND_TYPE, b"Node", None);
        let struct_row = fix.start_row(ROW_STRUCT);
        fix.field(struct_row, b"size", Some(int));
        fix.declarations[node].type_root = Some(struct_row);
        fix.doc(DOC_MEMBER, 0, b"The size in bytes.");
        let bytes = lower(&fix, b"package demo\n")?;
        let view = FragmentView::validate(&bytes)?;
        let mut docs = view.docs().ok_or(TestError::Missing("docs"))?;
        let fact = docs.next().ok_or(TestError::Missing("doc fact"))??;
        if fact.owner.raw != 1 || fact.fragment != DocFragmentInput::Text(b"The size in bytes.") {
            return Err(TestError::Missing("member doc owner"));
        }
        if docs.next().is_some() {
            return Err(TestError::Missing("exact member docs"));
        }
        Ok(())
    }
}
