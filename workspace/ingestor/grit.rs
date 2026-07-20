//! [`GritAdapter`]: the in-process [`GitRepository`] implementation backed by
//! the MIT-licensed `grit-lib` crate (gitbutlerapp/grit), adopted for its
//! in-process networking — no `git` subprocess per poll.
//!
//! # Transport coverage (and what is deliberately refused)
//!
//! - **Local repositories** (`file://` URLs or bare paths) are served by
//!   `grit_lib::ls_remote::ls_remote` directly against the on-disk ref store
//!   and object database — full `git ls-remote` fidelity including annotated
//!   tag peeling (`refs/tags/x^{}` companion entries) and `HEAD`.
//! - **Smart HTTP(S)** URLs are served by a protocol-v0 `info/refs?service=`
//!   discovery over grit's `ureq`-backed [`HttpClient`], parsed here with
//!   grit's `pkt_line` reader so that `HEAD` and peeled `^{}` advertisement
//!   lines are *preserved* (grit's own `read_advertisement` strips them, which
//!   would lose the peeled-oid pin for annotated tags). A dumb-HTTP plaintext
//!   `info/refs` body is accepted as a fallback.
//! - **`ext::` / `fd::`** pseudo-URLs name git's shell-executing helper
//!   transports. This adapter has no subprocess at all, and refuses them with a
//!   typed [`GitRepositoryError::UnsupportedTransport`] before any I/O — the
//!   same security posture the CLI adapter enforces via
//!   `protocol.ext.allow=never`.
//! - **Any other scheme** (`git://`, `ssh://`, scp-like) is refused with the
//!   same typed error: grit's daemon/ssh transports drop peeled `^{}` lines
//!   from the advertisement, so serving them here would silently degrade the
//!   annotated-tag oid pin. Callers needing those schemes fall back to
//!   [`crate::git::GitCommandAdapter`].
//!
//! Option-shaped "URLs" (e.g. `--upload-pack=…`) are structurally inert here:
//! there is no argv, so such a string can only ever be a (nonexistent) local
//! path, which fails with a typed error.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use grit_lib::objects::ObjectId;
use grit_lib::pkt_line;
use grit_lib::repo::Repository;
use grit_lib::transport::http::ureq_client::UreqHttpClient;
use grit_lib::transport::http::HttpClient;

use crate::git::{GitRepository, GitRepositoryError, LsRemoteRef};

/// How a feed URL is dispatched by [`GritAdapter`].
#[derive(Debug, PartialEq, Eq)]
enum RemoteUrlClass {
    /// A local repository path (`file://` URL or a bare filesystem path).
    LocalPath(PathBuf),
    /// A smart/dumb HTTP(S) remote.
    SmartHttp,
    /// A transport this adapter refuses (hostile helper transports and schemes
    /// grit cannot serve with full ls-remote fidelity).
    UnsupportedTransport {
        /// The scheme name, for the typed error (`ext`, `fd`, `git`, `ssh`, …).
        scheme: String,
    },
}

/// Classify `url` into a [`RemoteUrlClass`] without performing any I/O.
fn classify_remote_url(url: &str) -> RemoteUrlClass {
    // Hostile helper transports first: `ext::` runs an arbitrary command and
    // `fd::` reads inherited descriptors. Refuse before anything else looks at
    // the string.
    for hostile in ["ext::", "fd::"] {
        if url.starts_with(hostile) {
            return RemoteUrlClass::UnsupportedTransport {
                scheme: hostile.trim_end_matches(':').to_owned(),
            };
        }
    }
    if url.starts_with("http://") || url.starts_with("https://") {
        return RemoteUrlClass::SmartHttp;
    }
    if let Some(path) = url.strip_prefix("file://") {
        return RemoteUrlClass::LocalPath(PathBuf::from(path));
    }
    if let Some((scheme, _rest)) = url.split_once("://") {
        return RemoteUrlClass::UnsupportedTransport { scheme: scheme.to_owned() };
    }
    // No scheme: a plain local path. An option-shaped string lands here too and
    // simply fails to open as a repository — it can never become a flag.
    RemoteUrlClass::LocalPath(PathBuf::from(url))
}

