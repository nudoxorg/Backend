//! Quality scoring: README richness, temporal freshness, fused total.

use super::score::Score;
use super::types::ExtractionInput;

fn readme_score(readme: Option<&str>) -> Score {
    let mut s = Score::new();
    let (text_len, code_blocks, sections) = match readme {
        None => (0usize, 0u32, 0u32),
        Some(r) => {
            let text_len = r.len();
            // Count fenced code blocks: lines starting with ```
            let code_blocks = r
                .lines()
                .filter(|l| l.trim_start().starts_with("```"))
                .count() as u32
                / 2; // open+close = 1 block
            // Count headings.
            let sections = r
                .lines()
                .filter(|l| l.trim_start().starts_with('#'))
                .count() as u32;
            (text_len, code_blocks, sections)
        }
    };
    s.frac("readme_text", 75, (text_len as f64 / 3000.).min(1.0));
    s.n("readme_code_blocks", 25, (code_blocks * 5).min(25));
    s.has("readme_has_code", 30, code_blocks > 0);
    s.n("readme_sections", 30, (sections * 4).min(30));
    s
}

/// Horizon (days) over which release freshness linearly decays to zero.
const FRESHNESS_HORIZON_DAYS: f64 = 365.0;
/// A package is "dead" when its last release is older than this *and*
/// release count is below [`DEADNESS_LOW_RELEASE_COUNT`].
const DEADNESS_AGE_DAYS: u32 = 730;
/// Low-ish release count paired with a very old last release → deadness demotion.
const DEADNESS_LOW_RELEASE_COUNT: u32 = 5;

/// Temporal quality group (lib.rs-inspired, simplified).
///
/// - **freshness**: higher when released recently; linear decay to 0 at ~365 days
/// - **maturity**: multi-release signal (caps at 10 releases)
/// - **not_dead**: demotion when last release is very old *and* release count is low
///
/// Returns `None` when no temporal inputs are present so the group is omitted
/// entirely (contributes 0 without changing the static denominator).
fn temporal_score(input: &ExtractionInput<'_>) -> Option<Score> {
    if input.last_release_days_ago.is_none() && input.release_count.is_none() {
        return None;
    }

    let mut t = Score::new();

    // Freshness: 1.0 at day 0 → 0.0 at FRESHNESS_HORIZON_DAYS (and beyond).
    if let Some(days) = input.last_release_days_ago {
        let freshness = (1.0 - f64::from(days) / FRESHNESS_HORIZON_DAYS).clamp(0.0, 1.0);
        t.frac("freshness", 15, freshness);
    }

    // Maturity: multi-release already partial; light signal here for temporal axis.
    if let Some(release_count) = input.release_count {
        t.frac(
            "maturity",
            10,
            (f64::from(release_count) / 10.0).min(1.0),
        );
    }

    // Deadness demotion: very old last release AND low-ish release count.
    // Score algebra is non-negative, so demotion = missing the `not_dead` points.
    match (input.last_release_days_ago, input.release_count) {
        (Some(days), Some(rc)) => {
            let is_dead = days >= DEADNESS_AGE_DAYS && rc < DEADNESS_LOW_RELEASE_COUNT;
            t.has("not_dead", 10, !is_dead);
        }
        (Some(days), None) => {
            // Without release_count we cannot assert deadness; only reward recent.
            t.has("not_dead", 10, days < DEADNESS_AGE_DAYS);
        }
        // release_count alone: maturity already scored; no deadness signal.
        (None, Some(_)) => {}
        (None, None) => unreachable!("gated above"),
    }

    Some(t)
}

pub(crate) fn compute_quality(input: &ExtractionInput<'_>) -> f32 {
    let mut score = Score::new();

    // Description length.
    let desc_len = input.description.map(|d| d.len()).unwrap_or(0);
    score.frac("description_len", 30, (desc_len as f64 / 300.).min(1.0));

    // Manifest completeness.
    score.has("repository", 10, input.has_repository);
    score.has("documentation", 20, input.has_documentation);
    score.has("license", 10, input.has_license);
    score.has("keywords", 7, !input.manifest_keywords.is_empty());
    score.has("categories", 5, !input.manifest_categories.is_empty());

    // README richness as a group.
    let rs = readme_score(input.readme);
    score.group("README", 5, rs);

    // Code size.
    score.has("non_trivial", 2, input.loc > 700);
    score.has("non_giant", 1, input.loc < 80_000);
    score.frac("loc", 3, (input.loc as f64 / 10_000.).min(1.0));

    // Release-history group — only contributes when release_count is known.
    if let Some(release_count) = input.release_count {
        let withdrawn_count = input.withdrawn_count.unwrap_or(0);
        let mut rh = Score::new();
        rh.n("release_maturity", 20, i64::from(release_count.min(20)));
        let ratio = if release_count == 0 {
            0.0f64
        } else {
            f64::from(withdrawn_count) / f64::from(release_count)
        };
        rh.has("low_withdrawn_ratio", 3, ratio < 0.15);
        score.group("release_history", 23, rh);
    }

    // Temporal group — only when last_release_days_ago and/or release_count set.
    // Missing temporal inputs → group omitted (contributes 0, no crash).
    if let Some(ts) = temporal_score(input) {
        score.group("temporal", 25, ts);
    }

    score.total() as f32
}
