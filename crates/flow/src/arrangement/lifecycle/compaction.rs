//! Plans and commits bounded level merges for one arrangement.

use super::*;

/// The compaction planner only needs a bounded suffix of run metadata to
/// select a merge.  Looking at every possible start turns a run-level policy
/// into an O(runs²) scan when an oversized run prevents a merge.
const MAX_COMPACTION_CANDIDATES: usize = 8;

impl<V: Clone + Ord + Debug + Eq + CanonicalValue + 'static> Arrangement<V> {
    pub(crate) fn plan_compaction_level(
        &self,
        level: usize,
        budget: CompactionBudget,
        trace: &TraceSpine,
        pending_runs_at_level_zero: usize,
        additional_live_bytes: usize,
        allow_promotion: bool,
    ) -> Result<Option<CompactionPlan<V>>, FlowError> {
        let run_count = self.levels[level].len();
        if run_count < 2 {
            return Ok(None);
        }

        // Prefer the newest compactable suffix. A single oversized run must
        // not prevent smaller neighboring runs from making progress. The
        // suffix is deliberately capped: choosing among all starts here
        // performs an avoidable quadratic metadata scan under skew.
        let max_take = run_count
            .min(budget.max_runs)
            .min(MAX_COMPACTION_CANDIDATES);
        let start = run_count.saturating_sub(max_take);
        let mut input_rows = 0_usize;
        let mut take = 0_usize;
        for run in &self.levels[level][start..] {
            let Some(next_rows) = input_rows.checked_add(run.len()) else {
                break;
            };
            if next_rows > budget.max_rows {
                break;
            }
            input_rows = next_rows;
            take += 1;
        }
        if take < 2 {
            // A level whose runs cannot be merged under the row envelope must
            // still make progress. Promote one immutable run by identity;
            // this does not inspect or rewrite its rows and keeps L0 bounded
            // for broad or adversarial batches. The destination level is
            // subject to the same policy on a later bounded step.
            let overfull = run_count > self.max_runs_per_level
                || (level == 0
                    && pending_runs_at_level_zero > 0
                    && run_count + pending_runs_at_level_zero > self.max_runs_per_level);
            if !allow_promotion || !overfull {
                return Ok(None);
            }
            if self.promotion_debt >= self.max_promotion_debt {
                return Err(FlowError::CompactionBackpressure);
            }
            let promoted = self.levels[level]
                .first()
                .cloned()
                .ok_or(FlowError::InvalidRoot)?;
            let target_level = level.checked_add(1).ok_or(FlowError::Overflow)?;
            if target_level >= self.max_levels {
                return Err(FlowError::CompactionBackpressure);
            }
            return Ok(Some(CompactionPlan {
                level,
                target_level,
                start: 0,
                take: 1,
                merged: Some(promoted),
                input_rows: 0,
                output_rows: 0,
                promoted: true,
            }));
        }
        let mut rows = Vec::with_capacity(input_rows);
        for run in &self.levels[level][start..start + take] {
            rows.extend(run.rows().cloned());
        }
        let merged = Run::from_rows(rows, trace.clone())?;
        if self
            .retained_bytes()
            .checked_add(additional_live_bytes)
            .and_then(|bytes| bytes.checked_add(merged.owner_retained_bytes()))
            .is_none_or(|bytes| bytes > self.max_retained_bytes)
        {
            return Err(FlowError::CompactionBackpressure);
        }
        let output_rows = merged.len();
        let target_level = level.checked_add(1).ok_or(FlowError::Overflow)?;
        if target_level >= self.max_levels {
            return Err(FlowError::CompactionBackpressure);
        }
        Ok(Some(CompactionPlan {
            level,
            target_level,
            start,
            take,
            merged: (!merged.is_empty()).then(|| Arc::new(merged)),
            input_rows: u64::try_from(input_rows).map_err(|_| FlowError::Overflow)?,
            output_rows: u64::try_from(output_rows).map_err(|_| FlowError::Overflow)?,
            promoted: false,
        }))
    }

    /// Plans one merge when an append would exceed a level's run limit.
    pub(super) fn plan_compaction_after_append(
        &self,
        trace: &TraceSpine,
        pending_runs: usize,
        pending_bytes: usize,
    ) -> Result<Option<CompactionPlan<V>>, FlowError> {
        let budget = CompactionBudget {
            max_rows: CompactionBudget::default().max_rows,
            max_runs: self.max_runs_per_level,
        };
        for level in 0..self.levels.len() {
            let after_append = self.levels[level]
                .len()
                .checked_add(usize::from(level == 0));
            if after_append.is_none_or(|count| count <= self.max_runs_per_level) {
                continue;
            }
            if let Some(plan) = self.plan_compaction_level(
                level,
                budget,
                trace,
                if level == 0 { pending_runs } else { 0 },
                pending_bytes,
                false,
            )? {
                return Ok(Some(plan));
            }
        }
        // A broad run can leave L0 temporarily unmergeable. Give every
        // overfull level a bounded promotion opportunity so pressure does
        // not accumulate forever above a permanently oversized lower run.
        for level in 0..self.levels.len() {
            let after_append = self.levels[level]
                .len()
                .checked_add(usize::from(level == 0));
            if after_append.is_none_or(|count| count <= self.max_runs_per_level) {
                continue;
            }
            if let Some(plan) = self.plan_compaction_level(
                level,
                budget,
                trace,
                if level == 0 { pending_runs } else { 0 },
                pending_bytes,
                true,
            )? {
                return Ok(Some(plan));
            }
        }
        Ok(None)
    }

    pub(crate) fn commit_compaction(&mut self, plan: CompactionPlan<V>) {
        let target_level = plan.target_level;
        while self.levels.len() <= target_level {
            self.levels.push(Vec::new());
        }
        self.levels[plan.level].drain(plan.start..plan.start + plan.take);
        if let Some(merged) = plan.merged {
            self.levels[target_level].push(merged);
        }
        if plan.promoted {
            self.promotion_debt = self.promotion_debt.saturating_add(1);
        } else {
            self.promotion_debt = self.promotion_debt.saturating_sub(1);
        }
    }
}
