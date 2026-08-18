//! [`GitRepository`]: the thin, swappable git adapter (REGISTRYLESS-PLAN §15
//! risk table — "ls-remote polling scale" / adapter behind a trait).
//!
//! # Adapter lineup
//!
//! Two implementations exist behind the trait:
//!
//! - [`crate::ingest::grit::GritAdapter`] — the **default** (see
//!   [`crate::ingest::DefaultGitAdapter`]): in-process ls-remote via the `grit-lib`
//!   crate, adopted for its better networking (no subprocess per poll).
//! - [`GitCommandAdapter`] (this module) — the `git` CLI subprocess
//!   **fallback**, kept compiled for transports grit does not serve (`git://`,
//!   `ssh://`) and as the subject of the subprocess-hardening regression tests.
//!
//! # The subprocess adapter's safety rules, enforced here:
//!
//! - **Arg vectors only.** Every argument is a separate `Command::arg`; the URL
//!   and refspecs are never concatenated into a shell string, so there is no
//!   shell-interpolation surface even for a hostile repository URL.
//! - **No config inheritance surprises.** `ls-remote` runs with
//!   `GIT_TERMINAL_PROMPT=0` so a repo requiring auth fails fast instead of
//!   blocking on a credential prompt, and with `GIT_CONFIG_GLOBAL` /
//!   `GIT_CONFIG_SYSTEM` pointed at `/dev/null` so an attacker-controlled global
//!   or system git config on the host cannot re-enable a dangerous transport.
//! - **Hostile-transport denylist.** Even with the URL passed positionally, a
//!   remote URL like `ext::sh -c …` names git's `ext` helper transport, which
//!   *runs an arbitrary shell command*. git's own default (`protocol.ext.allow`
//!   = `user`) is version- and environment-dependent (a `GIT_PROTOCOL_FROM_USER`
//!   context, or an inherited `protocol.ext.allow=always`, can re-enable it).
//!   We therefore do not trust the default: every invocation force-pins the
//!   shell-executing helper transports (`ext`, `fd`) to `never` via `-c`, so a
//!   hostile feed URL can never reach a code-execution transport regardless of
//!   host config or git version (see [`HARDENING_CONFIG`]).
//! - **Typed errors.** Nonexistent repo, malformed output, spawn failure, and
//!   non-zero exit each map to a distinct [`Error`] variant; the
//!   caller never sees a panic.
//!
//! The trait is intentionally minimal ([`GitRepository::list_remote_refs`] plus
//! [`GitRepository::head_object_id`]); the grit swap was a drop-in because
//! callers depend on the trait, not the subprocess.

use std::process::Command;

/// Config overrides force-applied (via `-c`) to every `git` invocation so a
/// hostile remote URL cannot select a code-execution transport, independent of
/// the host's git version or inherited config.
///
/// - `protocol.ext.allow=never` — the `ext::` transport runs an arbitrary shell
///   command; always refuse it.
/// - `protocol.fd.allow=never` — the `fd::` transport talks to inherited file
///   descriptors; never meaningful for a remote feed URL, so refuse it too.
///
/// These are passed *before* the subcommand (git only honors `-c` there) and
/// cannot be overridden by the repository URL, which is a positional argument.
const HARDENING_CONFIG: &[&str] = &["protocol.ext.allow=never", "protocol.fd.allow=never"];

/// One raw ref line from `git ls-remote`: an object id and its full ref name.
///
/// The [`crate::ingest::enumerate`] path hands the reassembled `ls-remote` bytes to the
/// ecosystem's pure listing parser, which does the tag/peel logic; this type is
/// only the transport-level pairing so the monitor can diff refs cheaply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LsRemoteRef {
    /// The 40-hex object id the ref points at.
    pub object_id: String,
    /// The full ref name, e.g. `refs/tags/v1.2.3` or `HEAD`.
    pub reference: String,
}

