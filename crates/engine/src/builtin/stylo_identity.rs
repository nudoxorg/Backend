//! Identities for native Stylo facts within one captured source revision.
//!
//! A structural address identifies an observation only within a selected host
//! revision and producer profile. It is not a persistent symbol identity and
//! says nothing about matching, cascade, or authored byte extents.

/// The semantic row family at one native structural address.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StyloRecordKind {
    /// One Stylo rule in the native rule tree.
    Rule,
    /// One parsed selector in a style rule.
    Selector,
    /// One retained native declaration after Stylo's own normalization.
    RetainedDeclaration,
    /// One authored source observation.
    SourceOccurrence,
    /// One native parser diagnostic.
    Diagnostic,
    /// One native dependency reported by the producer.
    Dependency,
}

/// The maximum supported nesting depth of a native rule address.
pub const MAX_STYLO_RULE_DEPTH: usize = 64;

/// Computes a revision-local semantic row identity from its exact host and
/// native structural position. The path is a normalized project-relative
/// source path; `rule_path` contains ordered child indexes from the stylesheet
/// root and `ordinal` distinguishes records within the selected row family.
/// `subordinal` distinguishes child observations, such as references within a
/// retained declaration.
///
/// # Errors
/// Returns an error for an invalid identity, path, or excessive nesting.
pub fn stylo_record_key(
    project: [u8; 32],
    path: &str,
    content: [u8; 32],
    source: [u8; 32],
    producer: [u8; 32],
    worker_artifact: [u8; 32],
    adapter: [u8; 32],
    configuration: [u8; 32],
    rule_path: &[u32],
    kind: StyloRecordKind,
    ordinal: u32,
    subordinal: u32,
) -> Result<[u8; 32], &'static str> {
    if project == [0; 32]
        || content == [0; 32]
        || source == [0; 32]
        || producer == [0; 32]
        || worker_artifact == [0; 32]
        || adapter == [0; 32]
        || configuration == [0; 32]
        || path.is_empty()
        || path.len() > 4096
        || path.starts_with('/')
        || path.as_bytes().get(1) == Some(&b':') && path.as_bytes()[0].is_ascii_alphabetic()
        || path.contains('\\')
        || path.bytes().any(|byte| byte.is_ascii_control())
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || rule_path.len() > MAX_STYLO_RULE_DEPTH
    {
        return Err("invalid Stylo source identity or structural address");
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-standalone-v1.record\0");
    hash.update(&project);
    hash.update(&(path.len() as u64).to_be_bytes());
    hash.update(path.as_bytes());
    hash.update(&content);
    hash.update(&source);
    hash.update(&producer);
    hash.update(&worker_artifact);
    hash.update(&adapter);
    hash.update(&configuration);
    hash.update(&(rule_path.len() as u64).to_be_bytes());
    for component in rule_path {
        hash.update(&component.to_be_bytes());
    }
    hash.update(&[match kind {
        StyloRecordKind::Rule => 1,
        StyloRecordKind::Selector => 2,
        StyloRecordKind::RetainedDeclaration => 3,
        StyloRecordKind::Diagnostic => 4,
        StyloRecordKind::Dependency => 5,
        StyloRecordKind::SourceOccurrence => 6,
    }]);
    hash.update(&ordinal.to_be_bytes());
    hash.update(&subordinal.to_be_bytes());
    Ok(*hash.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_revision_and_structural_address_are_distinct() {
        let key = |content, path: &str, address: &[u32], kind| {
            stylo_record_key(
                [1; 32], path, content, [6; 32], [3; 32], [7; 32], [8; 32], [5; 32], address, kind,
                0, 0,
            )
        };
        let base = key([2; 32], "src/a.css", &[0, 1], StyloRecordKind::Rule).unwrap();
        assert_ne!(
            base,
            key([4; 32], "src/a.css", &[0, 1], StyloRecordKind::Rule).unwrap()
        );
        assert_ne!(
            base,
            key([2; 32], "src/b.css", &[0, 1], StyloRecordKind::Rule).unwrap()
        );
        assert_ne!(
            base,
            key([2; 32], "src/a.css", &[0, 2], StyloRecordKind::Rule).unwrap()
        );
        assert_ne!(
            base,
            key([2; 32], "src/a.css", &[0, 1], StyloRecordKind::Selector).unwrap()
        );
        assert!(key([2; 32], "../a.css", &[0], StyloRecordKind::Rule).is_err());
        assert!(key([2; 32], "a\0.css", &[0], StyloRecordKind::Rule).is_err());
        assert!(key([2; 32], "C:/a.css", &[0], StyloRecordKind::Rule).is_err());
        assert!(
            key(
                [2; 32],
                "a.css",
                &[0; MAX_STYLO_RULE_DEPTH + 1],
                StyloRecordKind::Rule
            )
            .is_err()
        );
    }
}

/// Derives an authored occurrence identity from an exact original-host byte
/// site and the native observer provenance. It does not prove the bytes or
/// a semantic target; those require the live capture and correspondence proof.
///
/// # Errors
/// Rejects invalid identity, host path, source range or observer recipe.
pub fn stylo_source_occurrence_key(
    project: [u8; 32],
    path: &str,
    content: [u8; 32],
    source: [u8; 32],
    producer: [u8; 32],
    worker_artifact: [u8; 32],
    adapter: [u8; 32],
    configuration: [u8; 32],
    observer_recipe: &str,
    bom_removed: bool,
    role: backend_semantic::stylo_analysis::StyloSourceRole,
    start: u64,
    end: u64,
) -> Result<[u8; 32], &'static str> {
    // Reuse the canonical host/path and revision guard, not a native node ID.
    stylo_record_key(
        project,
        path,
        content,
        source,
        producer,
        worker_artifact,
        adapter,
        configuration,
        &[],
        StyloRecordKind::Rule,
        0,
        0,
    )?;
    if start >= end || observer_recipe.is_empty() || observer_recipe.len() > 4096 {
        return Err("invalid Stylo source occurrence identity");
    }
    let mut hash = blake3::Hasher::new();
    hash.update(b"nudox.stylo-standalone-v1.authored-site\0");
    hash.update(&project);
    hash.update(&(path.len() as u64).to_be_bytes());
    hash.update(path.as_bytes());
    hash.update(&content);
    hash.update(&source);
    hash.update(&producer);
    hash.update(&worker_artifact);
    hash.update(&adapter);
    hash.update(&configuration);
    hash.update(&(observer_recipe.len() as u64).to_be_bytes());
    hash.update(observer_recipe.as_bytes());
    hash.update(&[u8::from(bom_removed)]);
    hash.update(&[match role {
        backend_semantic::stylo_analysis::StyloSourceRole::ClassName => 1,
        backend_semantic::stylo_analysis::StyloSourceRole::CustomPropertyDeclarationName => 2,
    }]);
    hash.update(&start.to_be_bytes());
    hash.update(&end.to_be_bytes());
    Ok(*hash.finalize().as_bytes())
}
