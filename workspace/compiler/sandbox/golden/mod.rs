//! `heart::sync::ContentIo` for the smolvm golden store (§8d, V-GOLD-2).
//!
//! A golden VM (a warm, forkable toolchain-base machine — see
//! [`crate::backend::smolvm::SmolvmRuntime::prepare_golden`]) is a
//! **content-addressed item**: its persisted state — the qcow2 base disk plus
//! the memfd RAM checkpoint / device manifests that live under its smolvm data
//! directory — can be packed into transferable bytes, shipped to a peer over the
//! same verifying transport the IR VCS and object-pack planes use, and unpacked
//! there to replicate the golden so a subsequent `fork_golden` CoW-restores from
//! it. This is the identical seam
//! `ir-vcs::sync::FsChangeIo` and `index::pack::sync::ObjectPackContentIo`
//! implement for their planes; the transport that moves the bytes is a caller
//! concern (the `transport` crate), never wired here — `heart` stays iroh-free.
//!
//! # Live-golden checkpoint in `read` (V-GOLD-2 gap closed)
//!
//! When the golden is currently running forkable (its control socket is live),
//! `read` drives a **fresh RAM checkpoint** before packing by sending the smolvm
//! `FORK <dir>` command directly to the golden's control socket. This uses the
//! same `smolvm::agent::fork::{control_socket_path, control_socket_cmd}` pair
//! that `prepare_fork` uses internally, but without registering a clone in the DB
//! or CoW-cloning disks — we only need the memfd snapshot write half.
//!
//! The snapshot is written to `<golden_data_dir>/fork-snapshots/CURRENT/`, a
//! well-known sentinel name inside the golden's own data dir. The Landlock
//! confinement that restricts the frozen golden VMM to its own data dir is
//! satisfied because the snapshot path is a subdirectory of `vm_data_dir(golden)`.
//! A prior stale `CURRENT/` is removed before the FORK command so the snapshot
//! is always fresh.
//!
//! If the control socket does not exist (golden not running, or this node is only
//! a receiver of a replicated golden), `read` falls back to packing whatever
//! snapshot state is already on disk — the pre-gap behaviour.
//!
//! The `FORK` command pauses the golden and writes its checkpoint; on success the
//! golden stays paused (frozen) as the shared CoW base, exactly as after any
//! `prepare_fork` call. Subsequent `fork_golden` calls still work: the golden's
//! control socket continues to answer `STATUS` and `FORK` while frozen.
//!
//! # The content-address unit
//!
//! `type Id = ImageDigest` — the toolchain-image content hash that keys a golden
//! in the [`GoldenPool`]. A golden is registered under the digest of the OCI
//! image it was warmed from; the same image on desktop and fleet ⇒ same digest ⇒
//! **one shareable golden** (GD-18 parity). The packed item is the golden's
//! `snapshot_dir` — the on-disk `smolvm::agent::vm_data_dir(golden_name)` tree
//! (qcow2 base + any `fork-snapshots/…` memfd manifests already written).
//!
//! # Archive format (`NDGSNAP1`)
//!
//! `read` walks the golden's data dir and emits a **deterministic** archive so a
//! packed golden is byte-reproducible across nodes for a fixed on-disk state:
//!
//! ```text
//!   magic:      8   b"NDGSNAP1"
//!   id:        32   the ImageDigest raw SHA-256 bytes this snapshot claims
//!   n_entries:  8   u64 LE, count of file entries
//!   ── entries, sorted bytewise by relative path ──
//!     path_len:  8   u64 LE
//!     path:      …   relative path bytes (POSIX '/'-joined)
//!     data_len:  8   u64 LE
//!     data:      …   file bytes
//!   payload_hash: 32  BLAKE3 over everything above (magic..last entry)
//! ```
//!
//! Entries are sorted by relative path so enumeration order never changes the
//! bytes (same discipline as `ToolchainImage::content_fingerprint`'s layer sort).
//!
//! # verify — the sole write licence
//!
//! [`ContentIo::verify`] parses the header, recomputes the BLAKE3 `payload_hash`
//! over the archive body and rejects a mismatch (corruption/truncation →
//! [`VerifyError::HashMismatch`]), then checks the archive's **embedded
//! `ImageDigest` equals `id`** — the content-address commitment. `ImageDigest`
//! is a SHA-256 OCI *image* digest, not a hash of the (non-reproducible, RAM-
//! bearing) snapshot bytes, so it cannot be *re-derived* from the payload the way
//! an object-pack id is re-derived from its TOC; instead the archive *commits* to
//! the digit it belongs to and BLAKE3 guards the bytes' integrity end-to-end. A
//! passing `verify` — matching payload hash **and** matching embedded id — is the
//! sole licence to `write`.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use smolvm::agent::fork::{control_socket_cmd, control_socket_path};
use smolvm::agent::vm_data_dir;

