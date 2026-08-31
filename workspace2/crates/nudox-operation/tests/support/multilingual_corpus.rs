use core::{ops::Range, str::Utf8Error};

pub(crate) const PACKAGE_COUNT: usize = 210;
pub(crate) const SOURCE_BYTE_LIMIT: usize = 96;

const PACKAGES_PER_LANGUAGE: usize = 30;
const ORDINAL_DIGITS: usize = 3;

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
        let template = SourceTemplate::for_language(self.language);
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
            let symbol_start = writer.written();
            writer.write(template.symbol_prefix)?;
            let symbol_digits = writer.write(&ordinal_bytes(self.ordinal)?)?;
            let symbol = symbol_start..symbol_digits.end;
            writer.write(template.middle)?;
            writer.write(&ordinal_bytes(self.ordinal)?)?;
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
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RenderedPackage<'output> {
    pub(crate) source: &'output str,
    pub(crate) expected_symbol: &'output str,
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
    symbol_prefix: &'static [u8],
    middle: &'static [u8],
    suffix: &'static [u8],
}

impl SourceTemplate {
    const fn for_language(language: CorpusLanguage) -> Self {
        match language {
            CorpusLanguage::Rust => Self {
                lead: b"pub const ",
                symbol_prefix: b"package_",
                middle: b": &str = \"rust-",
                suffix: b"\";\n",
            },
            CorpusLanguage::TypeScript => Self {
                lead: b"export const ",
                symbol_prefix: b"package_",
                middle: b" = \"typescript-",
                suffix: b"\";\n",
            },
            CorpusLanguage::Python => Self {
                lead: b"",
                symbol_prefix: b"package_",
                middle: b" = \"python-",
                suffix: b"\"\n",
            },
            CorpusLanguage::Go => Self {
                lead: b"package fixture\nconst ",
                symbol_prefix: b"package_",
                middle: b" = \"go-",
                suffix: b"\"\n",
            },
            CorpusLanguage::Java => Self {
                lead: b"public final class ",
                symbol_prefix: b"Package",
                middle: b" { public static final String NAME = \"java-",
                suffix: b"\"; }\n",
            },
            CorpusLanguage::CSharp => Self {
                lead: b"public static class ",
                symbol_prefix: b"Package",
                middle: b" { public const string Name = \"csharp-",
                suffix: b"\"; }\n",
            },
            CorpusLanguage::Clang => Self {
                lead: b"const char *",
                symbol_prefix: b"package_",
                middle: b" = \"clang-",
                suffix: b"\";\n",
            },
        }
    }

    const fn required_bytes(self) -> usize {
        self.lead.len()
            + self.symbol_prefix.len()
            + ORDINAL_DIGITS
            + self.middle.len()
            + ORDINAL_DIGITS
            + self.suffix.len()
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
