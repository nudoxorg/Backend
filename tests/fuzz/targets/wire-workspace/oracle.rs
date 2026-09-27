//! Oracle for the untrusted workspace manifest, delta, and commit envelopes.
//!
//! Success for manifest and commit must be a fixpoint of the public encoder.
//! The delta has no public encoder, so its law is determinism plus agreement
//! between the full decoder and the persisted header when both accept.

use backend_version::{
    Commit, CoverageWitness, ID_BYTES, MAX_WORKSPACE_WIRE_BYTES, MAX_WORKSPACE_WIRE_ITEMS,
    RelationBinding, SchemaIdentity, UntrustedWorkspaceManifest, WorkspaceDecodeError,
    WorkspaceDelta, partial_coverage,
};

use crate::{OracleFailure, Verdict};

/// Largest input this harness will mutate or replay.
///
/// The integer lives in the sibling `max_len` file so Nix and Rust share it.
pub(crate) const MAX_LEN: usize = crate::decimal_usize(include_str!("max_len"));

const _: () = assert!(MAX_LEN == MAX_WORKSPACE_WIRE_BYTES);

/// Canonical one-relation manifest used as the `canonical` seed.
///
/// # Errors
///
/// Returns the workspace constructor error when the fixture is not canonical.
pub(crate) fn canonical() -> Result<Vec<u8>, String> {
    let manifest = UntrustedWorkspaceManifest::new(
        1,
        vec![RelationBinding::new(
            SchemaIdentity::new(1, 7, 1),
            [1; ID_BYTES],
        )],
        Vec::new(),
        [3; ID_BYTES],
        CoverageWitness::Partial(partial_coverage(1)),
    )
    .map_err(|error| WorkspaceDecodeError::Semantic(error).to_string())?;
    Ok(manifest.encode())
}

/// Exercises manifest, delta, and commit admission on one buffer.
///
/// # Errors
///
/// Returns when a successful decode does not re-encode to a fixpoint, when
/// two reads of the same buffer disagree, or when the delta header and the
/// full transition disagree on identity fields they both accepted.
pub(crate) fn exercise(bytes: &[u8]) -> Result<(), OracleFailure> {
    judge(bytes).map(|_| ())
}

/// Classifies one buffer after the manifest, delta, and commit laws run.
///
/// # Errors
///
/// Returns when a law fails. A clean rejection is [`Verdict::Rejected`], not
/// an error.
pub(crate) fn judge(bytes: &[u8]) -> Result<Verdict, OracleFailure> {
    let manifest = check_manifest(bytes)?;
    let delta = check_delta(bytes)?;
    let commit = check_commit(bytes)?;
    if manifest || delta || commit {
        Ok(Verdict::Accepted)
    } else {
        Ok(Verdict::Rejected)
    }
}

fn check_manifest(bytes: &[u8]) -> Result<bool, OracleFailure> {
    let decoded = UntrustedWorkspaceManifest::decode_untrusted(bytes);
    let again = UntrustedWorkspaceManifest::decode_untrusted(bytes);
    if decoded != again {
        return Err(OracleFailure::new("manifest decode is not deterministic"));
    }
    let Ok(manifest) = decoded else {
        return Ok(false);
    };
    let encoded = manifest.encode();
    if encoded.len() > MAX_LEN {
        return Err(OracleFailure::new(
            "manifest encoder exceeded the wire byte cap",
        ));
    }
    let round = UntrustedWorkspaceManifest::decode_untrusted(&encoded)
        .map_err(|error| OracleFailure::new(format!("canonical manifest was rejected: {error}")))?;
    if round != manifest {
        return Err(OracleFailure::new(
            "manifest encode/decode is not a fixpoint",
        ));
    }
    if round.encode() != encoded {
        return Err(OracleFailure::new("manifest encode is not stable"));
    }
    Ok(true)
}

fn check_delta(bytes: &[u8]) -> Result<bool, OracleFailure> {
    let decoded = WorkspaceDelta::decode_untrusted(bytes);
    let again = WorkspaceDelta::decode_untrusted(bytes);
    if decoded != again {
        return Err(OracleFailure::new("delta decode is not deterministic"));
    }
    let header = WorkspaceDelta::decode_persisted_header(bytes, MAX_LEN);
    let header_again = WorkspaceDelta::decode_persisted_header(bytes, MAX_LEN);
    if header != header_again {
        return Err(OracleFailure::new(
            "delta header decode is not deterministic",
        ));
    }
    if let Ok(delta) = &decoded
        && delta.relations().len() > MAX_WORKSPACE_WIRE_ITEMS
    {
        return Err(OracleFailure::new(
            "delta admitted more relations than the wire cap",
        ));
    }
    if let Ok(header) = &header
        && header.relations().len() > MAX_WORKSPACE_WIRE_ITEMS
    {
        return Err(OracleFailure::new(
            "delta header admitted more relations than the wire cap",
        ));
    }
    // The header parser and the full parser accept different suffixes of the
    // same grammar, so one may reject while the other accepts. When both
    // accept, their identity fields are the same claim.
    if let (Ok(delta), Ok(header)) = (&decoded, &header) {
        if header.base() != delta.base() || header.target() != delta.target() {
            return Err(OracleFailure::new(
                "delta header roots disagree with the full transition",
            ));
        }
        if header.relations().len() != delta.relations().len() {
            return Err(OracleFailure::new(
                "delta header relation count disagrees with the full transition",
            ));
        }
        for (full, head) in delta.relations().iter().zip(header.relations()) {
            if full.schema() != head.schema()
                || full.base() != head.base()
                || full.target() != head.target()
                || full.delta() != head.delta()
            {
                return Err(OracleFailure::new(
                    "delta header relation identity disagrees with the full transition",
                ));
            }
        }
    }
    Ok(decoded.is_ok() || header.is_ok())
}

fn check_commit(bytes: &[u8]) -> Result<bool, OracleFailure> {
    let decoded = Commit::decode_untrusted(bytes);
    let again = Commit::decode_untrusted(bytes);
    if decoded != again {
        return Err(OracleFailure::new("commit decode is not deterministic"));
    }
    let Ok(commit) = decoded else {
        return Ok(false);
    };
    let encoded = commit.encode();
    if encoded.len() > MAX_LEN {
        return Err(OracleFailure::new(
            "commit encoder exceeded the wire byte cap",
        ));
    }
    let round = Commit::decode_untrusted(&encoded)
        .map_err(|error| OracleFailure::new(format!("canonical commit was rejected: {error}")))?;
    if round != commit {
        return Err(OracleFailure::new("commit encode/decode is not a fixpoint"));
    }
    if round.encode() != encoded {
        return Err(OracleFailure::new("commit encode is not stable"));
    }
    Ok(true)
}
