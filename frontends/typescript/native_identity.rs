//! Build-time compiler source identity embedded in the packaged image.

/// SHA-256 of sorted relative compiler source, dependency, and declaration inputs.
/// Runtime authority never opens the source checkout or consults Git or Node.
pub const TSZ_NATIVE_AUTHORITY_IDENTITY: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/tsz-native-authority.sha256"));