use heart::sync::{ContentIo, VerifyError};

use crate::backend::smolvm::golden_vm_name;
use crate::cage::smolvm::GoldenPool;
use crate::toolchains::images::ImageDigest;
use crate::vm::GoldenId;

/// Archive magic tag for the golden-snapshot pack format.
const MAGIC: &[u8; 8] = b"NDGSNAP1";

/// Hard per-item cap for a packed golden: **8 GiB**.
///
/// Goldens carry a full toolchain rootfs (qcow2 base) plus a memfd RAM checkpoint
/// — an order of magnitude larger than an IR change file or an object pack — so
/// the ceiling is generous. It is a defence against a malicious announcement
/// over-allocating on the receiver *before* the bytes verify; the real transfer
/// size is bounded by the sparse on-disk footprint of the golden's data dir.
pub const MAX_GOLDEN_BYTES: usize = 8 * 1024 * 1024 * 1024;

// ---------------------------------------------------------------------------
// GoldenContentIo
// ---------------------------------------------------------------------------

/// [`ContentIo`] adapter over the smolvm golden store, keyed by [`ImageDigest`].
///
/// Holds the process-shared [`GoldenPool`] (the `ImageDigest → GoldenId`
/// registry consulted by `has`/`write` and by
/// [`crate::backend::smolvm::SmolvmRuntime::fork_golden`]). The on-disk snapshot
/// tree it packs/unpacks is `smolvm::agent::vm_data_dir(golden_vm_name(id))`.
pub struct GoldenContentIo {
    pool: Arc<GoldenPool>,
}

impl GoldenContentIo {
    /// Wrap a shared [`GoldenPool`] (the same pool the cage/runtime forks from).
    pub fn new(pool: Arc<GoldenPool>) -> Self {
        Self { pool }
    }

    /// Borrow the underlying pool.
    pub fn pool(&self) -> &Arc<GoldenPool> {
        &self.pool
    }

    /// The golden's on-disk data dir (`snapshot_dir`) for an image digest.
    fn snapshot_dir(id: &ImageDigest) -> PathBuf {
        vm_data_dir(&golden_vm_name(id))
    }

