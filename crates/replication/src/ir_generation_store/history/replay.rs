// Bounded first-parent ancestry pages over immutable admitted records.
use super::catalog::*;
use super::*;

/// A replay node whose content-addressed commit and generation snapshot have
/// already passed their local identity and target checks. The page owns at
/// most one such lookahead outside its returned entries.
struct ValidatedReplayNode {
    record: HistoryCommitRecord,
    generation: LocalSemanticGeneration,
}

impl LocalSemanticGenerationFiles {
    pub(crate) fn replay_history(
        &self,
        target: &SemanticTargetKey,
        tip: HistoryCommitId,
    ) -> Result<HistoryReplay, String> {
        self.replay_history_page(target, tip)
    }

    pub(crate) fn continue_history_replay(
        &self,
        target: &SemanticTargetKey,
        cursor: HistoryReplayCursor,
    ) -> Result<HistoryReplay, String> {
        self.replay_history_page(target, cursor.next_ancestor)
    }

    #[cfg(test)]
    pub(crate) fn replay_history_with_page_hook(
        &self,
        target: &SemanticTargetKey,
        tip: HistoryCommitId,
        after_entry: impl FnMut() -> Result<(), String>,
    ) -> Result<HistoryReplay, String> {
        self.replay_history_page_with_hook(target, tip, after_entry)
    }

    fn replay_history_page(
        &self,
        target: &SemanticTargetKey,
        start: HistoryCommitId,
    ) -> Result<HistoryReplay, String> {
        self.replay_history_page_with_hook(target, start, || Ok(()))
    }

    fn replay_history_page_with_hook(
        &self,
        target: &SemanticTargetKey,
        start: HistoryCommitId,
        mut after_entry: impl FnMut() -> Result<(), String>,
    ) -> Result<HistoryReplay, String> {
        let target_root = self.target_root(target);
        let commits_root = target_root.join("history").join("commits");
        let mut entries = Vec::with_capacity(MAX_REPLAY_COMMITS);
        let mut next = None;
        let mut current = Some(load_replay_node(
            target_root.as_path(),
            target,
            &commits_root,
            start,
        )?);

        while let Some(node) = current.take() {
            let (node, parent) = validate_replay_parent(
                target_root.as_path(),
                target,
                &commits_root,
                node,
            )?;
            let parent_identity = parent.as_ref().map(|parent| parent.record.identity);
            entries.push(HistoryReplayEntry {
                commit: AdmittedHistoryCommit { record: node.record },
                generation: node.generation,
            });
            after_entry()?;

            if entries.len() == MAX_REPLAY_COMMITS {
                next = parent_identity.map(|next_ancestor| HistoryReplayCursor { next_ancestor });
                break;
            }
            current = parent;
        }

        entries.reverse();
        Ok(HistoryReplay { entries, next })
    }
}

/// Loads one node without walking its parent. Its predecessor has already
/// validated this node's first-parent edge when it created the lookahead.
fn load_replay_node(
    target_root: &Path,
    target: &SemanticTargetKey,
    commits_root: &Path,
    identity: HistoryCommitId,
) -> Result<ValidatedReplayNode, String> {
    let record = load_history_commit(commits_root, identity)?;
    if record.target != *target {
        return Err("semantic history commit belongs to another target".to_owned());
    }
    if !matches!(record.generation_root, HistoryGenerationRoot::NxfiV1(_)) {
        return Err("typed V2 history requires typed replay".to_owned());
    }
    let generation_record = load_record(target_root, record.generation, target)?;
    validate_commit_generation(&record, &generation_record)?;
    let generation = generation_from_validated_record(generation_record, record.stamp);
    Ok(ValidatedReplayNode { record, generation })
}

/// Validates the current node's direct parent edge and loads that parent as a
/// single owned lookahead. Reusing it on the next iteration avoids rereading
/// the same commit and generation merely because it is now the current node.
fn validate_replay_parent(
    target_root: &Path,
    target: &SemanticTargetKey,
    commits_root: &Path,
    current: ValidatedReplayNode,
) -> Result<(ValidatedReplayNode, Option<ValidatedReplayNode>), String> {
    let mut parent_records = Vec::with_capacity(current.record.parents.len());
    for identity in &current.record.parents {
        parent_records.push(load_history_commit(commits_root, *identity)?);
    }
    validate_parent_set_with_records(&current.record, &parent_records)?;

    let mut parent = None;
    for parent_record in parent_records {
        if !matches!(parent_record.generation_root, HistoryGenerationRoot::NxfiV1(_)) {
            return Err("typed V2 history requires typed replay".to_owned());
        }
        let parent_generation_record =
            load_record(target_root, parent_record.generation, target)?;
        validate_commit_generation(&parent_record, &parent_generation_record)?;
        let parent_generation =
            generation_from_validated_record(parent_generation_record, parent_record.stamp);
        if parent.is_none() {
            parent = Some(ValidatedReplayNode {
                record: parent_record,
                generation: parent_generation,
            });
        }
    }

    Ok((current, parent))
}
