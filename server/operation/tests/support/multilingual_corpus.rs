//! Deterministic source fixtures for the compiler output audit.
//!
//! The fixture is intentionally source-first. Each row carries a closed
//! shape and typed expectations; the audit must never recover a fact from a
//! display string or from the row's admission ordinal.

use core::{ops::Range, str::Utf8Error};

use compiler_ir::{BuiltinType, EntityKind, PrimitiveType};

/// Seven language lanes with the same deterministic row budget.
pub(crate) const PACKAGE_COUNT: usize = 210;
pub(crate) const CASES_PER_LANGUAGE: usize = 30;
/// The source lease is deliberately large enough for the richest fixture
/// while remaining a bounded caller-owned region.
pub(crate) const SOURCE_BYTE_LIMIT: usize = 1_024;

const SHAPES_PER_LANGUAGE: usize = 6;
pub(crate) const CASES_PER_SHAPE: usize = CASES_PER_LANGUAGE / SHAPES_PER_LANGUAGE;
const ORDINAL_DIGITS: usize = 3;

/// Closed source package shape used by the matrix and mismatch selectors.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum PackageShape {
    Constant,
    Callable,
    Aggregate,
    Generic,
    Documentation,
    Reference,
}

impl PackageShape {
    pub(crate) const ALL: [Self; SHAPES_PER_LANGUAGE] = [
        Self::Constant,
        Self::Callable,
        Self::Aggregate,
        Self::Generic,
        Self::Documentation,
        Self::Reference,
    ];

    const fn for_local(local: usize) -> Self {
        match local / CASES_PER_SHAPE {
            0 => Self::Constant,
            1 => Self::Callable,
            2 => Self::Aggregate,
            3 => Self::Generic,
            4 => Self::Documentation,
            _ => Self::Reference,
        }
    }
}

/// Closed source authority state expected by one case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CaseAvailability {
    /// The request is expected to produce the full fused semantic result.
    Output,
    /// The profile has a required project/image authority which this test
    /// support crate does not synthesize. The comparator records the exact
    /// typed driver terminal instead of substituting syntax-only facts.
    AuthorityRequired,
    /// The source shape is intentionally outside the selected authority's
    /// closed language contract.
    ExplicitUnsupported(UnsupportedReason),
}

/// Why a fixture is intentionally unsupported.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum UnsupportedReason {
    OverloadNotInLanguage,
    AuthorityShape,
}

/// Availability of one optional semantic plane.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PlaneAvailability {
    /// A source fact is required and must be compared when output exists.
    Required,
    /// The source contract intentionally does not promise this plane. The
    /// observer must still report whether it captured or semantically lacked
    /// the plane; this state is never a parity match.
    ObserverUnavailable,
    /// The selected authority explicitly has no such plane for this shape.
    /// This is a semantic absence, distinct from an observer that cannot see
    /// the plane in the current representation.
    Unavailable,
    /// The selected profile does not promise this plane.
    Unsupported,
}

/// The source-level type category asserted by a fixture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ExpectedType {
    Primitive(PrimitiveType),
    /// Exact owned-IR builtin role, including language-specific numeric
    /// widths/roles which do not fit the compact three-primitive opcode set.
    Builtin(BuiltinType),
    Callable,
    Nominal,
    Structural,
    Generic,
    Reference,
}

/// Expected declaration cardinality. A row is source-parity eligible only
/// with an exact cardinality; `Deferred` is an explicit red terminal while
/// an adapter's synthetic carrier policy is still being made observable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CountExpectation {
    Exact(u16),
    Deferred,
}

/// Expected parentage for the primary source declaration.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ParentExpectation {
    Root,
    Nested,
    Unavailable,
}

/// Renderer result class. There is one neutral renderer today; language
/// dialect renderers are represented explicitly as unsupported until their
/// API is committed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RenderAvailability {
    NeutralRequired,
    DialectUnsupported,
    Unavailable,
}

/// Whether the reference/overload fixture requires an authority occurrence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RelationExpectation {
    None,
    OccurrenceRequired,
    OverloadRequired,
    Unsupported,
}