    /// If the golden is currently live and forkable (its control socket exists
    /// and answers `STATUS OK`), drive a fresh RAM + device checkpoint by
    /// sending `FORK <checkpoint_dir>` to its control socket.
    ///
    /// The checkpoint is written to `<golden_data_dir>/fork-snapshots/CURRENT/`,
    /// a sentinel subdirectory of the golden's own data dir (required by smolvm's
    /// Landlock confinement: the frozen golden VMM can only write inside its own
    /// data dir). A stale `CURRENT/` from a prior checkpoint is removed first.
    ///
    /// After `FORK` the golden is paused/frozen as the shared CoW base — exactly
    /// the same post-fork state as any `prepare_fork` call, and subsequent
    /// `fork_golden` operations continue to work from that frozen base.
    ///
    /// Returns `Ok(())` on success (checkpoint written), `Ok(())` silently when
    /// the golden is not live (no control socket → nothing to checkpoint, caller
    /// packs existing on-disk state), and `Err` only on a genuine control-socket
    /// protocol failure (socket present but command failed).
    fn checkpoint_live_golden(id: &ImageDigest) -> io::Result<()> {
        let name = golden_vm_name(id);
        let ctl = control_socket_path(&name);

        // No control socket → golden not running forkable on this node.
        // Fall through; read() will pack whatever is on disk.
        if !ctl.exists() {
            return Ok(());
        }

        // Probe STATUS first: a frozen (previously-forked) golden still answers
        // STATUS and can be checkpointed again; a golden that is merely paused
        // by the OS reports paused, which is still forkable.
        let status = control_socket_cmd(&ctl, "STATUS").map_err(|e| {
            io::Error::new(
                io::ErrorKind::Other,
                format!("golden '{name}' control socket STATUS failed: {e}"),
            )
        })?;
        if !status.starts_with("OK") {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("golden '{name}' not ready for checkpoint: {status}"),
            ));
        }

        // Write the fresh checkpoint into fork-snapshots/CURRENT/ inside the
        // golden's own data dir (Landlock-safe: the frozen VMM is confined there).
        let gdir = vm_data_dir(&name);
        let checkpoint_dir = gdir.join("fork-snapshots").join("CURRENT");

        // Remove a stale checkpoint from a prior read() call so the snapshot
        // is always fresh (not a replay of old RAM state).
        if checkpoint_dir.exists() {
            std::fs::remove_dir_all(&checkpoint_dir).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("remove stale checkpoint dir: {e}"),
                )
            })?;
        }
        std::fs::create_dir_all(&checkpoint_dir).map_err(|e| {
            io::Error::new(io::ErrorKind::Other, format!("create checkpoint dir: {e}"))
        })?;

        // Drive the FORK command: freezes the golden, writes memfd RAM + device
        // snapshot to checkpoint_dir. On success the golden stays paused as
        // the shared CoW base; subsequent fork_golden calls still work.
        let reply = control_socket_cmd(&ctl, &format!("FORK {}", checkpoint_dir.display()))
            .map_err(|e| {
                io::Error::new(
                    io::ErrorKind::Other,
                    format!("golden '{name}' FORK command failed: {e}"),
                )
            })?;
        if !reply.starts_with("OK") {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("golden '{name}' FORK returned non-OK: {reply}"),
            ));
        }

        tracing::debug!(
            golden = %name,
            checkpoint = %checkpoint_dir.display(),
            "live golden checkpointed before pack (V-GOLD-2)"
        );
        Ok(())
    }
}

impl ContentIo for GoldenContentIo {
    /// The toolchain-image content hash that keys a golden.
    type Id = ImageDigest;

    /// Read = checkpoint the live golden (if running), then pack the golden's
    /// `snapshot_dir` into a deterministic archive.
    ///
    /// When the golden's control socket is live, `read` first drives a fresh
    /// memfd RAM + device checkpoint via the `FORK` control-socket command
    /// (see [`Self::checkpoint_live_golden`] for the mechanism). This ensures
    /// the packed bytes reflect current golden RAM, not a stale on-disk snapshot.
    /// When the golden is not live on this node (no control socket), `read`
    /// falls back to packing the persisted snapshot dir as-is.
    ///
    /// Errors with [`io::ErrorKind::NotFound`] when no golden is registered for
    /// `id` (nothing to pack — the caller should announce only ids it holds).
    fn read(&self, id: &ImageDigest) -> io::Result<Vec<u8>> {
        if self.pool.lookup(id).is_none() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("no golden registered for {id}"),
            ));
        }
        let dir = Self::snapshot_dir(id);
        if !dir.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("golden snapshot dir missing for {id}: {}", dir.display()),
            ));
        }
        // Drive a fresh RAM checkpoint of the live golden before packing, so the
        // packed bytes capture current golden state rather than a prior stale
        // snapshot. Falls through silently when the golden is not live here.
        Self::checkpoint_live_golden(id)?;
        pack_snapshot(id, &dir)
    }

    /// Write = unpack the archive into a local `snapshot_dir` and register the
    /// golden in the [`GoldenPool`] under `id`.
    ///
    /// The caller guarantees [`Self::verify`] already succeeded for `(id, bytes)`
    /// — we re-verify anyway (verify-before-write is the trust anchor), then
    /// materialise the snapshot tree so a subsequent
    /// [`crate::backend::smolvm::SmolvmRuntime::fork_golden`] can CoW-restore from
    /// it.
    fn write(&self, id: &ImageDigest, bytes: &[u8]) -> io::Result<()> {
        // Verify-before-write: the sole licence to touch the disk.
        self.verify(id, bytes)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

        let dir = Self::snapshot_dir(id);
        unpack_snapshot(bytes, &dir)?;

        // Register the golden so fork_golden(id) resolves. The GoldenId mirrors
        // the deterministic golden VM name derived from the digest.
        self.pool.install(*id, GoldenId::new(golden_vm_name(id)));
        Ok(())
    }

    /// Whether a golden is registered for this `ImageDigest`.
    fn has(&self, id: &ImageDigest) -> io::Result<bool> {
        Ok(self.pool.lookup(id).is_some())
    }

    /// Verify `bytes` are a well-formed golden snapshot archive whose embedded
    /// id equals `id` and whose BLAKE3 payload hash matches.
    ///
    /// Full content-address check for this plane: a passing verify (matching
    /// payload hash **and** matching embedded id) is the sole licence to write.
    fn verify(&self, id: &ImageDigest, bytes: &[u8]) -> Result<(), VerifyError> {
        let (embedded, _entries) = parse_and_check(id, bytes)?;
        if embedded != *id {
            return Err(VerifyError::HashMismatch {
                expected: id.to_string(),
                got: embedded.to_string(),
            });
        }
        Ok(())
    }

    /// 8 GiB — see [`MAX_GOLDEN_BYTES`].
    fn max_item_bytes(&self) -> usize {
        MAX_GOLDEN_BYTES
    }
}