/// The grit-backed adapter (the default [`GitRepository`] implementation; see
/// [`crate::DefaultGitAdapter`]).
pub struct GritAdapter {
    /// The blocking HTTP client used for smart-HTTP `info/refs` discovery.
    http_client: UreqHttpClient,
}

impl Default for GritAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GritAdapter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("GritAdapter").finish_non_exhaustive()
    }
}

impl GritAdapter {
    /// An adapter with a default (unauthenticated, proxy-env-aware) HTTP
    /// client.
    #[must_use]
    pub fn new() -> Self {
        Self { http_client: UreqHttpClient::new() }
    }

    /// Every ref the remote reports, with `HEAD` and peeled `^{}` entries
    /// preserved — the superset both trait methods filter from.
    fn all_remote_refs(&self, url: &str) -> Result<Vec<LsRemoteRef>, GitRepositoryError> {
        match classify_remote_url(url) {
            RemoteUrlClass::LocalPath(path) => local_ls_remote(url, &path),
            RemoteUrlClass::SmartHttp => self.http_ls_remote(url),
            RemoteUrlClass::UnsupportedTransport { scheme } => {
                Err(GitRepositoryError::UnsupportedTransport { url: url.to_owned(), scheme })
            }
        }
    }

    /// Smart-HTTP `info/refs?service=git-upload-pack` discovery (protocol v0),
    /// parsed with peeled/`HEAD` fidelity; falls back to a dumb-HTTP plaintext
    /// body.
    fn http_ls_remote(&self, url: &str) -> Result<Vec<LsRemoteRef>, GitRepositoryError> {
        let discovery_url =
            format!("{}/info/refs?service=git-upload-pack", url.trim_end_matches('/'));
        // No `Git-Protocol` header: request the classic v0 advertisement, which
        // carries HEAD and peeled `^{}` lines inline (v2 would require a second
        // ls-refs round-trip and grit-lib keeps that plumbing private).
        let body = self
            .http_client
            .get(&discovery_url, None)
            .map_err(|source| GitRepositoryError::Grit { url: url.to_owned(), source })?;
        parse_info_refs_body(url, &body)
    }
}

impl GitRepository for GritAdapter {
    fn ls_remote_bytes(&self, url: &str) -> Result<Vec<u8>, GitRepositoryError> {
        let refs = self.list_remote_refs(url)?;
        let mut bytes = Vec::new();
        for entry in &refs {
            bytes.extend_from_slice(entry.object_id.as_bytes());
            bytes.push(b'\t');
            bytes.extend_from_slice(entry.reference.as_bytes());
            bytes.push(b'\n');
        }
        Ok(bytes)
    }

    fn list_remote_refs(&self, url: &str) -> Result<Vec<LsRemoteRef>, GitRepositoryError> {
        // Parity with `git ls-remote --tags --heads`: branch and tag refs (and
        // the tags' peeled `^{}` companions), no HEAD.
        let refs = self.all_remote_refs(url)?;
        Ok(refs
            .into_iter()
            .filter(|entry| {
                entry.reference.starts_with("refs/heads/")
                    || entry.reference.starts_with("refs/tags/")
            })
            .collect())
    }

    fn head_object_id(&self, url: &str) -> Result<Option<String>, GitRepositoryError> {
        let refs = self.all_remote_refs(url)?;
        Ok(refs
            .into_iter()
            .find(|entry| entry.reference == "HEAD")
            .map(|entry| entry.object_id))
    }
}

/// List refs from a local repository via `grit_lib::ls_remote` (full fidelity:
/// `HEAD`, sorted refs, peeled annotated tags).
fn local_ls_remote(url: &str, path: &Path) -> Result<Vec<LsRemoteRef>, GitRepositoryError> {
    // A checkout keeps its git directory under `.git/`; a bare repository *is*
    // the git directory.
    let dot_git = path.join(".git");
    let git_dir = if dot_git.is_dir() { dot_git } else { path.to_path_buf() };
    let repository = Repository::open(&git_dir, None)
        .map_err(|source| GitRepositoryError::Grit { url: url.to_owned(), source })?;
    let options = grit_lib::ls_remote::Options::default();
    let entries = grit_lib::ls_remote::ls_remote(&repository.git_dir, &repository.odb, &options)
        .map_err(|source| GitRepositoryError::Grit { url: url.to_owned(), source })?;
    Ok(entries
        .into_iter()
        .map(|entry| LsRemoteRef { object_id: entry.oid.to_hex(), reference: entry.name })
        .collect())
}

