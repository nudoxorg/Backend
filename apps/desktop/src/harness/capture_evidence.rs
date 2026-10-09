//! Capture bytes and committed frames; source admission belongs to the producer receipt.

use serde::Serialize;
use sha2::{Digest as _, Sha256};
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

/// The platform-verified main executable file, read once for a capture session.
#[derive(Debug, Serialize)]
pub struct CaptureImage {
    /// The launch path reported by `current_exe`, not a source checkout identity.
    pub current_exe_path: PathBuf,
    /// SHA-256 of the held, platform-verified main executable file.
    pub sha256: String,
    /// Number of executable bytes hashed.
    pub bytes: u64,
}

impl CaptureImage {
    /// Reads the running image without adopting a replacement at its launch path.
    /// The anchor must be compiled into the main executable.
    ///
    /// # Errors
    /// Refuses an unavailable, replaced or changing image.
    pub fn read(main_image_anchor: fn()) -> io::Result<Self> {
        let current_exe_path = std::env::current_exe()?;
        let mut executable =
            backend_platform::executable_identity::open_running_executable(main_image_anchor)?;
        let (sha256, bytes) = executable.with_verified_read(|file| {
            let mut hash = Sha256::new();
            let mut bytes = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
                bytes += count as u64;
            }
            Ok((format!("{:x}", hash.finalize()), bytes))
        })?;
        Ok(Self {
            current_exe_path,
            sha256,
            bytes,
        })
    }
}

/// Labels never establish a source-to-image relationship, even when they agree.
/// An external producer audit must bind its actual compiler output to this image.
#[must_use]
pub fn source_labels(requested: Option<&str>, embedded: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "requested_source_label": requested,
        "embedded_source_label": embedded,
        "source_binding": {
            "state": "Unestablished",
            "reason": "capture labels and executable identity do not authenticate a producing source; an independently verified build receipt is required",
        },
    })
}

/// Exact saved bytes, using the GUI harness artifact hash semantics.
#[derive(Debug, Serialize)]
pub struct SavedArtifact {
    /// File path relative to the capture directory.
    pub path: String,
    /// SHA-256 of the encoded file bytes, not screenshot pixels.
    pub sha256: String,
}

impl SavedArtifact {
    /// Writes and identifies the same byte buffer.
    ///
    /// # Errors
    /// Returns a filesystem error without claiming a saved artifact.
    pub fn write(out: &Path, relative: &str, bytes: &[u8]) -> io::Result<Self> {
        std::fs::write(out.join(relative), bytes)?;
        Ok(Self {
            path: relative.to_owned(),
            sha256: backend_gui_harness::hash_bytes(bytes),
        })
    }

    /// Saves an encoded PNG and records its byte digest.
    ///
    /// # Errors
    /// Returns an encoding or filesystem error without claiming a saved PNG.
    pub fn png(out: &Path, relative: &str, image: &image::RgbaImage) -> Result<Self, String> {
        use image::ImageEncoder as _;
        let mut bytes = Vec::new();
        image::codecs::png::PngEncoder::new(&mut bytes)
            .write_image(
                image.as_raw(),
                image.width(),
                image.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|error| error.to_string())?;
        Self::write(out, relative, &bytes).map_err(|error| error.to_string())
    }
}

/// Two consecutive quiet commits, never two observations of the same tree.
pub struct QuietCommits {
    previous: u64,
    consecutive: u8,
}

impl QuietCommits {
    /// Starts after the last committed tree observed before settlement.
    #[must_use]
    pub const fn after(previous: u64) -> Self {
        Self {
            previous,
            consecutive: 0,
        }
    }

    /// Admits one corresponding tree, requiring strict committed-frame advancement.
    ///
    /// # Errors
    /// Rejects a missing, reused, regressed or mismatched native tree.
    pub fn observe(
        &mut self,
        frame: u64,
        tree: &serde_json::Value,
        quiet: bool,
    ) -> Result<bool, String> {
        if frame == 0 || frame <= self.previous {
            return Err(format!(
                "native tree frame {frame} did not advance after {}",
                self.previous
            ));
        }
        if tree["frame"]["frame_number"].as_u64() != Some(frame) {
            return Err("native tree and committed frame number disagree".to_owned());
        }
        self.previous = frame;
        self.consecutive = if quiet {
            self.consecutive.saturating_add(1).min(2)
        } else {
            0
        };
        Ok(self.consecutive == 2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(frame: u64) -> serde_json::Value {
        serde_json::json!({"frame": {"frame_number": frame}})
    }

    #[test]
    fn quiet_samples_require_distinct_commits_and_reset_on_motion() -> Result<(), String> {
        let mut commits = QuietCommits::after(10);
        assert!(!commits.observe(11, &tree(11), true)?);
        assert!(commits.observe(11, &tree(11), true).is_err());
        assert!(commits.observe(9, &tree(9), true).is_err());
        assert!(commits.observe(0, &tree(0), true).is_err());
        assert!(commits.observe(12, &tree(11), true).is_err());
        assert!(!commits.observe(12, &tree(12), false)?);
        assert!(!commits.observe(13, &tree(13), true)?);
        assert!(commits.observe(14, &tree(14), true)?);
        Ok(())
    }

    #[test]
    fn matching_or_absent_source_labels_never_attest_an_image() {
        for (requested, embedded) in [
            (None, None),
            (Some("same-head"), Some("same-head")),
            (Some("current"), Some("old")),
        ] {
            let labels = source_labels(requested, embedded);
            assert_eq!(labels["requested_source_label"].as_str(), requested);
            assert_eq!(labels["embedded_source_label"].as_str(), embedded);
            assert_eq!(labels["source_binding"]["state"], "Unestablished");
        }
    }

    #[cfg(any(target_os = "linux", target_os = "macos", windows))]
    #[test]
    fn capture_image_hashes_the_actual_platform_verified_main_file()
    -> Result<(), Box<dyn std::error::Error>> {
        fn anchor() {
            std::hint::black_box("capture image native law");
        }
        let image = CaptureImage::read(anchor)?;
        let mut executable =
            backend_platform::executable_identity::open_running_executable(anchor)?;
        let expected = executable.with_verified_read(|file| {
            let mut hash = Sha256::new();
            let mut length = 0_u64;
            let mut buffer = [0_u8; 8192];
            loop {
                let count = file.read(&mut buffer)?;
                if count == 0 {
                    break;
                }
                hash.update(&buffer[..count]);
                length += count as u64;
            }
            Ok((format!("{:x}", hash.finalize()), length))
        })?;
        assert_eq!((image.sha256, image.bytes), expected);
        assert_eq!(image.current_exe_path, std::env::current_exe()?);
        Ok(())
    }
}