// ---------------------------------------------------------------------------
// Archive codec
// ---------------------------------------------------------------------------

/// Recursively collect `(relative_path, absolute_path)` for every regular file
/// under `root`, sorted bytewise by relative path for deterministic output.
fn collect_files(root: &Path) -> io::Result<Vec<(Vec<u8>, PathBuf)>> {
    fn walk(root: &Path, dir: &Path, out: &mut Vec<(Vec<u8>, PathBuf)>) -> io::Result<()> {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            let ft = entry.file_type()?;
            if ft.is_dir() {
                walk(root, &path, out)?;
            } else if ft.is_file() {
                let rel = path
                    .strip_prefix(root)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
                // POSIX '/'-joined relative path bytes (portable across nodes).
                let rel_str = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                out.push((rel_str.into_bytes(), path));
            }
            // Symlinks / special files are skipped: a golden data dir is qcow2
            // images + manifests (regular files); we never chase links out of the
            // tree.
        }
        Ok(())
    }
    let mut out = Vec::new();
    walk(root, root, &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Pack the golden data dir at `dir` into the `NDGSNAP1` archive, committing to
/// `id` and sealing with a trailing BLAKE3 payload hash.
fn pack_snapshot(id: &ImageDigest, dir: &Path) -> io::Result<Vec<u8>> {
    let files = collect_files(dir)?;

    let mut buf = Vec::new();
    buf.extend_from_slice(MAGIC);
    buf.extend_from_slice(id.as_bytes());
    buf.extend_from_slice(&(files.len() as u64).to_le_bytes());
    for (rel, abs) in &files {
        let data = std::fs::read(abs)?;
        buf.extend_from_slice(&(rel.len() as u64).to_le_bytes());
        buf.extend_from_slice(rel);
        buf.extend_from_slice(&(data.len() as u64).to_le_bytes());
        buf.extend_from_slice(&data);
    }
    // Seal with BLAKE3 over the whole body (magic..last entry).
    let payload_hash = heart::ContentHash::of_bytes(&buf);
    buf.extend_from_slice(payload_hash.as_bytes());
    Ok(buf)
}

/// A parsed entry: relative path bytes + file data.
struct Entry {
    rel: Vec<u8>,
    data: Vec<u8>,
}

/// Parse the archive, recompute + check the BLAKE3 payload hash, and return the
/// embedded [`ImageDigest`] plus the file entries. Every framing / hash failure
/// maps to a [`VerifyError`] so `verify` needs nothing else.
fn parse_and_check(
    id: &ImageDigest,
    bytes: &[u8],
) -> Result<(ImageDigest, Vec<Entry>), VerifyError> {
    // magic(8) + id(32) + n(8) + payload_hash(32) = 80 bytes minimum.
    const HEADER: usize = 8 + 32 + 8;
    const FOOTER: usize = 32;
    if bytes.len() < HEADER + FOOTER {
        return Err(VerifyError::HashMismatch {
            expected: id.to_string(),
            got: format!("(archive too short: {} bytes)", bytes.len()),
        });
    }
    if &bytes[..8] != MAGIC {
        return Err(VerifyError::HashMismatch {
            expected: id.to_string(),
            got: "(bad magic: not an NDGSNAP1 golden snapshot)".to_string(),
        });
    }

    let body = &bytes[..bytes.len() - FOOTER];
    let footer = &bytes[bytes.len() - FOOTER..];
    let computed = heart::ContentHash::of_bytes(body);
    if computed.as_bytes() != footer {
        return Err(VerifyError::HashMismatch {
            expected: heart::ContentHash::from_bytes({
                let mut b = [0u8; 32];
                b.copy_from_slice(footer);
                b
            })
            .to_string(),
            got: computed.to_string(),
        });
    }

    // Embedded id.
    let mut idb = [0u8; 32];
    idb.copy_from_slice(&bytes[8..40]);
    let embedded = ImageDigest::from_bytes(idb);

    // Entries.
    let mut pos = HEADER;
    let n = u64_at(bytes, 40)?;
    let mut entries = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let rel_len = u64_at(bytes, pos)? as usize;
        pos += 8;
        let rel = slice_at(bytes, pos, rel_len)?.to_vec();
        pos += rel_len;
        let data_len = u64_at(bytes, pos)? as usize;
        pos += 8;
        let data = slice_at(bytes, pos, data_len)?.to_vec();
        pos += data_len;
        entries.push(Entry { rel, data });
    }
    // Anything left over before the footer is malformed framing.
    if pos != bytes.len() - FOOTER {
        return Err(VerifyError::HashMismatch {
            expected: id.to_string(),
            got: format!(
                "(trailing bytes: parsed to {pos}, body ends at {})",
                bytes.len() - FOOTER
            ),
        });
    }

    Ok((embedded, entries))
}

