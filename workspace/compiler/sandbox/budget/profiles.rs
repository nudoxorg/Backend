//! Per-producer limit presets (design §7).
//!
//! Centralised so ops can tune one table; producers pick a profile by name.
//!
//! `mem_bytes` is the **cgroup RAM** ceiling. The supervisor applies
//! `RLIMIT_AS` at **4×** this value (VA headroom for rustc/LLVM); do not treat
//! `mem_bytes` as a virtual-address cap.

use crate::budget::limits::Limits;

/// Named ceiling sets for each producer class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ProducerProfile {
    /// rust-analyzer load: cargo metadata + build scripts + proc-macro srv — HIGH.
    Rust,
    /// `javac` + `javadoc` doclet — HIGH; annotation processors.
    Java,
    /// `go run` oracle — MEDIUM; cgo possible.
    Go,
    /// snix-eval worker — HIGH-bounded interpreter.
    Nix,
    /// `dotnet` Roslyn oracle — HIGH; large-package compilations are hungry.
    /// Source mode runs no user code (Roslyn only parses), but analyzers /
    /// source-generators in packages are a code-exec vector — treat like Java.
    CSharp,
    /// deno_doc / pyrefly workers — LOW static parsers.
    StaticParser,
    /// Escape tests / trivial commands.
    Tiny,
}

impl ProducerProfile {
    /// Limits for this profile (design §7 tables).
    pub const fn limits(self) -> Limits {
        self.limits_with(crate::budget::limits::LimitOverride::none())
    }

    /// Profile ceilings with a sparse per-package/operator overlay.
    pub const fn limits_with(self, overlay: crate::budget::limits::LimitOverride) -> Limits {
        // const fn cannot call overlay.apply (non-const); expand inline.
        let base = self.base_limits();
        Limits {
            mem_bytes: match overlay.mem_bytes {
                Some(v) => v,
                None => base.mem_bytes,
            },
            cpu_secs: match overlay.cpu_secs {
                Some(v) => v,
                None => base.cpu_secs,
            },
            wall: match overlay.wall {
                Some(v) => v,
                None => base.wall,
            },
            pids: match overlay.pids {
                Some(v) => v,
                None => base.pids,
            },
            max_stdout: match overlay.max_stdout {
                Some(v) => v,
                None => base.max_stdout,
            },
            max_stderr: match overlay.max_stderr {
                Some(v) => v,
                None => base.max_stderr,
            },
            fsize_bytes: match overlay.fsize_bytes {
                Some(v) => v,
                None => base.fsize_bytes,
            },
            nofile: match overlay.nofile {
                Some(v) => v,
                None => base.nofile,
            },
        }
    }

    pub(crate) const fn base_limits(self) -> Limits {
        match self {
            // mem 6 GiB, wall 15 min, cpu 900s, pids 512
            // RA resident sets on medium workspaces regularly exceed 3 GiB.
            Self::Rust => Limits::from_const(
                6 * 1024 * 1024 * 1024,
                900,
                15 * 60,
                512,
                16 * 1024 * 1024, // leftover headroom; no longer rustdoc JSON
                256 * 1024,
                2 * 1024 * 1024 * 1024,
                4096,
            ),
            // mem 2 GiB, wall 10 min, pids 256
            Self::Java => Limits::from_const(
                2 * 1024 * 1024 * 1024,
                600,
                10 * 60,
                256,
                8 * 1024 * 1024,
                256 * 1024,
                1024 * 1024 * 1024,
                2048,
            ),
            // mem 4 GiB, wall 10 min, cpu 600s, pids 256
            // Roslyn compilations of large packages exceed javadoc's 2 GiB.
            Self::CSharp => Limits::from_const(
                4 * 1024 * 1024 * 1024,
                600,
                10 * 60,
                256,
                8 * 1024 * 1024,
                256 * 1024,
                2 * 1024 * 1024 * 1024,
                2048,
            ),
            // mem 2 GiB, wall 5 min
            Self::Go => Limits::from_const(
                2 * 1024 * 1024 * 1024,
                300,
                5 * 60,
                256,
                8 * 1024 * 1024,
                256 * 1024,
                1024 * 1024 * 1024,
                2048,
            ),
            // mem 1 GiB, wall 2 min (cgroup replaces cooperative 120s budget)
            Self::Nix => Limits::from_const(
                1024 * 1024 * 1024,
                120,
                2 * 60,
                64,
                4 * 1024 * 1024,
                100 * 1024,
                512 * 1024 * 1024,
                1024,
            ),
            // mem 1 GiB, wall 2 min
            Self::StaticParser => Limits::from_const(
                1024 * 1024 * 1024,
                120,
                2 * 60,
                32,
                4 * 1024 * 1024,
                100 * 1024,
                512 * 1024 * 1024,
                1024,
            ),
            Self::Tiny => Limits::from_const(
                64 * 1024 * 1024,
                5,
                10,
                16,
                64 * 1024,
                64 * 1024,
                16 * 1024 * 1024,
                256,
            ),
        }
    }
}

impl ProducerProfile {
    /// Runtime (non-const) overlay — preferred when `LimitOverride` is dynamic.
    pub fn with_override(self, overlay: crate::budget::limits::LimitOverride) -> Limits {
        overlay.apply(self.base_limits())
    }

    /// Config / metrics wire name.
    pub const fn wire_name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Java => "java",
            Self::Go => "go",
            Self::CSharp => "csharp",
            Self::Nix => "nix",
            Self::StaticParser => "static_parser",
            Self::Tiny => "tiny",
        }
    }
}