/// Parse an HTTP `info/refs` response body — smart pkt-line framing when the
/// server sent it, plaintext `<oid>\t<ref>` lines otherwise — keeping `HEAD`
/// and peeled `^{}` entries.
fn parse_info_refs_body(url: &str, body: &[u8]) -> Result<Vec<LsRemoteRef>, GitRepositoryError> {
    // A smart response starts with a pkt-line whose payload is the
    // `# service=git-upload-pack` comment. Anything else is a dumb server's
    // plaintext ref list.
    if looks_like_smart_advertisement(body) {
        parse_smart_advertisement(url, body)
    } else {
        parse_dumb_ref_listing(url, body)
    }
}

/// Whether `body` begins with the smart-HTTP `# service=` announcement packet.
fn looks_like_smart_advertisement(body: &[u8]) -> bool {
    let mut cursor = Cursor::new(body);
    matches!(
        pkt_line::read_packet(&mut cursor),
        Ok(Some(pkt_line::Packet::Data(line))) if line.starts_with("# service=")
    )
}

/// Parse a smart v0 advertisement: skip the service packet and its flush, then
/// read `<oid> <ref>[\0capabilities]` lines until the closing flush.
fn parse_smart_advertisement(
    url: &str,
    body: &[u8],
) -> Result<Vec<LsRemoteRef>, GitRepositoryError> {
    let malformed = |detail: String| GitRepositoryError::MalformedOutput {
        url: url.to_owned(),
        detail,
    };
    let mut cursor = Cursor::new(body);
    // The `# service=…` packet (validated by the caller) and its flush.
    let _service = pkt_line::read_packet(&mut cursor)
        .map_err(|error| malformed(format!("unreadable service packet: {error}")))?;

    let mut refs = Vec::new();
    loop {
        let packet = pkt_line::read_packet(&mut cursor)
            .map_err(|error| malformed(format!("unreadable advertisement packet: {error}")))?;
        match packet {
            // The flush after `# service=` arrives first; the flush after the
            // ref list (or EOF) ends the walk. An empty repository advertises
            // only the `capabilities^{}` carrier, yielding zero refs.
            None => break,
            Some(pkt_line::Packet::Flush)
            | Some(pkt_line::Packet::Delim)
            | Some(pkt_line::Packet::ResponseEnd) => {
                if refs.is_empty() {
                    continue;
                }
                break;
            }
            Some(pkt_line::Packet::Data(line)) => {
                let line = line.trim_end_matches('\n');
                if let Some(message) = line.strip_prefix("ERR ") {
                    return Err(GitRepositoryError::Grit {
                        url: url.to_owned(),
                        source: grit_lib::error::Error::Message(format!(
                            "remote error: {}",
                            message.trim_end()
                        )),
                    });
                }
                // `version N` preamble lines and other non-ref chatter are
                // skipped; ref lines must lead with a full hex object id.
                if let Some(entry) = parse_advertised_ref_line(line) {
                    refs.push(entry);
                }
            }
        }
    }
    Ok(refs)
}

/// Parse one v0 advertisement payload: `<oid-hex> <refname>[\0<capabilities>]`.
///
/// Returns `None` for non-ref lines (`version 1`, `shallow <oid>`, the
/// `capabilities^{}` no-refs carrier). Peeled `^{}` and `HEAD` entries are
/// returned like any other ref.
fn parse_advertised_ref_line(line: &str) -> Option<LsRemoteRef> {
    let payload = line.split('\0').next().unwrap_or(line);
    let (oid_hex, reference) = payload.split_once([' ', '\t'])?;
    let object_id = ObjectId::from_hex(oid_hex).ok()?;
    let reference = reference.trim();
    if reference.is_empty() || reference == "capabilities^{}" {
        return None;
    }
    Some(LsRemoteRef { object_id: object_id.to_hex(), reference: reference.to_owned() })
}

