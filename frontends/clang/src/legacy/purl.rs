//! Parses the closed package URL grammar used to name a C/C++ checkout.

use thiserror::Error;

/// A borrowed, typed generic-package URL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Purl<'input> {
    input: &'input str,
    name: &'input str,
    version: &'input str,
}

impl<'input> Purl<'input> {
    /// Parses `pkg:generic/<name>@<version>` without normalizing caller text.
    pub fn parse(input: &'input str) -> Result<Self, PurlError> {
        if input.len() > MAX_PURL_LENGTH {
            return Err(PurlError::OverLength {
                input: input.to_owned(),
            });
        }
        let Some(rest) = input.strip_prefix("pkg:") else {
            return Err(PurlError::NonLowercaseScheme {
                input: input.to_owned(),
            });
        };
        let Some(rest) = rest.strip_prefix("generic/") else {
            return Err(PurlError::ForeignEcosystem {
                input: input.to_owned(),
            });
        };
        let Some((name, version)) = rest.rsplit_once('@') else {
            return Err(PurlError::MissingVersion {
                input: input.to_owned(),
            });
        };
        if name.is_empty() {
            return Err(PurlError::EmptyName {
                input: input.to_owned(),
            });
        }
        if version.is_empty() {
            return Err(PurlError::MissingVersion {
                input: input.to_owned(),
            });
        }
        if name.len() > MAX_PURL_CELL || version.len() > MAX_PURL_CELL {
            return Err(PurlError::OverLength {
                input: input.to_owned(),
            });
        }
        if !valid_percent_encoding(name) || !valid_percent_encoding(version) {
            return Err(PurlError::BadPercentEncoding {
                input: input.to_owned(),
            });
        }
        Ok(Self {
            input,
            name,
            version,
        })
    }

    /// The original spelling, retained as the URL's authority.
    pub const fn input(self) -> &'input str {
        self.input
    }
    /// The exact package name cell.
    pub const fn name(self) -> &'input str {
        self.name
    }
    /// The exact package version cell.
    pub const fn version(self) -> &'input str {
        self.version
    }
}

const MAX_PURL_LENGTH: usize = 512;
const MAX_PURL_CELL: usize = 256;

fn valid_percent_encoding(cell: &str) -> bool {
    let bytes = cell.as_bytes();
    bytes.iter().enumerate().all(|(index, byte)| {
        *byte != b'%'
            || bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit)
    })
}

/// Exact typed PURL rejection, always retaining the rejected input.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum PurlError {
    /// A non-generic ecosystem was supplied.
    #[error("foreign package ecosystem")]
    /// Rejected URL.
    ForeignEcosystem {
        /// Rejected URL.
        input: String,
    },
    /// The separator and version cell were absent.
    #[error("PURL has no version")]
    /// Rejected URL.
    MissingVersion {
        /// Rejected URL.
        input: String,
    },
    /// The package name cell was empty.
    #[error("PURL has an empty package name")]
    /// Rejected URL.
    EmptyName {
        /// Rejected URL.
        input: String,
    },
    /// A percent sign was not followed by two hexadecimal digits.
    #[error("PURL has invalid percent encoding")]
    /// Rejected URL.
    BadPercentEncoding {
        /// Rejected URL.
        input: String,
    },
    /// The URL or one of its cells exceeded the bound.
    #[error("PURL cell exceeds its bound")]
    /// Rejected URL.
    OverLength {
        /// Rejected URL.
        input: String,
    },
    /// The scheme or package type used uppercase spelling.
    #[error("PURL scheme or type is not lowercase")]
    /// Rejected URL.
    NonLowercaseScheme {
        /// Rejected URL.
        input: String,
    },
}

impl PurlError {
    /// Returns the exact rejected URL spelling.
    pub fn input(&self) -> &str {
        match self {
            Self::ForeignEcosystem { input }
            | Self::MissingVersion { input }
            | Self::EmptyName { input }
            | Self::BadPercentEncoding { input }
            | Self::OverLength { input }
            | Self::NonLowercaseScheme { input } => input,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Purl, PurlError};

    #[test]
    fn purl_table_preserves_typed_cells_and_rejection_input() {
        let accepted = Purl::parse("pkg:generic/lib%2Fbase@1.2.3");
        assert!(accepted.is_ok());
        let Some(accepted) = accepted.ok() else {
            return;
        };
        assert_eq!(
            (accepted.name(), accepted.version()),
            ("lib%2Fbase", "1.2.3")
        );
        let cases = [
            ("pkg:npm/lib@1", 0),
            ("pkg:generic/lib", 1),
            ("pkg:generic/@1", 2),
            ("pkg:generic/lib%@1", 3),
            ("PKG:generic/lib@1", 5),
        ];
        for (input, kind) in cases {
            let Err(error) = Purl::parse(input) else {
                assert!(false, "invalid fixture accepted");
                continue;
            };
            assert_eq!(error.input(), input);
            assert_eq!(
                matches!(
                    (kind, error),
                    (0, PurlError::ForeignEcosystem { .. })
                        | (1, PurlError::MissingVersion { .. })
                        | (2, PurlError::EmptyName { .. })
                        | (3, PurlError::BadPercentEncoding { .. })
                        | (5, PurlError::NonLowercaseScheme { .. })
                ),
                true
            );
        }
    }
}
