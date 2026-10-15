//! Public-contract tests for source-syntax type rendering.
//!
//! The canonical encoding is for machines; this renderer is for readers, so
//! each case states a type the way its language writes it, and an
//! unrepresentable type is absent rather than approximated.

use core::num::NonZeroUsize;

use backend_semantic::ir::{
    BuildError, BuiltinType, CanonicalTypeRenderLimits, ConcreteType, ExternalEntityRef,
    ExternalFragmentId, ExternalTarget, Ir, IrBuilder, Mutability, TupleElement, TupleElementKind,
    TypeExpr, TypeId, UnknownReason, UnknownType, VariadicForm, render_source_callable,
    render_source_type,
};
use backend_semantic::vocabulary::{
    GoVersion, LanguageProfile, PythonVersion, RustEdition, TypeScriptSource,
};

const RUST: LanguageProfile = LanguageProfile::Rust(RustEdition::Rust2024);
const GO: LanguageProfile = LanguageProfile::Go(GoVersion::Go125);
const PYTHON: LanguageProfile = LanguageProfile::Python(PythonVersion::Python314);
const TYPESCRIPT: LanguageProfile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);

fn limits() -> CanonicalTypeRenderLimits {
    CanonicalTypeRenderLimits::new(NonZeroUsize::new(32).expect("nonzero"))
}

struct Fixture {
    builder: IrBuilder,
}

impl Fixture {
    fn new() -> Self {
        Self {
            builder: IrBuilder::new(),
        }
    }

    fn builtin(&mut self, builtin: BuiltinType) -> Result<TypeId, BuildError> {
        Ok(self
            .builder
            .intern_concrete(ConcreteType::Builtin(builtin))?
            .erase())
    }

    fn concrete(&mut self, ty: ConcreteType) -> Result<TypeId, BuildError> {
        Ok(self.builder.intern_concrete(ty)?.erase())
    }

    /// An external declaration known by its written spelling.
    fn named(&mut self, name: &str) -> Result<TypeId, BuildError> {
        let display = self.builder.intern_atom(name.as_bytes())?;
        let fragment = ExternalFragmentId::from_canonical_bytes(b"source-type-render");
        let external = self
            .builder
            .intern_external(ExternalTarget::FragmentEntity {
                target: ExternalEntityRef::bind(fragment, 1),
                display,
            })?;
        self.concrete(ConcreteType::External(external))
    }

    /// An unknown type that retains the author's spelling.
    fn spelled(&mut self, name: &str) -> Result<TypeId, BuildError> {
        self.spelled_for(name, UnknownReason::NoIrRepresentation)
    }

    fn spelled_for(&mut self, name: &str, reason: UnknownReason) -> Result<TypeId, BuildError> {
        let spelling = self.builder.intern_atom(name.as_bytes())?;
        Ok(self
            .builder
            .intern_unknown(UnknownType::new(reason).with_spelling(spelling))?
            .erase())
    }

    fn unknown(&mut self, reason: UnknownReason) -> Result<TypeId, BuildError> {
        Ok(self
            .builder
            .intern_unknown(UnknownType::new(reason))?
            .erase())
    }

    fn element(
        &mut self,
        label: Option<&str>,
        ty: TypeId,
        kind: TupleElementKind,
    ) -> Result<TupleElement, BuildError> {
        let label = label
            .map(|label| self.builder.intern_atom(label.as_bytes()))
            .transpose()?;
        Ok(TupleElement { label, ty, kind })
    }

    fn function(
        &mut self,
        parameters: &[TupleElement],
        results: &[TupleElement],
    ) -> Result<TypeId, BuildError> {
        let parameters = self.builder.intern_tuple_elements(parameters)?;
        let results = self.builder.intern_tuple_elements(results)?;
        self.concrete(ConcreteType::Function {
            parameters,
            results,
            abi: None,
            variadic: VariadicForm::None,
            unsafe_: false,
        })
    }

