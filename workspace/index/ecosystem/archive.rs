//! Archive framing per ecosystem — extends the registry's tar-only
//! `ArchiveFormat` with `Zip` (nupkg, Go module zips, jars are all zips).

/// The compression framing of an ecosystem's source archives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ArchiveKind {
    /// A `.tar.gz` (crates.io, npm tarballs, PyPI sdists).
    TarGz,
    /// A `.tar.zst`.
    TarZst,
    /// A bare uncompressed `.tar`.
    Tar,
    /// A zip container (`.nupkg`, goproxy module zips, `.jar`).
    Zip,
}