/// All source-bound expectations for one row. This is intentionally closed:
/// adding a source fact requires adding a typed field or enum variant rather
/// than smuggling a JSON key or display label through the test.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ExpectedFacts {
    pub(crate) shape: PackageShape,
    pub(crate) availability: CaseAvailability,
    pub(crate) primary_kind: EntityKind,
    pub(crate) primary_type: ExpectedType,
    pub(crate) declarations: CountExpectation,
    pub(crate) parent: ParentExpectation,
    pub(crate) nested_member: bool,
    pub(crate) generic_parameters: CountExpectation,
    pub(crate) documentation: PlaneAvailability,
    pub(crate) attributes: PlaneAvailability,
    pub(crate) provenance: PlaneAvailability,
    pub(crate) source_file: PlaneAvailability,
    pub(crate) extension: PlaneAvailability,
    pub(crate) occurrences: PlaneAvailability,
    pub(crate) relation: RelationExpectation,
    pub(crate) neutral_render: RenderAvailability,
    pub(crate) dialect_render: RenderAvailability,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum CorpusLanguage {
    Rust,
    TypeScript,
    Python,
    Go,
    Java,
    CSharp,
    Clang,
}

impl CorpusLanguage {
    pub(crate) const ALL: [Self; 7] = [
        Self::Rust,
        Self::TypeScript,
        Self::Python,
        Self::Go,
        Self::Java,
        Self::CSharp,
        Self::Clang,
    ];
}

/// Stable matrix key. It is independent of admission order and is used to
/// join canonical, reversed, and fixed-shuffle observations.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct CaseId(pub(crate) u16);

impl CaseId {
    pub(crate) const fn raw(self) -> u16 {
        self.0
    }
}

/// Source-independent package/file scope supplied to the compiler request.
/// The generated source bytes and admission ordinal are deliberately absent:
/// they belong to source provenance and current-version payload, not stable
/// declaration identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ScopeSpec {
    pub(crate) ecosystem: &'static str,
    pub(crate) package: &'static str,
    pub(crate) path: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CorpusPackage {
    pub(crate) ordinal: usize,
    pub(crate) language: CorpusLanguage,
    pub(crate) shape: PackageShape,
    pub(crate) case_id: CaseId,
}

impl CorpusPackage {
    /// Returns the validated scope recipe for this language lane.
    pub(crate) const fn scope_spec(self) -> ScopeSpec {
        let (package, path) = match self.language {
            CorpusLanguage::Rust => ("fixture-rust", "src/lib.rs"),
            CorpusLanguage::TypeScript => ("fixture-typescript", "src/index.ts"),
            CorpusLanguage::Python => ("fixture-python", "src/module.py"),
            CorpusLanguage::Go => ("fixture-go", "src/package.go"),
            CorpusLanguage::Java => ("fixture-java", "Package.java"),
            CorpusLanguage::CSharp => ("fixture-csharp", "Package.cs"),
            CorpusLanguage::Clang => ("fixture-clang", "src/module.cc"),
        };
        ScopeSpec {
            ecosystem: "corpus",
            package,
            path,
        }
    }

    /// Returns the closed source-authority expectation carried by this row.
    pub(crate) fn expected_facts(self) -> ExpectedFacts {
        expected_facts(self)
    }

