use super::*;

#[derive(Debug, Clone)]
pub(super) struct Working {
    pub(super) original_idx: usize,
    pub(super) item: RankedItem,
    pub(super) dep_relation: Option<DepRelation>,
    pub(super) usage: Option<UsageStat>,
    pub(super) labels: Vec<LocalLabel>,
    pub(super) boosted: f32,
}

/// Place items best-first under the constraint that an item originally at
/// index `i` may not land earlier than `i - max_rank_climb`.
///
/// When all bonuses are zero this recovers the input order for strictly
/// descending scores (and preserves relative order via `original_idx` ties).
pub(super) fn reorder_with_climb_cap(items: Vec<Working>, max_rank_climb: usize) -> Vec<Working> {
    let n = items.len();
    if n == 0 {
        return items;
    }

    let mut remaining = items;
    let mut out = Vec::with_capacity(n);

    for pos in 0..n {
        // Eligible = not yet placed and allowed at `pos` under the climb cap.
        // Every remaining item becomes eligible eventually as `pos` grows, so the
        // filter is non-empty whenever `remaining` is.
        let best_idx = remaining
            .iter()
            .enumerate()
            .filter(|(_, w)| w.original_idx.saturating_sub(max_rank_climb) <= pos)
            .max_by(|(_, a), (_, b)| cmp_working(a, b))
            .map(|(i, _)| i);

        let best_idx = if let Some(i) = best_idx {
            i
        } else {
            if remaining.is_empty() {
                break;
            }
            0
        };
        out.push(remaining.remove(best_idx));
    }

    out
}

/// Prefer higher boosted score; on ties prefer earlier original rank (identity
/// when bonuses are zero), then lower package id for full determinism.
fn cmp_working(a: &Working, b: &Working) -> std::cmp::Ordering {
    a.boosted
        .total_cmp(&b.boosted)
        .then_with(|| b.original_idx.cmp(&a.original_idx)) // smaller original_idx wins under max_by
        .then_with(|| b.item.id.cmp(&a.item.id))
}

pub(super) fn local_name_matches(pkg: &LocalOnlyPackage, query: &str) -> bool {
    let q = query.to_ascii_lowercase();
    let name = pkg.name.to_ascii_lowercase();
    if name == q || name.contains(&q) {
        return true;
    }
    pkg.keywords.iter().any(|k| k.eq_ignore_ascii_case(query))
}
