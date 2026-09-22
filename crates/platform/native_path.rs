//! Lossless native paths at persistence and process boundaries.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

/// Reversible operating-system path units used by durable state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "encoding", content = "units", rename_all = "snake_case")]
pub enum NativePathWire {
    /// Unix `OsStr` bytes.
    Unix(Vec<u8>),
    /// Windows UTF-16 code units.
    Windows(Vec<u16>),
}

/// Canonical bytes used to hash and compare one native path.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct NativePathKey(Vec<u8>);

impl NativePathKey {
    /// Returns the stable tagged native-unit encoding.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

/// A path that retains exact operating-system units.
#[derive(Clone, Debug)]
pub struct NativePath {
    path: PathBuf,
    key: NativePathKey,
}

impl NativePath {
    /// Admits a non-empty path without converting it through UTF-8.
    pub fn from_path(path: &Path) -> Result<Self, NativePathError> {
        let wire = wire_from_path(path)?;
        let key = key_from_wire(&wire);
        Ok(Self {
            path: path.to_path_buf(),
            key,
        })
    }

    /// Restores a native path only on the platform that owns its unit format.
    pub fn from_wire(wire: &NativePathWire) -> Result<Self, NativePathError> {
        let path = path_from_wire(wire)?;
        Self::from_path(&path)
    }

    /// Returns the exact native path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.path
    }

    /// Returns the stable tagged native-unit key.
    #[must_use]
    pub const fn key(&self) -> &NativePathKey {
        &self.key
    }

    /// Borrows a UTF-8 spelling when the native path has one.
    pub fn to_str(&self) -> Result<&str, NativePathError> {
        self.path.to_str().ok_or(NativePathError::Encoding)
    }

    /// Returns presentation text. This value must never be used as identity.
    #[must_use]
    pub fn display_lossy(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    /// Encodes the exact native units for durable persistence.
    pub fn to_wire(&self) -> Result<NativePathWire, NativePathError> {
        wire_from_path(&self.path)
    }
}

/// Native path admission failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativePathError {
    /// The path has no native units.
    Empty,
    /// A native NUL cannot cross filesystem or process boundaries.
    Nul,
    /// The path has no lossless UTF-8 spelling.
    Encoding,
    /// The persisted unit format belongs to another operating system.
    Platform,
}

impl fmt::Display for NativePathError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Empty => "native path is empty",
            Self::Nul => "native path contains a NUL unit",
            Self::Encoding => "native path is not valid UTF-8",
            Self::Platform => "native path encoding belongs to another platform",
        })
    }
}

impl std::error::Error for NativePathError {}

fn key_from_wire(wire: &NativePathWire) -> NativePathKey {
    match wire {
        NativePathWire::Unix(units) => {
            let mut key = Vec::with_capacity(units.len().saturating_add(1));
            key.push(0);
            key.extend_from_slice(units);
            NativePathKey(key)
        }
        NativePathWire::Windows(units) => {
            let mut key = Vec::with_capacity(units.len().saturating_mul(2).saturating_add(1));
            key.push(1);
            for unit in units {
                key.extend_from_slice(&unit.to_le_bytes());
            }
            NativePathKey(key)
        }
    }
}

#[cfg(unix)]
fn wire_from_path(path: &Path) -> Result<NativePathWire, NativePathError> {
    use std::os::unix::ffi::OsStrExt as _;

    let units = path.as_os_str().as_bytes();
    if units.is_empty() {
        return Err(NativePathError::Empty);
    }
    if units.contains(&0) {
        return Err(NativePathError::Nul);
    }
    Ok(NativePathWire::Unix(units.to_vec()))
}

#[cfg(windows)]
fn wire_from_path(path: &Path) -> Result<NativePathWire, NativePathError> {
    use std::os::windows::ffi::OsStrExt as _;

    let units = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if units.is_empty() {
        return Err(NativePathError::Empty);
    }
    if units.contains(&0) {
        return Err(NativePathError::Nul);
    }
    Ok(NativePathWire::Windows(units))
}

#[cfg(unix)]
fn path_from_wire(wire: &NativePathWire) -> Result<PathBuf, NativePathError> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt as _;

    let NativePathWire::Unix(units) = wire else {
        return Err(NativePathError::Platform);
    };
    if units.is_empty() {
        return Err(NativePathError::Empty);
    }
    if units.contains(&0) {
        return Err(NativePathError::Nul);
    }
    Ok(PathBuf::from(OsString::from_vec(units.clone())))
}

#[cfg(windows)]
fn path_from_wire(wire: &NativePathWire) -> Result<PathBuf, NativePathError> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt as _;

    let NativePathWire::Windows(units) = wire else {
        return Err(NativePathError::Platform);
    };
    if units.is_empty() {
        return Err(NativePathError::Empty);
    }
    if units.contains(&0) {
        return Err(NativePathError::Nul);
    }
    Ok(PathBuf::from(OsString::from_wide(units)))
}

#[cfg(test)]
mod tests {
    use super::{NativePath, NativePathWire};
    use std::path::{Path, PathBuf};

    #[test]
    fn utf8_path_round_trips_through_native_wire() {
        let native = NativePath::from_path(Path::new("/tmp/nudox path")).expect("native path");
        let wire = native.to_wire().expect("native wire");
        let restored = NativePath::from_wire(&wire).expect("wire round trip");
        assert_eq!(restored.as_path(), native.as_path());
        assert_eq!(restored.key(), native.key());
    }

    #[cfg(unix)]
    #[test]
    fn unix_non_utf8_bytes_round_trip_without_loss() {
        use std::ffi::OsString;
        use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

        let path = PathBuf::from(OsString::from_vec(b"/tmp/nudox-\xff".to_vec()));
        let native = NativePath::from_path(&path).expect("native path");
        assert!(native.to_str().is_err());
        let NativePathWire::Unix(units) = native.to_wire().expect("native wire") else {
            panic!("unix path changed wire family")
        };
        assert_eq!(units, path.as_os_str().as_bytes());
        assert_eq!(
            NativePath::from_wire(&NativePathWire::Unix(units))
                .expect("wire round trip")
                .as_path(),
            path
        );
    }
}
