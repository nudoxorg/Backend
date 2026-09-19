//! Checked paths for local Unix process endpoints.

use std::fmt;
use std::path::{Path, PathBuf};

/// Portable byte budget for a local Unix socket path.
///
/// Unix families expose slightly different `sun_path` capacities. Keeping a
/// conservative shared budget makes an admitted endpoint portable across the
/// supported hosts and reserves space for the terminating NUL where required.
pub const MAX_UNIX_ENDPOINT_PATH_BYTES: usize = 100;

/// A borrowed Unix endpoint that has passed the shared path invariant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnixEndpointRef<'path> {
    path: &'path Path,
}

impl<'path> UnixEndpointRef<'path> {
    /// Admits a nonempty endpoint within the portable Unix socket budget.
    ///
    /// # Errors
    ///
    /// Returns an error when the encoded path is empty or too long.
    pub fn new(path: &'path Path) -> Result<Self, UnixEndpointPathError> {
        validate(path)?;
        Ok(Self { path })
    }

    /// Returns the admitted path.
    #[must_use]
    pub const fn as_path(self) -> &'path Path {
        self.path
    }
}

/// An owned Unix endpoint whose path invariant is preserved by construction.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct UnixEndpointPath {
    path: PathBuf,
}

impl UnixEndpointPath {
    /// Admits an owned endpoint path.
    ///
    /// # Errors
    ///
    /// Returns an error when the encoded path is empty or too long.
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, UnixEndpointPathError> {
        let path = path.into();
        validate(&path)?;
        Ok(Self { path })
    }

    /// Borrows the admitted endpoint without repeating validation.
    #[must_use]
    pub fn as_ref(&self) -> UnixEndpointRef<'_> {
        UnixEndpointRef { path: &self.path }
    }

    /// Returns the admitted path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Consumes the capability and returns its owned path.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.path
    }
}

impl AsRef<Path> for UnixEndpointPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

/// Failure to admit a local Unix endpoint path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnixEndpointPathError {
    /// The endpoint path was empty.
    Empty,
    /// The endpoint exceeded the portable encoded byte budget.
    TooLong,
}

impl fmt::Display for UnixEndpointPathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Unix endpoint path is empty"),
            Self::TooLong => formatter.write_str("Unix endpoint path is too long"),
        }
    }
}

impl std::error::Error for UnixEndpointPathError {}

fn validate(path: &Path) -> Result<(), UnixEndpointPathError> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.is_empty() {
        Err(UnixEndpointPathError::Empty)
    } else if bytes.len() > MAX_UNIX_ENDPOINT_PATH_BYTES {
        Err(UnixEndpointPathError::TooLong)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_and_owned_endpoints_share_one_encoded_byte_invariant() {
        let boundary = PathBuf::from("x".repeat(MAX_UNIX_ENDPOINT_PATH_BYTES));
        let oversized = PathBuf::from("x".repeat(MAX_UNIX_ENDPOINT_PATH_BYTES + 1));

        assert!(UnixEndpointRef::new(&boundary).is_ok());
        assert!(UnixEndpointPath::new(boundary).is_ok());
        assert_eq!(
            UnixEndpointRef::new(Path::new("")),
            Err(UnixEndpointPathError::Empty)
        );
        assert_eq!(
            UnixEndpointPath::new(oversized),
            Err(UnixEndpointPathError::TooLong)
        );
    }
}