    pub(crate) fn render(
        self,
        output: &mut [u8],
    ) -> Result<RenderedPackage<'_>, CorpusRenderError> {
        // Render into a bounded staging lease first. This makes the required
        // byte count exact while preserving the caller's buffer byte-for-byte
        // when its lease is too short.
        let mut staged = [0_u8; SOURCE_BYTE_LIMIT];
        let mut writer = SourceWriter::new(&mut staged);
        let mut symbol = None;
        let mut member = None;
        match self.shape {
            PackageShape::Constant => write_constant(self, &mut writer, &mut symbol)?,
            PackageShape::Callable => write_callable(self, &mut writer, &mut symbol)?,
            PackageShape::Aggregate => {
                write_aggregate(self, &mut writer, &mut symbol, &mut member)?
            }
            PackageShape::Generic => {
                write_generic(self, &mut writer, &mut symbol, &mut member)?
            }
            PackageShape::Documentation => write_documentation(self, &mut writer, &mut symbol)?,
            PackageShape::Reference => write_reference(self, &mut writer, &mut symbol)?,
        }
        let written = writer.written();
        let source_bytes = staged
            .get(..written)
            .ok_or(CorpusRenderError::InvalidGeneratedRange { written })?;
        if output.len() < written {
            return Err(CorpusRenderError::InsufficientOutput {
                required: written,
                available: output.len(),
            });
        }
        output[..written].copy_from_slice(source_bytes);
        let source = core::str::from_utf8(&output[..written]).map_err(CorpusRenderError::Utf8)?;
        let symbol = symbol.ok_or(CorpusRenderError::MissingSymbol)?;
        let symbol = source
            .get(symbol)
            .ok_or(CorpusRenderError::InvalidGeneratedRange { written })?;
        let member = member
            .map(|range| {
                source
                    .get(range)
                    .ok_or(CorpusRenderError::InvalidGeneratedRange { written })
            })
            .transpose()?;
        let expected = expected_facts(self);
        Ok(RenderedPackage {
            source,
            expected_symbol: symbol,
            expected_member: member,
            expected,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RenderedPackage<'output> {
    pub(crate) source: &'output str,
    pub(crate) expected_symbol: &'output str,
    pub(crate) expected_member: Option<&'output str>,
    pub(crate) expected: ExpectedFacts,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum CorpusRenderError {
    #[error("corpus source requires {required} output bytes, but only {available} are available")]
    InsufficientOutput { required: usize, available: usize },
    #[error("corpus ordinal {observed} exceeded the three-digit limit {limit}")]
    OrdinalOutOfRange { limit: usize, observed: usize },
    #[error("appending {appended} bytes to the {written}-byte corpus prefix overflowed")]
    LengthOverflow { written: usize, appended: usize },
    #[error("generated corpus range ended outside its {written} written bytes")]
    InvalidGeneratedRange { written: usize },
    #[error("generated corpus source was not UTF-8")]
    Utf8(#[source] Utf8Error),
    #[error("generated corpus package did not record a primary symbol")]
    MissingSymbol,
}

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct CorpusPackages {
    next: usize,
}

impl Iterator for CorpusPackages {
    type Item = CorpusPackage;

    fn next(&mut self) -> Option<Self::Item> {
        if self.next == PACKAGE_COUNT {
            return None;
        }
        let ordinal = self.next;
        self.next += 1;
        let local = ordinal % CASES_PER_LANGUAGE;
        Some(CorpusPackage {
            ordinal,
            language: language_for(ordinal),
            shape: PackageShape::for_local(local),
            case_id: CaseId(ordinal as u16),
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = PACKAGE_COUNT - self.next;
        (remaining, Some(remaining))
    }
}

impl ExactSizeIterator for CorpusPackages {}

pub(crate) const fn corpus_packages() -> CorpusPackages {
    CorpusPackages { next: 0 }
}

fn expected_facts(package: CorpusPackage) -> ExpectedFacts {
    let scalar = scalar_type(package.ordinal % 3);
    let scalar_expected = match (package.language, package.ordinal % 3) {
        (CorpusLanguage::Rust, 2) => ExpectedType::Reference,
        (CorpusLanguage::Clang, _) => ExpectedType::Structural,
        (CorpusLanguage::TypeScript, 0) => ExpectedType::Builtin(BuiltinType::Bool),
        (CorpusLanguage::TypeScript, 1) => ExpectedType::Builtin(BuiltinType::Number),
        (CorpusLanguage::TypeScript, 2) => ExpectedType::Builtin(BuiltinType::String),
        (CorpusLanguage::Python, 0) => ExpectedType::Builtin(BuiltinType::Bool),
        (CorpusLanguage::Python, 1) => ExpectedType::Builtin(BuiltinType::ArbitraryInteger),
        (CorpusLanguage::Python, 2) => ExpectedType::Builtin(BuiltinType::String),
        (CorpusLanguage::Go, 0) => ExpectedType::Builtin(BuiltinType::Bool),
        (CorpusLanguage::Go, 1) => ExpectedType::Builtin(BuiltinType::NativeSignedInteger),
        (CorpusLanguage::Go, 2) => ExpectedType::Builtin(BuiltinType::String),
        (CorpusLanguage::Java, 0) => ExpectedType::Builtin(BuiltinType::Bool),
        (CorpusLanguage::Java, 1) => ExpectedType::Builtin(BuiltinType::I32),
        (CorpusLanguage::Java, 2) => ExpectedType::Builtin(BuiltinType::String),
        (CorpusLanguage::CSharp, 0) => ExpectedType::Builtin(BuiltinType::Bool),
        (CorpusLanguage::CSharp, 1) => ExpectedType::Builtin(BuiltinType::I32),
        (CorpusLanguage::CSharp, 2) => ExpectedType::Builtin(BuiltinType::String),
        (CorpusLanguage::Rust, 0) => ExpectedType::Builtin(BuiltinType::Bool),
        (CorpusLanguage::Rust, 1) => ExpectedType::Builtin(BuiltinType::I32),
    };
    // Every ordinary row has a real authority builder in the audit harness.
    // If a host lacks that producer, the run records a typed
    // `LocallyUnavailable` terminal and excludes the row from parity counts;
    // it never downgrades to syntax-only facts. `AuthorityRequired` remains a
    // closed state for explicit negative probes outside this 210-row census.
    let authority = CaseAvailability::Output;
    let (primary_kind, primary_type, declarations, parent, nested_member, generic_parameters) =
        match package.shape {
            PackageShape::Constant => (
                match package.language {
                    CorpusLanguage::Java => EntityKind::Field,
                    CorpusLanguage::Python | CorpusLanguage::Clang => EntityKind::Static,
                    _ => EntityKind::Constant,
                },
                scalar_expected,
                CountExpectation::Exact(match package.language {
                    CorpusLanguage::Java | CorpusLanguage::CSharp => 2,
                    _ => 1,
                }),
                if matches!(package.language, CorpusLanguage::Java | CorpusLanguage::CSharp) {
                    ParentExpectation::Nested
                } else {
                    ParentExpectation::Root
                },
                false,
                CountExpectation::Exact(0),
            ),
            PackageShape::Callable => (
                EntityKind::Function,
                ExpectedType::Callable,
                CountExpectation::Exact(match package.language {
                    CorpusLanguage::Java | CorpusLanguage::CSharp => 4,
                    _ => 3,
                }),
                if matches!(package.language, CorpusLanguage::Java | CorpusLanguage::CSharp) {
                    ParentExpectation::Nested
                } else {
                    ParentExpectation::Root
                },
                false,
                CountExpectation::Exact(0),
            ),
            PackageShape::Aggregate => (
                match package.language {
                    CorpusLanguage::TypeScript => EntityKind::Trait,
                    _ => EntityKind::Record,
                },
                ExpectedType::Nominal,
                CountExpectation::Exact(2),
                ParentExpectation::Root,
                true,
                CountExpectation::Exact(0),
            ),
            PackageShape::Generic => (
                match package.language {
                    CorpusLanguage::Rust | CorpusLanguage::TypeScript => EntityKind::Alias,
                    _ => EntityKind::Record,
                },
                if matches!(package.language, CorpusLanguage::Rust | CorpusLanguage::TypeScript) {
                    ExpectedType::Generic
                } else {
                    ExpectedType::Nominal
                },
                CountExpectation::Exact(match package.language {
                    CorpusLanguage::Rust => 1,
                    _ => 2,
                }),
                ParentExpectation::Root,
                !matches!(
                    package.language,
                    CorpusLanguage::Rust | CorpusLanguage::TypeScript
                ),
                if matches!(package.language, CorpusLanguage::Python | CorpusLanguage::Java) {
                    CountExpectation::Deferred
                } else {
                    CountExpectation::Exact(1)
                },
            ),
            PackageShape::Documentation => (
                EntityKind::Function,
                ExpectedType::Callable,
                CountExpectation::Exact(match package.language {
                    CorpusLanguage::Rust => 2,
                    CorpusLanguage::Java | CorpusLanguage::CSharp => 2,
                    _ => 1,
                }),
                if matches!(package.language, CorpusLanguage::Java | CorpusLanguage::CSharp) {
                    ParentExpectation::Nested
                } else {
                    ParentExpectation::Root
                },
                false,
                CountExpectation::Exact(0),
            ),
            PackageShape::Reference => (
                EntityKind::Function,
                ExpectedType::Callable,
                CountExpectation::Exact(match package.language {
                    CorpusLanguage::Java | CorpusLanguage::CSharp => 9,
                    CorpusLanguage::TypeScript => 3,
                    CorpusLanguage::Python | CorpusLanguage::Rust | CorpusLanguage::Go
                    | CorpusLanguage::Clang => 4,
                }),
                if matches!(package.language, CorpusLanguage::Java | CorpusLanguage::CSharp) {
                    ParentExpectation::Nested
                } else {
                    ParentExpectation::Root
                },
                false,
                CountExpectation::Exact(0),
            ),
        };
    // Every producer marks the declaration documentation authority, including
    // a proved-empty doc list. Attribute capture is currently closed only by
    // the languages whose extension row owns an attribute/decorator list.
    let documentation = PlaneAvailability::Required;
    let attributes = if matches!(
        package.language,
        CorpusLanguage::Python | CorpusLanguage::Java | CorpusLanguage::CSharp
    ) {
        PlaneAvailability::Required
    } else {
        PlaneAvailability::Unavailable
    };
    let extension = PlaneAvailability::Required;
    let occurrences = if package.shape == PackageShape::Reference {
        PlaneAvailability::Required
    } else {
        PlaneAvailability::Unavailable
    };
    let relation = if package.shape != PackageShape::Reference {
        RelationExpectation::None
    } else if matches!(package.language, CorpusLanguage::Java | CorpusLanguage::CSharp) {
        RelationExpectation::OverloadRequired
    } else {
        RelationExpectation::OccurrenceRequired
    };
    ExpectedFacts {
        shape: package.shape,
        availability: authority,
        primary_kind,
        primary_type,
        declarations,
        parent,
        nested_member,
        generic_parameters,
        documentation,
        attributes,
        provenance: PlaneAvailability::Required,
        source_file: PlaneAvailability::Required,
        extension,
        occurrences,
        relation,
        neutral_render: RenderAvailability::NeutralRequired,
        dialect_render: RenderAvailability::DialectUnsupported,
    }
}

fn write_constant(
    package: CorpusPackage,
    writer: &mut SourceWriter<'_>,
    symbol: &mut Option<Range<usize>>,
) -> Result<(), CorpusRenderError> {
    match package.language {
        CorpusLanguage::Rust => {
            writer.write(b"pub const ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b": ")?;
            writer.write(rust_type(package.ordinal % 3))?;
            writer.write(b" = ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b";\n")?;
        }
        CorpusLanguage::TypeScript => {
            writer.write(b"export const ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b": ")?;
            writer.write(typescript_type(package.ordinal % 3))?;
            writer.write(b" = ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b";\n")?;
        }
        CorpusLanguage::Python => {
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b": ")?;
            writer.write(python_type(package.ordinal % 3))?;
            writer.write(b" = ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b"\n")?;
        }
        CorpusLanguage::Go => {
            writer.write(b"package fixture\n\nconst ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b" ")?;
            writer.write(go_type(package.ordinal % 3))?;
            writer.write(b" = ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b"\n")?;
        }
        CorpusLanguage::Java => {
            writer.write(b"public final class Package")?;
            writer.write(b" { public static final ")?;
            writer.write(java_type(package.ordinal % 3))?;
            writer.write(b" ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b" = ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b"; }\n")?;
        }
        CorpusLanguage::CSharp => {
            writer.write(b"public static class Package")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b" { public const ")?;
            writer.write(csharp_type(package.ordinal % 3))?;
            writer.write(b" ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b" = ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b"; }\n")?;
        }
        CorpusLanguage::Clang => {
            writer.write(b"const ")?;
            writer.write(clang_type(package.ordinal % 3))?;
            writer.write(b" ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b" = ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b";\n")?;
        }
    }
    Ok(())
}

fn write_callable(
    package: CorpusPackage,
    writer: &mut SourceWriter<'_>,
    symbol: &mut Option<Range<usize>>,
) -> Result<(), CorpusRenderError> {
    match package.language {
        CorpusLanguage::Rust => {
            writer.write(b"pub fn ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"(argument: i32) -> ")?;
            writer.write(rust_type(package.ordinal % 3))?;
            writer.write(b" { ")?;
            writer.write(rust_value(package.ordinal % 3, package.ordinal)?)?;
            writer.write(b" }\n")?;
        }
        CorpusLanguage::TypeScript => {
            writer.write(b"export function ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"(argument: number): ")?;
            writer.write(typescript_type(package.ordinal % 3))?;
            writer.write(b" { return ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b"; }\n")?;
        }
        CorpusLanguage::Python => {
            writer.write(b"def ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"(argument: int) -> ")?;
            writer.write(python_type(package.ordinal % 3))?;
            writer.write(b":\n    return ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b"\n")?;
        }
        CorpusLanguage::Go => {
            writer.write(b"package fixture\n\nfunc ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"(argument int) ")?;
            writer.write(go_type(package.ordinal % 3))?;
            writer.write(b" { return ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b" }\n")?;
        }
        CorpusLanguage::Java => {
            writer.write(b"public final class Package")?;
            writer.write(b" { public static ")?;
            writer.write(java_type(package.ordinal % 3))?;
            writer.write(b" ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"(int argument) { return ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b"; } }\n")?;
        }
        CorpusLanguage::CSharp => {
            writer.write(b"public static class Package")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b" { public static ")?;
            writer.write(csharp_type(package.ordinal % 3))?;
            writer.write(b" ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"(int argument) => ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b"; }\n")?;
        }
        CorpusLanguage::Clang => {
            writer.write(clang_type(package.ordinal % 3))?;
            writer.write(b" ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"(int argument) { return ")?;
            write_value(writer, package.language, package.ordinal % 3, package.ordinal)?;
            writer.write(b"; }\n")?;
        }
    }
    Ok(())
}

fn write_aggregate(
    package: CorpusPackage,
    writer: &mut SourceWriter<'_>,
    symbol: &mut Option<Range<usize>>,
    member: &mut Option<Range<usize>>,
) -> Result<(), CorpusRenderError> {
    match package.language {
        CorpusLanguage::Rust => {
            writer.write(b"pub struct ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b" { pub ")?;
            *member = Some(write_identifier(writer, b"member_", package.ordinal)?);
            writer.write(b": i32 }\n")?;
        }
        CorpusLanguage::TypeScript => {
            writer.write(b"export interface ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b" { ")?;
            *member = Some(write_identifier(writer, b"member_", package.ordinal)?);
            writer.write(b": number; }\n")?;
        }
        CorpusLanguage::Python => {
            writer.write(b"class ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b":\n    ")?;
            *member = Some(write_identifier(writer, b"member_", package.ordinal)?);
            writer.write(b": int = 0\n")?;
        }
        CorpusLanguage::Go => {
            writer.write(b"package fixture\n\ntype ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b" struct { ")?;
            *member = Some(write_identifier(writer, b"Member_", package.ordinal)?);
            writer.write(b" int }\n")?;
        }
        CorpusLanguage::Java => {
            writer.write(b"public class ")?;
            let symbol_start = writer.written();
            writer.write(b"Package")?;
            *symbol = Some(symbol_start..writer.written());
            writer.write(b" { public int ")?;
            *member = Some(write_identifier(writer, b"member_", package.ordinal)?);
            writer.write(b"; }\n")?;
        }
        CorpusLanguage::CSharp => {
            writer.write(b"public class ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b" { public int ")?;
            *member = Some(write_identifier(writer, b"member_", package.ordinal)?);
            writer.write(b"; }\n")?;
        }
        CorpusLanguage::Clang => {
            writer.write(b"struct ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b" { int ")?;
            *member = Some(write_identifier(writer, b"member_", package.ordinal)?);
            writer.write(b"; };\n")?;
        }
    }
    Ok(())
}

fn write_generic(
    package: CorpusPackage,
    writer: &mut SourceWriter<'_>,
    symbol: &mut Option<Range<usize>>,
    member: &mut Option<Range<usize>>,
) -> Result<(), CorpusRenderError> {
    match package.language {
        CorpusLanguage::Rust => {
            writer.write(b"pub type ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"<T> = T;\n")?;
        }
        CorpusLanguage::TypeScript => {
            writer.write(b"export type ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"<T> = T;\n")?;
        }
        CorpusLanguage::Python => {
            writer.write(b"class ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"[T]:\n    ")?;
            *member = Some(writer.write(b"value")?);
            writer.write(b": T\n")?;
        }
        CorpusLanguage::Go => {
            writer.write(b"package fixture\n\ntype ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"[T any] struct { ")?;
            *member = Some(writer.write(b"Value")?);
            writer.write(b" T }\n")?;
        }
        CorpusLanguage::Java => {
            writer.write(b"public class ")?;
            let symbol_start = writer.written();
            writer.write(b"Package")?;
            *symbol = Some(symbol_start..writer.written());
            writer.write(b"<T> { public T ")?;
            *member = Some(writer.write(b"value")?);
            writer.write(b"; }\n")?;
        }
        CorpusLanguage::CSharp => {
            writer.write(b"public class ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"<T> { public T ")?;
            *member = Some(writer.write(b"value")?);
            writer.write(b"; }\n")?;
        }
        CorpusLanguage::Clang => {
            writer.write(b"template <typename T> struct ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b" { T ")?;
            *member = Some(writer.write(b"value")?);
            writer.write(b"; };\n")?;
        }
    }
    Ok(())
}

fn write_documentation(
    package: CorpusPackage,
    writer: &mut SourceWriter<'_>,
    symbol: &mut Option<Range<usize>>,
) -> Result<(), CorpusRenderError> {
    match package.language {
        CorpusLanguage::Rust => {
            writer.write(b"/// package documentation\n#[deprecated]\npub fn ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"() {}\n")?;
        }
        CorpusLanguage::TypeScript => {
            writer.write(b"/** package documentation */\nexport function ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"() {}\n")?;
        }
        CorpusLanguage::Python => {
            writer.write(b"@staticmethod\ndef ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"():\n    \"\"\"package documentation\"\"\"\n    return 0\n")?;
        }
        CorpusLanguage::Go => {
            writer.write(b"package fixture\n\n// package documentation\nfunc ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"() {}\n")?;
        }
        CorpusLanguage::Java => {
            writer.write(b"public final class Package")?;
            writer.write(b" { /** package documentation */ @Deprecated public static void ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"() {} }\n")?;
        }
        CorpusLanguage::CSharp => {
            writer.write(b"public static class Package")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b" { /// package documentation\n [Obsolete] public static void ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"() {} }\n")?;
        }
        CorpusLanguage::Clang => {
            writer.write(b"/// package documentation\n[[deprecated]] void ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"() {}\n")?;
        }
    }
    Ok(())
}

fn write_reference(
    package: CorpusPackage,
    writer: &mut SourceWriter<'_>,
    symbol: &mut Option<Range<usize>>,
) -> Result<(), CorpusRenderError> {
    let scalar = package.ordinal % 3;
    match package.language {
        CorpusLanguage::Rust => {
            writer.write(b"fn target_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"() -> i32 { 0 }\nfn ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"() -> i32 { target_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"() }\n")?;
        }
        CorpusLanguage::TypeScript => {
            writer.write(b"function target_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"(): number { return 0; }\nexport function ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"(): ")?;
            writer.write(typescript_type(scalar))?;
            writer.write(b" { return target_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"(); }\n")?;
        }
        CorpusLanguage::Python => {
            writer.write(b"def target_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"() -> int:\n    return 0\n\ndef ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"() -> int:\n    return target_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"()\n")?;
        }
        CorpusLanguage::Go => {
            writer.write(b"package fixture\n\nfunc target_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"() int { return 0 }\nfunc ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"() int { return target_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"() }\n")?;
        }
        CorpusLanguage::Java => {
            writer.write(b"public class Package")?;
            writer.write(b" { public static int ")?;
            writer.write(b" ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"(int value) { return value; } public static int ")?;
            writer.write(b"package_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"(boolean value) { return value ? 1 : 0; } public static int use_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"() { return package_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"(1); } }\n")?;
        }
        CorpusLanguage::CSharp => {
            writer.write(b"public static class Package")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b" { public static int ")?;
            writer.write(b" ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"(int value) => value; public static int ")?;
            writer.write(b"package_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"(bool value) => value ? 1 : 0; public static int use_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"() => package_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"(1); }\n")?;
        }
        CorpusLanguage::Clang => {
            writer.write(b"int target_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"() { return 0; }\nint ")?;
            *symbol = Some(write_identifier(writer, b"package_", package.ordinal)?);
            writer.write(b"() { return target_")?;
            writer.write(&ordinal_bytes(package.ordinal)?)?;
            writer.write(b"(); }\n")?;
        }
    }
    Ok(())
}

struct SourceWriter<'output> {
    output: &'output mut [u8],
    written: usize,
}

impl<'output> SourceWriter<'output> {
    const fn new(output: &'output mut [u8]) -> Self {
        Self { output, written: 0 }
    }

    fn write(&mut self, input: &[u8]) -> Result<Range<usize>, CorpusRenderError> {
        let start = self.written;
        let end = start
            .checked_add(input.len())
            .ok_or(CorpusRenderError::LengthOverflow {
                written: start,
                appended: input.len(),
            })?;
        let available = self.output.len();
        let destination = self
            .output
            .get_mut(start..end)
            .ok_or(CorpusRenderError::InsufficientOutput {
                required: end,
                available,
            })?;
        destination.copy_from_slice(input);
        self.written = end;
        Ok(start..end)
    }

    const fn written(&self) -> usize {
        self.written
    }
}

fn write_identifier(
    writer: &mut SourceWriter<'_>,
    prefix: &[u8],
    ordinal: usize,
) -> Result<Range<usize>, CorpusRenderError> {
    let prefix = writer.write(prefix)?;
    let digits = writer.write(&ordinal_bytes(ordinal)?)?;
    Ok(prefix.start..digits.end)
}

fn write_value(
    writer: &mut SourceWriter<'_>,
    language: CorpusLanguage,
    variant: usize,
    ordinal: usize,
) -> Result<(), CorpusRenderError> {
    match variant {
        0 => writer.write(if matches!(language, CorpusLanguage::Python) {
            b"True"
        } else {
            b"true"
        })?,
        1 => writer.write(b"0")?,
        _ => {
            writer.write(b"\"")?;
            writer.write(string_prefix(language))?;
            writer.write(&ordinal_bytes(ordinal)?)?;
            writer.write(b"\"")?;
        }
    };
    Ok(())
}

fn rust_value(variant: usize, ordinal: usize) -> Result<&'static [u8], CorpusRenderError> {
    match variant {
        0 => Ok(b"true"),
        1 => Ok(b"0"),
        _ => {
            let _ = ordinal;
            Ok(b"\"rust-value\"")
        }
    }
}

const fn scalar_type(variant: usize) -> PrimitiveType {
    match variant {
        0 => PrimitiveType::Bool,
        1 => PrimitiveType::I32,
        _ => PrimitiveType::String,
    }
}

const fn rust_type(variant: usize) -> &'static [u8] {
    match variant {
        0 => b"bool",
        1 => b"i32",
        _ => b"&str",
    }
}

const fn typescript_type(variant: usize) -> &'static [u8] {
    match variant {
        0 => b"boolean",
        1 => b"number",
        _ => b"string",
    }
}

const fn python_type(variant: usize) -> &'static [u8] {
    match variant {
        0 => b"bool",
        1 => b"int",
        _ => b"str",
    }
}

const fn go_type(variant: usize) -> &'static [u8] {
    match variant {
        0 => b"bool",
        1 => b"int",
        _ => b"string",
    }
}

const fn java_type(variant: usize) -> &'static [u8] {
    match variant {
        0 => b"boolean",
        1 => b"int",
        _ => b"String",
    }
}

const fn csharp_type(variant: usize) -> &'static [u8] {
    match variant {
        0 => b"bool",
        1 => b"int",
        _ => b"string",
    }
}

const fn clang_type(variant: usize) -> &'static [u8] {
    match variant {
        0 | 1 => b"int",
        _ => b"const char *",
    }
}

const fn string_prefix(language: CorpusLanguage) -> &'static [u8] {
    match language {
        CorpusLanguage::Rust => b"rust-",
        CorpusLanguage::TypeScript => b"typescript-",
        CorpusLanguage::Python => b"python-",
        CorpusLanguage::Go => b"go-",
        CorpusLanguage::Java => b"java-",
        CorpusLanguage::CSharp => b"csharp-",
        CorpusLanguage::Clang => b"clang-",
    }
}

const fn language_for(ordinal: usize) -> CorpusLanguage {
    match ordinal / CASES_PER_LANGUAGE {
        0 => CorpusLanguage::Rust,
        1 => CorpusLanguage::TypeScript,
        2 => CorpusLanguage::Python,
        3 => CorpusLanguage::Go,
        4 => CorpusLanguage::Java,
        5 => CorpusLanguage::CSharp,
        _ => CorpusLanguage::Clang,
    }
}

fn ordinal_bytes(ordinal: usize) -> Result<[u8; ORDINAL_DIGITS], CorpusRenderError> {
    const ORDINAL_LIMIT: usize = 999;
    if ordinal > ORDINAL_LIMIT {
        return Err(CorpusRenderError::OrdinalOutOfRange {
            limit: ORDINAL_LIMIT,
            observed: ordinal,
        });
    }
    Ok([
        digit_byte((ordinal / 100) % 10),
        digit_byte((ordinal / 10) % 10),
        digit_byte(ordinal % 10),
    ])
}

const fn digit_byte(digit: usize) -> u8 {
    match digit {
        0 => b'0',
        1 => b'1',
        2 => b'2',
        3 => b'3',
        4 => b'4',
        5 => b'5',
        6 => b'6',
        7 => b'7',
        8 => b'8',
        _ => b'9',
    }
}