/// Why a git adapter operation failed.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The `git` binary could not be spawned at all.
    #[error("failed to spawn git for {url}: {message}")]
    Spawn {
        /// The repository URL the command targeted.
        url: String,
        /// The spawn error message.
        message: String,
    },
    /// `git` ran but exited non-zero — includes the nonexistent-repo case, which
    /// git reports as a fatal remote error.
    #[error("git exited with status {status} for {url}: {stderr}")]
    CommandFailed {
        /// The repository URL.
        url: String,
        /// The process exit code (or `-1` when terminated by signal).
        status: i32,
        /// The captured stderr, trimmed.
        stderr: String,
    },
    /// `git` produced output that was not valid UTF-8 or not the expected
    /// `<oid>\t<ref>` line shape.
    #[error("git produced malformed ls-remote output for {url}: {detail}")]
    MalformedOutput {
        /// The repository URL.
        url: String,
        /// What was wrong with the output.
        detail: String,
    },
    /// The URL names a transport the adapter refuses: the shell-executing
    /// helper transports (`ext::`, `fd::`) are refused for safety by every
    /// adapter, and [`crate::ingest::grit::GritAdapter`] additionally refuses schemes
    /// it cannot serve with full ls-remote fidelity (`git://`, `ssh://`).
    #[error("the {scheme} transport is refused for {url}")]
    UnsupportedTransport {
        /// The repository URL.
        url: String,
        /// The refused scheme name (`ext`, `fd`, `git`, `ssh`, …).
        scheme: String,
    },
    /// The in-process grit implementation reported a repository, transport, or
    /// protocol failure (typed passthrough of `grit_lib`'s error).
    #[error("grit failed for {url}")]
    Grit {
        /// The repository URL.
        url: String,
        /// The underlying grit error.
        #[source]
        source: grit_lib::error::Error,
    },
}

/// Backwards-compatible alias: the git adapter error (now [`Error`]).
pub use self::Error as GitRepositoryError;

/// A thin, swappable git remote adapter (REGISTRYLESS-PLAN §7.4 / §15).
pub trait GitRepository: Send + Sync {
    /// The raw `git ls-remote --tags --heads <url>` bytes, for the ecosystem
    /// listing parser (REGISTRYLESS-PLAN §7.4 step 2 / RL-14). Kept as bytes so
    /// the pure parser owns all UTF-8 / tab / peel handling.
    fn ls_remote_bytes(&self, url: &str) -> Result<Vec<u8>, Error>;

    /// The parsed ref list from `git ls-remote --tags --heads <url>`, for the
    /// git monitor's ref diff.
    fn list_remote_refs(&self, url: &str) -> Result<Vec<LsRemoteRef>, Error>;

    /// The object id `HEAD` currently points at, used for the untagged
    /// pseudo-version path (REGISTRYLESS-PLAN §7.4 step 2, §3.3).
    fn head_object_id(&self, url: &str) -> Result<Option<String>, Error>;
}

/// The `git` CLI subprocess adapter (the shipped implementation).
#[derive(Debug, Clone)]
pub struct GitCommandAdapter {
    /// The `git` executable to invoke (`"git"` by default; overridable for a
    /// pinned nix-store path or a test double).
    git_binary: String,
}

impl Default for GitCommandAdapter {
    fn default() -> Self {
        Self {
            git_binary: "git".to_owned(),
        }
    }
}

impl GitCommandAdapter {
    /// An adapter invoking the given `git` executable.
    pub fn new(git_binary: impl Into<String>) -> Self {
        Self {
            git_binary: git_binary.into(),
        }
    }

