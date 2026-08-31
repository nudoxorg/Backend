//! Exercises the `server-operation` tests support multilingual-corpus contract through its observable boundary.
//! The cases target malformed, partial, reordered, and resource-constrained behavior.
//! Assertions retain exact typed causes so regressions cannot pass through lossy errors.
use core::{ops::Range, str::Utf8Error};

use compiler_ir::{EntityKind, PrimitiveType};

pub(crate) const PACKAGE_COUNT: usize = 210;
pub(crate) const SOURCE_BYTE_LIMIT: usize = 128;

const PACKAGES_PER_LANGUAGE: usize = 30;
const ORDINAL_DIGITS: usize = 3;
const VALUE_VARIANTS: usize = 3;

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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct CorpusPackage {
    pub(crate) ordinal: usize,
    pub(crate) language: CorpusLanguage,
}

impl CorpusPackage {
    pub(crate) fn render(
        self,
        output: &mut [u8],
    ) -> Result<RenderedPackage<'_>, CorpusRenderError> {
        let template = SourceTemplate::for_package(self);
        let required = template.required_bytes();
        if output.len() < required {
            return Err(CorpusRenderError::InsufficientOutput {
                required,
                available: output.len(),
            });
        }

        let (written, symbol) = {
            let mut writer = OutputWriter::new(output);
            writer.write(template.lead)?;
            if !template.outer_prefix.is_empty() {
                writer.write(template.outer_prefix)?;
                writer.write(&ordinal_bytes(self.ordinal)?)?;
            }
            let symbol_start = writer.written();
            writer.write(template.symbol_prefix)?;
            let symbol_digits = writer.write(&ordinal_bytes(self.ordinal)?)?;
            let symbol_start = if template.outer_prefix.is_empty() {
                symbol_start
            } else {
                symbol_start + template.symbol_prefix.len()
            };
            let symbol = symbol_start..symbol_digits.end;
            writer.write(template.value_prefix)?;
            match template.value {
                CorpusValue::Ordinal => writer.write(&ordinal_bytes(self.ordinal)?)?,
                CorpusValue::Fixed(value) => writer.write(value)?,
            };
            writer.write(template.suffix)?;
            (writer.written(), symbol)
        };
        let source_bytes = output
            .get(..written)
            .ok_or(CorpusRenderError::InvalidGeneratedRange { written })?;
        let source = core::str::from_utf8(source_bytes).map_err(CorpusRenderError::Utf8)?;
        let expected_symbol = source
            .get(symbol)
            .ok_or(CorpusRenderError::InvalidGeneratedRange { written })?;
        Ok(RenderedPackage {
            source,
            expected_symbol,
            expected_kind: template.expected_kind,
            expected_type: template.expected_type,
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RenderedPackage<'output> {
    pub(crate) source: &'output str,
    pub(crate) expected_symbol: &'output str,
    pub(crate) expected_kind: EntityKind,
    pub(crate) expected_type: PrimitiveType,
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
        Some(CorpusPackage {
            ordinal,
            language: language_for(ordinal),
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

#[derive(Clone, Copy)]
struct SourceTemplate {
    lead: &'static [u8],
    outer_prefix: &'static [u8],
    symbol_prefix: &'static [u8],
    value_prefix: &'static [u8],
    suffix: &'static [u8],
    value: CorpusValue,
    expected_kind: EntityKind,
    expected_type: PrimitiveType,
}

#[derive(Clone, Copy)]
enum CorpusValue {
    Ordinal,
    Fixed(&'static [u8]),
}

impl SourceTemplate {
    const fn for_package(package: CorpusPackage) -> Self {
        match package.language {
            CorpusLanguage::Rust => rust_template(package.ordinal),
            CorpusLanguage::TypeScript => typescript_template(package.ordinal),
            CorpusLanguage::Python => python_template(package.ordinal),
            CorpusLanguage::Go => go_template(package.ordinal),
            CorpusLanguage::Java => java_template(package.ordinal),
            CorpusLanguage::CSharp => csharp_template(package.ordinal),
            CorpusLanguage::Clang => clang_template(),
        }
    }

    const fn required_bytes(self) -> usize {
        self.lead.len()
            + self.outer_prefix.len()
            + if self.outer_prefix.is_empty() {
                0
            } else {
                ORDINAL_DIGITS
            }
            + self.symbol_prefix.len()
            + ORDINAL_DIGITS
            + self.value_prefix.len()
            + match self.value {
                CorpusValue::Ordinal => ORDINAL_DIGITS,
                CorpusValue::Fixed(value) => value.len(),
            }
            + self.suffix.len()
    }
}

const fn variant(ordinal: usize) -> usize {
    ordinal % VALUE_VARIANTS
}

const fn scalar_type(variant: usize) -> PrimitiveType {
    match variant {
        0 => PrimitiveType::Bool,
        1 => PrimitiveType::I32,
        _ => PrimitiveType::String,
    }
}

const fn rust_template(ordinal: usize) -> SourceTemplate {
    let variant = variant(ordinal);
    SourceTemplate {
        lead: b"pub const ",
        outer_prefix: b"",
        symbol_prefix: b"package_",
        value_prefix: match variant {
            0 => b": bool = ",
            1 => b": i32 = ",
            _ => b": &str = \"rust-",
        },
        suffix: if variant == 2 { b"\";\n" } else { b";\n" },
        value: match variant {
            0 => CorpusValue::Fixed(b"true"),
            1 => CorpusValue::Fixed(b"0"),
            _ => CorpusValue::Ordinal,
        },
        expected_kind: EntityKind::Constant,
        expected_type: scalar_type(variant),
    }
}

const fn typescript_template(ordinal: usize) -> SourceTemplate {
    let variant = variant(ordinal);
    SourceTemplate {
        lead: if variant == 2 {
            b"export function "
        } else {
            b"export const "
        },
        outer_prefix: b"",
        symbol_prefix: b"package_",
        value_prefix: match variant {
            0 => b": boolean = ",
            1 => b": number = ",
            _ => b"(): string { return \"typescript-",
        },
        suffix: if variant == 2 { b"\"; }\n" } else { b";\n" },
        value: match variant {
            0 | 1 => CorpusValue::Fixed(if variant == 0 { b"true" } else { b"0" }),
            _ => CorpusValue::Ordinal,
        },
        expected_kind: if variant == 2 {
            EntityKind::Function
        } else {
            EntityKind::Constant
        },
        expected_type: scalar_type(variant),
    }
}

const fn python_template(ordinal: usize) -> SourceTemplate {
    let variant = variant(ordinal);
    SourceTemplate {
        lead: b"",
        outer_prefix: b"",
        symbol_prefix: b"package_",
        value_prefix: if variant == 2 {
            b" = \"python-"
        } else {
            b" = "
        },
        suffix: if variant == 2 { b"\"\n" } else { b"\n" },
        value: match variant {
            0 => CorpusValue::Fixed(b"True"),
            1 => CorpusValue::Fixed(b"0"),
            _ => CorpusValue::Ordinal,
        },
        expected_kind: EntityKind::Constant,
        expected_type: scalar_type(variant),
    }
}

const fn go_template(ordinal: usize) -> SourceTemplate {
    let variant = variant(ordinal);
    SourceTemplate {
        lead: b"package fixture\n",
        outer_prefix: b"",
        symbol_prefix: b"package_",
        value_prefix: match variant {
            0 => b" bool = ",
            1 => b" int = ",
            _ => b" string = \"go-",
        },
        suffix: if variant == 2 { b"\"\n" } else { b"\n" },
        value: match variant {
            0 => CorpusValue::Fixed(b"true"),
            1 => CorpusValue::Fixed(b"0"),
            _ => CorpusValue::Ordinal,
        },
        expected_kind: EntityKind::Constant,
        expected_type: scalar_type(variant),
    }
}

const fn java_template(ordinal: usize) -> SourceTemplate {
    let variant = variant(ordinal);
    SourceTemplate {
        lead: b"",
        outer_prefix: b"public final class Package",
        symbol_prefix: match variant {
            0 => b" { public static final boolean Package",
            1 => b" { public static final int Package",
            _ => b" { public static final String Package",
        },
        value_prefix: match variant {
            0 => b" = true; }\n",
            1 => b" = 0; }\n",
            _ => b" = \"java-",
        },
        suffix: if variant == 2 { b"\"; }\n" } else { b"" },
        value: if variant == 2 {
            CorpusValue::Ordinal
        } else {
            CorpusValue::Fixed(b"")
        },
        expected_kind: EntityKind::Constant,
        expected_type: scalar_type(variant),
    }
}

const fn csharp_template(ordinal: usize) -> SourceTemplate {
    let variant = variant(ordinal);
    SourceTemplate {
        lead: b"",
        outer_prefix: b"public static class Package",
        symbol_prefix: match variant {
            0 => b" { public const bool Package",
            1 => b" { public const int Package",
            _ => b" { public const string Package",
        },
        value_prefix: match variant {
            0 => b" = true; }\n",
            1 => b" = 0; }\n",
            _ => b" = \"csharp-",
        },
        suffix: if variant == 2 { b"\"; }\n" } else { b"" },
        value: if variant == 2 {
            CorpusValue::Ordinal
        } else {
            CorpusValue::Fixed(b"")
        },
        expected_kind: EntityKind::Constant,
        expected_type: scalar_type(variant),
    }
}

const fn clang_template() -> SourceTemplate {
    SourceTemplate {
        lead: b"const char *",
        outer_prefix: b"",
        symbol_prefix: b"package_",
        value_prefix: b" = \"clang-",
        suffix: b"\";\n",
        value: CorpusValue::Ordinal,
        expected_kind: EntityKind::Constant,
        expected_type: PrimitiveType::String,
    }
}

struct OutputWriter<'output> {
    output: &'output mut [u8],
    written: usize,
}

impl<'output> OutputWriter<'output> {
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
        let destination =
            self.output
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

const fn language_for(ordinal: usize) -> CorpusLanguage {
    match ordinal / PACKAGES_PER_LANGUAGE {
        0 => CorpusLanguage::Rust,
        1 => CorpusLanguage::TypeScript,
        2 => CorpusLanguage::Python,
        3 => CorpusLanguage::Go,
        4 => CorpusLanguage::Java,
        5 => CorpusLanguage::CSharp,
        _ => CorpusLanguage::Clang,
    }
}

const fn ordinal_bytes(ordinal: usize) -> Result<[u8; ORDINAL_DIGITS], CorpusRenderError> {
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