fn u64_at(bytes: &[u8], off: usize) -> Result<u64, VerifyError> {
    let s = slice_at(bytes, off, 8)?;
    let mut b = [0u8; 8];
    b.copy_from_slice(s);
    Ok(u64::from_le_bytes(b))
}

fn slice_at(bytes: &[u8], off: usize, len: usize) -> Result<&[u8], VerifyError> {
    bytes
        .get(off..off + len)
        .ok_or_else(|| VerifyError::HashMismatch {
            expected: "(well-formed archive framing)".to_string(),
            got: format!(
                "(truncated: need {len} bytes at offset {off}, have {})",
                bytes.len()
            ),
        })
}

/// Unpack a **verified** archive into a fresh `dir` (materialising the golden's
/// snapshot tree). Existing contents at `dir` are removed first so a re-fetch is
/// idempotent.
fn unpack_snapshot(bytes: &[u8], dir: &Path) -> io::Result<()> {
    // The archive was already verified by the caller; re-parse for the entries.
    // A framing error here would mean verify/write disagreed — surface as InvalidData.
    let embedded_id = {
        let mut idb = [0u8; 32];
        idb.copy_from_slice(&bytes[8..40]);
        ImageDigest::from_bytes(idb)
    };
    let (_id, entries) = parse_and_check(&embedded_id, bytes)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;

    if dir.exists() {
        std::fs::remove_dir_all(dir)?;
    }
    std::fs::create_dir_all(dir)?;

    for entry in entries {
        let rel = String::from_utf8(entry.rel)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))?;
        // Reject path traversal: relative, no '..' components.
        let mut dest = dir.to_path_buf();
        for comp in rel.split('/') {
            if comp.is_empty() || comp == "." || comp == ".." {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("unsafe path component in golden archive: {rel:?}"),
                ));
            }
            dest.push(comp);
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&dest, &entry.data)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> ImageDigest {
        ImageDigest::from_bytes([byte; 32])
    }

    /// Build a small snapshot dir with a couple of files.
    fn make_snapshot(root: &Path) {
        std::fs::create_dir_all(root.join("disks")).unwrap();
        std::fs::write(root.join("base.qcow2"), b"qcow2-base-bytes").unwrap();
        std::fs::write(root.join("disks").join("overlay.raw"), b"overlay").unwrap();
        std::fs::write(root.join("machine.json"), b"{\"cpus\":1}").unwrap();
    }

    #[test]
    fn pack_is_deterministic() {
        let tmp = tempfile::TempDir::new().unwrap();
        make_snapshot(tmp.path());
        let id = digest(0xAB);
        let a = pack_snapshot(&id, tmp.path()).unwrap();
        let b = pack_snapshot(&id, tmp.path()).unwrap();
        assert_eq!(a, b, "same dir + id must pack byte-identically");
        assert_eq!(&a[..8], MAGIC);
    }

    #[test]
    fn verify_accepts_own_pack() {
        let tmp = tempfile::TempDir::new().unwrap();
        make_snapshot(tmp.path());
        let id = digest(0x11);
        let bytes = pack_snapshot(&id, tmp.path()).unwrap();
        let io = GoldenContentIo::new(Arc::new(GoldenPool::new()));
        io.verify(&id, &bytes).expect("own pack must verify");
    }

    #[test]
    fn verify_rejects_wrong_id() {
        let tmp = tempfile::TempDir::new().unwrap();
        make_snapshot(tmp.path());
        let bytes = pack_snapshot(&digest(0x22), tmp.path()).unwrap();
        let io = GoldenContentIo::new(Arc::new(GoldenPool::new()));
        let err = io
            .verify(&digest(0x33), &bytes)
            .expect_err("wrong id must fail");
        assert!(matches!(err, VerifyError::HashMismatch { .. }));
    }

    #[test]
    fn verify_rejects_corrupted_bytes() {
        let tmp = tempfile::TempDir::new().unwrap();
        make_snapshot(tmp.path());
        let id = digest(0x44);
        let mut bytes = pack_snapshot(&id, tmp.path()).unwrap();
        // Flip a byte in the middle of the payload.
        let mid = bytes.len() / 2;
        bytes[mid] ^= 0xFF;
        let io = GoldenContentIo::new(Arc::new(GoldenPool::new()));
        let err = io.verify(&id, &bytes).expect_err("corruption must fail");
        assert!(matches!(err, VerifyError::HashMismatch { .. }));
    }

    #[test]
    fn verify_rejects_bad_magic() {
        let io = GoldenContentIo::new(Arc::new(GoldenPool::new()));
        let junk = vec![0u8; 128];
        let err = io.verify(&digest(0x55), &junk).expect_err("junk must fail");
        assert!(matches!(err, VerifyError::HashMismatch { .. }));
    }

    #[test]
    fn pack_then_unpack_roundtrips_files() {
        let src = tempfile::TempDir::new().unwrap();
        make_snapshot(src.path());
        let id = digest(0x66);
        let bytes = pack_snapshot(&id, src.path()).unwrap();

        let dst = tempfile::TempDir::new().unwrap();
        let dst_dir = dst.path().join("golden");
        unpack_snapshot(&bytes, &dst_dir).unwrap();

        assert_eq!(
            std::fs::read(dst_dir.join("base.qcow2")).unwrap(),
            b"qcow2-base-bytes"
        );
        assert_eq!(
            std::fs::read(dst_dir.join("disks").join("overlay.raw")).unwrap(),
            b"overlay"
        );
        assert_eq!(
            std::fs::read(dst_dir.join("machine.json")).unwrap(),
            b"{\"cpus\":1}"
        );
    }

    #[test]
    fn has_reflects_pool_registration() {
        let pool = Arc::new(GoldenPool::new());
        let io = GoldenContentIo::new(pool.clone());
        let id = digest(0x77);
        assert!(!io.has(&id).unwrap());
        pool.install(id, GoldenId::new("nudox-golden-x"));
        assert!(io.has(&id).unwrap());
    }

    #[test]
    fn max_item_bytes_is_generous() {
        let io = GoldenContentIo::new(Arc::new(GoldenPool::new()));
        assert_eq!(io.max_item_bytes(), MAX_GOLDEN_BYTES);
        assert!(io.max_item_bytes() >= 1 << 30, "at least 1 GiB");
    }

    /// `checkpoint_live_golden` is a no-op (Ok) when there is no control socket —
    /// that is the state on a receiver node or when the golden hasn't been started
    /// yet. Exercises the early-return path without needing a live VM.
    #[test]
    fn checkpoint_live_golden_is_noop_when_no_control_socket() {
        // An ImageDigest whose corresponding golden_vm_name has no control socket.
        let id = digest(0x88);
        // golden_vm_name(id) -> "nudox-golden-<hex>"; no smolvm process runs here,
        // so vm_data_dir("nudox-golden-<hex>")/control.sock does not exist.
        let result = GoldenContentIo::checkpoint_live_golden(&id);
        assert!(
            result.is_ok(),
            "no control socket → checkpoint must be a silent no-op: {result:?}"
        );
    }
}