/// Parse a dumb-HTTP `info/refs` body: plaintext `<oid>\t<ref>` lines.
fn parse_dumb_ref_listing(url: &str, body: &[u8]) -> Result<Vec<LsRemoteRef>, GitRepositoryError> {
    let text = std::str::from_utf8(body).map_err(|_| GitRepositoryError::MalformedOutput {
        url: url.to_owned(),
        detail: "info/refs body was neither pkt-line framed nor valid UTF-8".to_owned(),
    })?;
    let mut refs = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let Some((oid_hex, reference)) = line.split_once('\t') else {
            return Err(GitRepositoryError::MalformedOutput {
                url: url.to_owned(),
                detail: format!("dumb info/refs line without a tab separator: {line:?}"),
            });
        };
        let object_id =
            ObjectId::from_hex(oid_hex.trim()).map_err(|_| GitRepositoryError::MalformedOutput {
                url: url.to_owned(),
                detail: format!("dumb info/refs line with a non-hex object id: {line:?}"),
            })?;
        refs.push(LsRemoteRef {
            object_id: object_id.to_hex(),
            reference: reference.trim().to_owned(),
        });
    }
    Ok(refs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_helper_transports_are_refused() {
        for url in ["ext::sh -c date", "fd::17"] {
            assert!(matches!(
                classify_remote_url(url),
                RemoteUrlClass::UnsupportedTransport { .. }
            ));
        }
    }

    #[test]
    fn http_and_file_urls_classify() {
        assert_eq!(classify_remote_url("https://example.test/r.git"), RemoteUrlClass::SmartHttp);
        assert_eq!(
            classify_remote_url("file:///tmp/repo"),
            RemoteUrlClass::LocalPath(PathBuf::from("/tmp/repo"))
        );
    }

    #[test]
    fn option_shaped_url_is_a_local_path_not_a_flag() {
        assert_eq!(
            classify_remote_url("--upload-pack=touch /tmp/pwn"),
            RemoteUrlClass::LocalPath(PathBuf::from("--upload-pack=touch /tmp/pwn"))
        );
    }

    #[test]
    fn unsupported_schemes_are_typed() {
        assert_eq!(
            classify_remote_url("git://example.test/r.git"),
            RemoteUrlClass::UnsupportedTransport { scheme: "git".to_owned() }
        );
        assert_eq!(
            classify_remote_url("ssh://example.test/r.git"),
            RemoteUrlClass::UnsupportedTransport { scheme: "ssh".to_owned() }
        );
    }

    #[test]
    fn smart_advertisement_preserves_head_and_peeled() {
        let oid_a = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
        let oid_b = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
        let mut body = Vec::new();
        pkt_line::write_line_to_vec(&mut body, "# service=git-upload-pack\n").unwrap();
        pkt_line::write_flush(&mut body).unwrap();
        pkt_line::write_line_to_vec(&mut body, &format!("{oid_a} HEAD\0multi_ack symref=HEAD:refs/heads/main\n")).unwrap();
        pkt_line::write_line_to_vec(&mut body, &format!("{oid_a} refs/heads/main\n")).unwrap();
        pkt_line::write_line_to_vec(&mut body, &format!("{oid_a} refs/tags/v1.0.0\n")).unwrap();
        pkt_line::write_line_to_vec(&mut body, &format!("{oid_b} refs/tags/v1.0.0^{{}}\n")).unwrap();
        pkt_line::write_flush(&mut body).unwrap();

        let refs = parse_info_refs_body("u", &body).expect("smart body parses");
        let names: Vec<&str> = refs.iter().map(|r| r.reference.as_str()).collect();
        assert_eq!(names, ["HEAD", "refs/heads/main", "refs/tags/v1.0.0", "refs/tags/v1.0.0^{}"]);
        assert_eq!(refs[3].object_id, oid_b);
    }

    #[test]
    fn dumb_body_parses_plaintext_lines() {
        let body = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\trefs/heads/main\n";
        let refs = parse_info_refs_body("u", body).expect("dumb body parses");
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].reference, "refs/heads/main");
    }
}
