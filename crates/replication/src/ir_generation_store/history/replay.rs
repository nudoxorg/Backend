// Bounded first-parent ancestry pages over immutable admitted records.
use super::catalog::*;
use super::*;
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

    fn replay_history_page(
        &self,
        target: &SemanticTargetKey,
        start: HistoryCommitId,
    ) -> Result<HistoryReplay, String> {
        let target_root = self.target_root(target);
        let commits_root = target_root.join("history").join("commits");
        let mut entries = Vec::with_capacity(MAX_REPLAY_COMMITS);
        let mut cursor = start;
        let mut next = None;
        loop {
            let record =
                validate_history_commit_node(target_root.as_path(), target, &commits_root, cursor)?;
            let generation_record = load_record(target_root.as_path(), record.generation, target)?;
            let generation = generation_from_record(generation_record, record.stamp)?;
            let parent = record.parents.first().copied();
            entries.push(HistoryReplayEntry {
                commit: AdmittedHistoryCommit { record },
                generation,
            });
            let Some(parent) = parent else {
                break;
            };
            if entries.len() == MAX_REPLAY_COMMITS {
                next = Some(HistoryReplayCursor {
                    next_ancestor: parent,
                });
                break;
            }
            cursor = parent;
        }
        entries.reverse();
        Ok(HistoryReplay { entries, next })
    }
}
