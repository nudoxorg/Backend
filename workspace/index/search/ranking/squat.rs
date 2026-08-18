//! Automatic typosquat / land-grab heuristics for package search.
//!
//! Pure and deterministic. False positives are preferred over false negatives
//! only when quality is already abysmal — a mature package is never flagged
//! solely for a short name.

/// Inputs the heuristic may consult (all optional fields allowed).
#[derive(Debug, Clone, Copy)]
pub struct SquatInput<'a> {
    pub name: &'a str,
    pub quality: f32,
    pub downloads: Option<u64>,
    pub release_count: Option<u32>,
    pub description: Option<&'a str>,
    pub has_repository: bool,
}

/// Whether this package looks like a name land-grab / squat.
///
/// Rules (any one is enough; all require low quality so mature packages pass):
/// 1. **Hollow shell** — quality < 0.20, no repo, empty/short description
/// 2. **Dead stub** — quality < 0.15, ≤1 release, downloads < 500 (or unknown)
/// 3. **Dictionary grab** — quality < 0.35, no repo, name is a short reserved
///    token (common land-grab targets: `yaml`, `http`, `json`, …)
pub fn is_squat_suspect(input: SquatInput<'_>) -> bool {
    if input.quality >= 0.45 {
        // Mature-enough packages are never auto-flagged.
        return false;
    }

    let desc = input.description.map_or("", str::trim);
    let hollow = input.quality < 0.20 && !input.has_repository && desc.len() < 20;

    let downloads = input.downloads.unwrap_or(0);
    let releases = input.release_count.unwrap_or(1);
    let dead_stub = input.quality < 0.15 && releases <= 1 && downloads < 500;

    let dict_grab =
        input.quality < 0.35 && !input.has_repository && is_reserved_landgrab_name(input.name);

    hollow || dead_stub || dict_grab
}

/// Short, high-value package-name targets commonly land-grabbed across
/// ecosystems. Lowercased exact match only (no substring).
fn is_reserved_landgrab_name(name: &str) -> bool {
    // Keep sorted — binary_search.
    const RESERVED: &[&str] = &[
        "angular", "api", "async", "await", "axios", "base", "cli", "config", "core", "crypto",
        "csv", "date", "db", "django", "express", "flask", "fs", "hash", "http", "https", "io",
        "json", "lodash", "log", "logs", "net", "numpy", "orm", "pandas", "rand", "random",
        "react", "requests", "serde", "sha", "sql", "test", "tests", "time", "tokio", "toml",
        "util", "utils", "uuid", "vue", "web", "xml", "yaml",
    ];
    let lower = name.to_ascii_lowercase();
    // Strip common eco affixes before the dictionary check.
    let stripped = lower
        .trim_start_matches("rust-")
        .trim_start_matches("cargo-")
        .trim_end_matches("-rs")
        .trim_start_matches("py")
        .trim_start_matches("python-")
        .trim_end_matches("-py");
    let candidate = if stripped.is_empty() {
        lower.as_str()
    } else {
        stripped
    };
    RESERVED.binary_search(&candidate).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(name: &str) -> SquatInput<'_> {
        SquatInput {
            name,
            quality: 0.1,
            downloads: Some(10),
            release_count: Some(1),
            description: Some("x"),
            has_repository: false,
        }
    }

    #[test]
    fn hollow_shell_flagged() {
        assert!(is_squat_suspect(base("my-empty-pkg")));
    }

    #[test]
    fn mature_package_not_flagged() {
        let mut s = base("serde");
        s.quality = 0.9;
        s.downloads = Some(50_000_000);
        s.release_count = Some(80);
        s.description = Some("A generic serialization/deserialization framework");
        s.has_repository = true;
        assert!(!is_squat_suspect(s));
    }

    #[test]
    fn dictionary_grab_yaml_low_quality() {
        let mut s = base("yaml");
        s.quality = 0.2;
        s.description = Some("yaml stuff here ok");
        assert!(is_squat_suspect(s));
    }

    #[test]
    fn dictionary_name_high_quality_with_repo_ok() {
        // Real PyYAML-class package should not trip when quality is solid.
        let s = SquatInput {
            name: "yaml",
            quality: 0.5,
            downloads: Some(1_000_000),
            release_count: Some(20),
            description: Some("YAML parser and emitter for Python"),
            has_repository: true,
        };
        assert!(!is_squat_suspect(s));
    }

    #[test]
    fn reserved_list_accepts_known_tokens() {
        assert!(is_reserved_landgrab_name("yaml"));
        assert!(is_reserved_landgrab_name("HTTP"));
        assert!(is_reserved_landgrab_name("rust-json"));
        assert!(!is_reserved_landgrab_name("serde-json"));
    }
}
