//! One fallible output buffer shared by bounded wire serializers.
use serde::Serialize;
use std::collections::TryReserveError;
use std::io::{self, Write};

/// Failure before one complete bounded JSON body could be produced.
#[derive(Debug)]
pub enum BoundedJsonError {
    /// Actual encoded bytes exceeded the caller's unchanged ceiling.
    LimitExceeded {
        /// Maximum complete encoded JSON body size.
        limit: usize,
    },
    /// Encoded length could not be represented; not a normal size refusal.
    LengthOverflow,
    /// The bounded output allocation was refused.
    Allocation(TryReserveError),
    /// The value or its wire projection could not be serialized.
    Serialization(serde_json::Error),
}

impl std::fmt::Display for BoundedJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LimitExceeded { limit } => write!(f, "JSON body exceeds {limit} bytes"),
            Self::LengthOverflow => f.write_str("JSON body length overflowed"),
            Self::Allocation(error) => write!(f, "bounded JSON allocation: {error}"),
            Self::Serialization(error) => write!(f, "JSON serialization: {error}"),
        }
    }
}
impl std::error::Error for BoundedJsonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Allocation(error) => Some(error),
            Self::Serialization(error) => Some(error),
            Self::LimitExceeded { .. } | Self::LengthOverflow => None,
        }
    }
}

struct BoundedWriter {
    bytes: Vec<u8>,
    limit: usize,
    failure: Option<BoundedJsonError>,
}
impl BoundedWriter {
    fn checked_length(
        length: usize,
        additional: usize,
        limit: usize,
    ) -> Result<usize, BoundedJsonError> {
        let total = length
            .checked_add(additional)
            .ok_or(BoundedJsonError::LengthOverflow)?;
        if total > limit {
            return Err(BoundedJsonError::LimitExceeded { limit });
        }
        Ok(total)
    }

    fn reserve_to(&mut self, required: usize) -> Result<(), BoundedJsonError> {
        if required > self.limit {
            return Err(BoundedJsonError::LimitExceeded { limit: self.limit });
        }
        if required <= self.bytes.capacity() {
            return Ok(());
        }
        let growth = self
            .bytes
            .capacity()
            .checked_mul(2)
            .unwrap_or(self.limit)
            .max(256);
        let target = required.max(growth).min(self.limit);
        let additional = target
            .checked_sub(self.bytes.len())
            .ok_or(BoundedJsonError::LengthOverflow)?;
        self.bytes
            .try_reserve_exact(additional)
            .map_err(BoundedJsonError::Allocation)
    }

    fn refused(&mut self, failure: BoundedJsonError) -> io::Error {
        if self.failure.is_none() {
            self.failure = Some(failure);
        }
        io::Error::other("bounded JSON writer refused output")
    }
}
impl Write for BoundedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.failure.is_some() {
            return Err(io::Error::other(
                "bounded JSON writer already refused output",
            ));
        }
        let total = Self::checked_length(self.bytes.len(), bytes.len(), self.limit)
            .map_err(|error| self.refused(error))?;
        self.reserve_to(total)
            .map_err(|error| self.refused(error))?;
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(super) fn bounded_json<T: Serialize + ?Sized>(
    value: &T,
    limit: usize,
) -> Result<Vec<u8>, BoundedJsonError> {
    let mut output = BoundedWriter {
        bytes: Vec::new(),
        limit,
        failure: None,
    };
    let result = serde_json::to_writer(&mut output, value);
    if let Some(failure) = output.failure {
        return Err(failure);
    }
    result.map_err(BoundedJsonError::Serialization)?;
    Ok(output.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn below_equal_and_over_bound_are_byte_exact() -> Result<(), Box<dyn std::error::Error>> {
        let value = vec!["line\n\"\\\t", "אבλ🙂"];
        let expected = serde_json::to_vec(&value)?;
        assert_eq!(bounded_json(&value, expected.len() + 1)?, expected);
        assert_eq!(bounded_json(&value, expected.len())?, expected);
        assert!(matches!(
            bounded_json(&value, expected.len() - 1),
            Err(BoundedJsonError::LimitExceeded { .. })
        ));
        assert!(matches!(
            bounded_json(&value, 0),
            Err(BoundedJsonError::LimitExceeded { limit: 0 })
        ));
        Ok(())
    }
    #[test]
    fn escaped_strings_and_incremental_capacity_stay_bounded()
    -> Result<(), Box<dyn std::error::Error>> {
        let text = "\u{0001}\"\\\n🙂".repeat(10_000);
        assert!(matches!(
            bounded_json(&text, 1024),
            Err(BoundedJsonError::LimitExceeded { limit: 1024 })
        ));
        let mut writer = BoundedWriter {
            bytes: Vec::new(),
            limit: 513,
            failure: None,
        };
        for _ in 0..513 {
            writer.write_all(b"a")?;
            assert!(writer.bytes.capacity() <= 513);
        }
        assert!(writer.write_all(b"b").is_err());
        assert_eq!(writer.bytes.len(), 513);
        assert!(writer.bytes.capacity() <= 513);
        Ok(())
    }
    #[test]
    fn arithmetic_allocation_and_serializer_failures_are_distinct() {
        assert!(matches!(
            BoundedWriter::checked_length(usize::MAX, 1, usize::MAX),
            Err(BoundedJsonError::LengthOverflow)
        ));
        let mut writer = BoundedWriter {
            bytes: Vec::new(),
            limit: usize::MAX,
            failure: None,
        };
        assert!(
            matches!(
                writer.reserve_to(usize::MAX),
                Err(BoundedJsonError::Allocation(_))
            ),
            "capacity overflow refuses without requesting physical memory"
        );
        struct Refused;
        impl Serialize for Refused {
            fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
                Err(serde::ser::Error::custom(
                    "deliberate serialization refusal",
                ))
            }
        }
        assert!(matches!(
            bounded_json(&Refused, 1024),
            Err(BoundedJsonError::Serialization(_))
        ));
    }
}