    /// Run `git ls-remote <flags> -- <url> <patterns...>` and return stdout
    /// bytes, mapping every failure mode to a typed error. Arg-vector only — no
    /// shell.
    ///
    /// `flags` are option arguments (e.g. `--tags`) placed *before* the `--`
    /// separator; `patterns` are ref globs (e.g. `HEAD`) placed *after* the URL,
    /// which is where git's positional refspec arguments belong.
    fn run_ls_remote(
        &self,
        url: &str,
        flags: &[&str],
        patterns: &[&str],
    ) -> Result<Vec<u8>, Error> {
        let mut command = Command::new(&self.git_binary);
        // Hardening `-c` overrides must precede the subcommand — git only honors
        // top-level `-c` there. They pin the shell-executing helper transports to
        // `never` so a hostile `ext::`/`fd::` URL is refused even if the host's
        // global/system config would otherwise allow them.
        for config in HARDENING_CONFIG {
            command.arg("-c").arg(config);
        }
        command.arg("ls-remote");
        for flag in flags {
            command.arg(flag);
        }
        // The URL is a single, un-split argument — never interpolated into a
        // shell string — so a URL like `--upload-pack=…` cannot inject a flag
        // (it would be treated as the positional repository, which git rejects).
        command.arg("--");
        command.arg(url);
        // Ref patterns are positional and must follow the URL.
        for pattern in patterns {
            command.arg(pattern);
        }
        // Fail fast instead of blocking on an interactive credential prompt.
        command.env("GIT_TERMINAL_PROMPT", "0");
        // Neutralize attacker-controllable global/system git config on the host:
        // without this a `protocol.ext.allow=always` line in a global config
        // could re-enable a transport our `-c` overrides above already deny, but
        // isolating the config files removes that whole surface. `/dev/null`
        // parses as an empty config on every platform we target.
        command.env("GIT_CONFIG_GLOBAL", "/dev/null");
        command.env("GIT_CONFIG_SYSTEM", "/dev/null");

        let output = command.output().map_err(|error| Error::Spawn {
            url: url.to_owned(),
            message: error.to_string(),
        })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(Error::CommandFailed {
                url: url.to_owned(),
                status: output.status.code().unwrap_or(-1),
                stderr,
            });
        }
        Ok(output.stdout)
    }
}

/// Parse raw `ls-remote` bytes into `<oid, ref>` pairs, rejecting malformed
/// lines with a typed error (the monitor needs a clean ref set to diff).
fn parse_ref_lines(url: &str, bytes: &[u8]) -> Result<Vec<LsRemoteRef>, Error> {
    let text = std::str::from_utf8(bytes).map_err(|_| Error::MalformedOutput {
        url: url.to_owned(),
        detail: "output was not valid UTF-8".to_owned(),
    })?;

    let mut refs = Vec::new();
    for raw_line in text.split('\n') {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let Some((object_id, reference)) = line.split_once('\t') else {
            return Err(Error::MalformedOutput {
                url: url.to_owned(),
                detail: format!("line without a tab separator: {line:?}"),
            });
        };
        let object_id = object_id.trim();
        let reference = reference.trim();
        if object_id.is_empty() || reference.is_empty() {
            return Err(Error::MalformedOutput {
                url: url.to_owned(),
                detail: format!("line with an empty field: {line:?}"),
            });
        }
        refs.push(LsRemoteRef {
            object_id: object_id.to_owned(),
            reference: reference.to_owned(),
        });
    }
    Ok(refs)
}

impl GitRepository for GitCommandAdapter {
    fn ls_remote_bytes(&self, url: &str) -> Result<Vec<u8>, Error> {
        self.run_ls_remote(url, &["--tags", "--heads"], &[])
    }

    fn list_remote_refs(&self, url: &str) -> Result<Vec<LsRemoteRef>, Error> {
        let bytes = self.run_ls_remote(url, &["--tags", "--heads"], &[])?;
        parse_ref_lines(url, &bytes)
    }

    fn head_object_id(&self, url: &str) -> Result<Option<String>, Error> {
        // `ls-remote <url> HEAD` prints the oid the remote's HEAD points at.
        let bytes = self.run_ls_remote(url, &[], &["HEAD"])?;
        let refs = parse_ref_lines(url, &bytes)?;
        Ok(refs
            .into_iter()
            .find(|entry| entry.reference == "HEAD")
            .map(|entry| entry.object_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_well_formed_ref_lines() {
        let bytes = b"deadbeef\trefs/tags/v1.0.0\ncafebabe\trefs/heads/main\n";
        let refs = parse_ref_lines("u", bytes).expect("well-formed");
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[0].reference, "refs/tags/v1.0.0");
    }

    #[test]
    fn rejects_line_without_tab() {
        let error = parse_ref_lines("u", b"deadbeef refs/tags/v1.0.0\n").unwrap_err();
        assert!(matches!(error, Error::MalformedOutput { .. }));
    }

    #[test]
    fn rejects_non_utf8() {
        let error = parse_ref_lines("u", &[0xff, 0xfe]).unwrap_err();
        assert!(matches!(error, Error::MalformedOutput { .. }));
    }

    #[test]
    fn empty_output_is_no_refs() {
        assert!(parse_ref_lines("u", b"").expect("empty ok").is_empty());
    }
}