    fn finish(self) -> Result<Ir, BuildError> {
        self.builder.finish()
    }
}

#[test]
fn rust_types_read_as_rust() -> Result<(), BuildError> {
    let mut fixture = Fixture::new();
    let text = fixture.builtin(BuiltinType::String)?;
    let reference = fixture.concrete(ConcreteType::Reference {
        target: text,
        mutability: Mutability::Immutable,
        lifetime: None,
    })?;
    let byte = fixture.builtin(BuiltinType::U8)?;
    let vec = fixture.named("Vec")?;
    let arguments = fixture.builder.intern_types(&[byte])?;
    let bytes = fixture.concrete(ConcreteType::Applied {
        constructor: vec,
        arguments,
    })?;
    let error = fixture.named("InvalidFont")?;
    let result = fixture.named("Result")?;
    let result_arguments = fixture.builder.intern_types(&[bytes, error])?;
    let fallible = fixture.concrete(ConcreteType::Applied {
        constructor: result,
        arguments: result_arguments,
    })?;
    let data = fixture.element(Some("data"), bytes, TupleElementKind::Required)?;
    let returns = fixture.element(None, fallible, TupleElementKind::Required)?;
    let function = fixture.function(&[data], &[returns])?;
    let ir = fixture.finish()?;

    assert_eq!(
        render_source_type(&ir, RUST, reference, limits()).as_deref(),
        Some("&str")
    );
    assert_eq!(
        render_source_type(&ir, RUST, bytes, limits()).as_deref(),
        Some("Vec<u8>")
    );
    assert_eq!(
        render_source_callable(&ir, RUST, "try_from_vec", function, limits()).as_deref(),
        Some("fn try_from_vec(data: Vec<u8>) -> Result<Vec<u8>, InvalidFont>")
    );
    Ok(())
}

#[test]
fn go_callables_and_pointers_read_as_go() -> Result<(), BuildError> {
    let mut fixture = Fixture::new();
    let string = fixture.builtin(BuiltinType::String)?;
    let error = fixture.spelled("error")?;
    let path = fixture.element(Some("path"), string, TupleElementKind::Required)?;
    let first = fixture.element(None, string, TupleElementKind::Required)?;
    let second = fixture.element(None, error, TupleElementKind::Required)?;
    let function = fixture.function(&[path], &[first, second])?;
    let config = fixture.spelled("Config")?;
    let pointer = fixture.concrete(ConcreteType::Pointer {
        target: config,
        mutability: Mutability::Mutable,
    })?;
    let integer = fixture.builtin(BuiltinType::NativeSignedInteger)?;
    let map = fixture.concrete(ConcreteType::Map {
        key: string,
        value: integer,
    })?;
    let ir = fixture.finish()?;

    assert_eq!(
        render_source_callable(&ir, GO, "Expand", function, limits()).as_deref(),
        Some("func Expand(path string) (string, error)")
    );
    assert_eq!(
        render_source_type(&ir, GO, function, limits()).as_deref(),
        Some("func(path string) (string, error)")
    );
    assert_eq!(
        render_source_type(&ir, GO, pointer, limits()).as_deref(),
        Some("*Config")
    );
    assert_eq!(
        render_source_type(&ir, GO, map, limits()).as_deref(),
        Some("map[string]int")
    );
    Ok(())
}

#[test]
fn python_and_typescript_callables_read_in_their_own_syntax() -> Result<(), BuildError> {
    let mut fixture = Fixture::new();
    let string = fixture.builtin(BuiltinType::String)?;
    let void = fixture.builtin(BuiltinType::Void)?;
    let integer = fixture.builtin(BuiltinType::ArbitraryInteger)?;
    let list = fixture.builtin(BuiltinType::List)?;
    let arguments = fixture.builder.intern_types(&[integer])?;
    let integers = fixture.concrete(ConcreteType::Applied {
        constructor: list,
        arguments,
    })?;
    let x = fixture.element(Some("x"), string, TupleElementKind::Required)?;
    let unannotated = fixture.unknown(UnknownReason::Unannotated)?;
    let y = fixture.element(Some("y"), unannotated, TupleElementKind::Required)?;
    let returns = fixture.element(None, string, TupleElementKind::Required)?;
    let nothing = fixture.element(None, void, TupleElementKind::Required)?;
    let python = fixture.function(&[x, y], &[returns])?;
    let typescript = fixture.function(&[x], &[nothing])?;
    let ir = fixture.finish()?;

    assert_eq!(
        render_source_type(&ir, PYTHON, integers, limits()).as_deref(),
        Some("list[int]")
    );
    // A parameter without a known type keeps its name.
    assert_eq!(
        render_source_callable(&ir, PYTHON, "where", python, limits()).as_deref(),
        Some("def where(x: str, y) -> str")
    );
    assert_eq!(
        render_source_type(&ir, TYPESCRIPT, typescript, limits()).as_deref(),
        Some("(x: string) => void")
    );
    assert_eq!(
        render_source_callable(&ir, TYPESCRIPT, "run", typescript, limits()).as_deref(),
        Some("function run(x: string): void")
    );
    Ok(())
}

#[test]
fn what_cannot_be_named_is_absent_never_encoded() -> Result<(), BuildError> {
    let mut fixture = Fixture::new();
    let unannotated = fixture.unknown(UnknownReason::Unannotated)?;
    let gap = fixture.unknown(UnknownReason::OracleGap)?;
    let fragment = ExternalFragmentId::from_canonical_bytes(b"stable-only");
    let stable = fixture.builder.intern_external(ExternalTarget::Stable {
        target: backend_semantic::ir::StableRef {
            fragment,
            declaration: backend_semantic::ir::DeclarationIdentity {
                family: backend_semantic::ir::DeclarationFamilyId::from_raw([3; 16]),
                variant: backend_semantic::ir::VariantFingerprint::from_raw([4; 16]),
            },
        },
    })?;
    let external = fixture.concrete(ConcreteType::External(stable))?;
    let byte = fixture.builtin(BuiltinType::U8)?;
    let arguments = fixture.builder.intern_types(&[byte, external])?;
    let applied = fixture.concrete(ConcreteType::Applied {
        constructor: byte,
        arguments,
    })?;
    // A reference that did not resolve keeps only the bare name the producer
    // saw (`T` for `testing.T`), which is not the type the author wrote.
    let bare = fixture.spelled_for("T", UnknownReason::UnresolvedExternal)?;
    // Go spells a foreign type by its bare name even where the model has no
    // representation for it; only universe types are written.
    let go_bare = fixture.spelled("T")?;
    let go_error = fixture.spelled("error")?;
    let ir = fixture.finish()?;

    for ty in [unannotated, gap, external, applied, bare] {
        for profile in [RUST, GO, PYTHON, TYPESCRIPT] {
            assert_eq!(render_source_type(&ir, profile, ty, limits()), None);
        }
    }
    assert_eq!(render_source_type(&ir, GO, go_bare, limits()), None);
    assert_eq!(
        render_source_type(&ir, GO, go_error, limits()).as_deref(),
        Some("error")
    );
    // A type that is not callable has no header.
    assert_eq!(render_source_callable(&ir, RUST, "f", byte, limits()), None);
    assert!(matches!(
        SemanticReaderExt::kind(&ir, byte),
        Some(TypeExpr::Concrete(_))
    ));
    Ok(())
}

/// Keeps the final assertion above readable without importing the reader
/// trait into every case.
trait SemanticReaderExt {
    fn kind(&self, id: TypeId) -> Option<TypeExpr>;
}

impl SemanticReaderExt for Ir {
    fn kind(&self, id: TypeId) -> Option<TypeExpr> {
        backend_semantic::ir::SemanticReader::ty(self, id)
    }
}
