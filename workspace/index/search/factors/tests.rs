use std::cmp::Ordering;

use super::*;

fn ppm(value: u32) -> QualityPpm {
    QualityPpm::new(value).expect("quality ppm in 0..=1_000_000")
}

fn bm25(value: f32) -> Bm25 {
    Bm25::new(value).expect("bm25 >= 0")
}

fn pct(value: f32) -> PopularityPct {
    PopularityPct::new(value).expect("popularity in 0..=1")
}

fn zeroed() -> RankingFactors {
    RankingFactors {
        bm25: bm25(0.0),
        quality_ppm: ppm(0),
        dependents: None,
        downloads: None,
        popularity_pct: None,
        withdrawn: false,
        squat_suspect: false,
        malware: false,
        verified_repo: false,
        exact_name: false,
        contains_name: false,
    }
}

fn id8_ceiling() -> f64 {
    let weights = FusionWeights::ID8;
    weights.dependents + weights.downloads + weights.text + weights.quality
}

fn assert_in_id8_bound(s: Score) {
    assert!(s.get().is_finite(), "score must be finite, got {}", s.get());
    assert!(s.get() >= 0.0, "score must be >= 0, got {}", s.get());
    let ceiling = id8_ceiling();
    assert!(
        s.get() <= ceiling + 1e-9,
        "score {} exceeds ID-8 ceiling {ceiling}",
        s.get()
    );
}

#[test]
fn ties_break_score_desc_then_name_asc() {
    let mut low = zeroed();
    low.quality_ppm = ppm(100_000);
    let mut high = zeroed();
    high.quality_ppm = ppm(900_000);
    let low_s = score(&low, &FusionWeights::ID8);
    let high_s = score(&high, &FusionWeights::ID8);
    assert!(high_s > low_s);
    assert_eq!(low_s, score(&low, &FusionWeights::ID8));

    let mut rows = [
        ("zeta", low_s),
        ("alpha", low_s),
        ("mu", high_s),
        ("beta", low_s),
    ];
    rows.sort_by(|a, b| rank_key(a.1, a.0).cmp(&rank_key(b.1, b.0)));
    assert_eq!(rows.map(|(name, _)| name), ["mu", "alpha", "beta", "zeta"]);

    let mut again = [
        ("zeta", low_s),
        ("alpha", low_s),
        ("mu", high_s),
        ("beta", low_s),
    ];
    again.sort_by(|a, b| rank_key(a.1, a.0).cmp(&rank_key(b.1, b.0)));
    assert_eq!(rows, again);

    assert_eq!(
        rank_key(low_s, "alpha").cmp(&rank_key(low_s, "alpha")),
        Ordering::Equal
    );
}

#[test]
fn withdrawn_multiplies_the_whole_score_by_a_quarter() {
    let mut clean = zeroed();
    clean.quality_ppm = ppm(800_000);
    clean.bm25 = bm25(4.0);
    clean.downloads = Some(100_000);
    let clean_s = score(&clean, &FusionWeights::ID8);

    let mut yanked = clean;
    yanked.withdrawn = true;
    let yanked_s = score(&yanked, &FusionWeights::ID8);

    assert_eq!(yanked_s, Score::new(clean_s.get() * Gate::WITHDRAWN));
    assert!(yanked_s < clean_s);
    assert!((yanked_s.get() / clean_s.get() - 0.25).abs() < 1e-12);
}

#[test]
fn none_downloads_does_not_nan() {
    let missing = zeroed();
    let missing_s = score(&missing, &FusionWeights::ID8);
    assert!(missing_s.get().is_finite());
    assert!(!missing_s.get().is_nan());
    assert_eq!(missing_s, Score::new(0.0));

    let mut deps_only = missing;
    deps_only.downloads = None;
    deps_only.dependents = Some(100);
    let got = score(&deps_only, &FusionWeights::ID8);
    assert!(got.get().is_finite());
    assert!(!got.get().is_nan());

    let mass = (100_u64).saturating_mul(DEPENDENT_DOWNLOAD_EQUIV) as f64;
    let unit = mass / POPULARITY_SATURATION as f64;
    let expect = Score::new((FusionWeights::DEPENDENTS + FusionWeights::DOWNLOADS) * unit);
    assert_eq!(got, expect);
    assert!(got > missing_s);
}

#[test]
fn score_is_finite_and_inside_the_id8_bound() {
    // Small table: numeric corners × gate flags. The loop is the property
    // (every combination is finite and bounded); the rows are the generator.
    let qualities = [0_u32, 1_000_000];
    let bm25s = [0.0_f32, 10.0, 100.0];
    let percentiles: [Option<f32>; 4] = [None, Some(0.0), Some(0.5), Some(1.0)];
    let downloads: [Option<u64>; 4] = [None, Some(0), Some(10_000_000), Some(u64::MAX)];
    let dependents: [Option<u32>; 3] = [None, Some(0), Some(4_000)];
    let flags = [
        (false, false, false),
        (true, false, false),
        (false, true, false),
        (false, false, true),
        (true, true, true),
    ];

    let mut checked = 0_u32;
    for &quality in &qualities {
        for &raw_bm25 in &bm25s {
            for &percentile in &percentiles {
                for &download in &downloads {
                    for &dependent in &dependents {
                        for &(withdrawn, squat_suspect, malware) in &flags {
                            let factors = RankingFactors {
                                bm25: bm25(raw_bm25),
                                quality_ppm: ppm(quality),
                                dependents: dependent,
                                downloads: download,
                                popularity_pct: percentile.map(pct),
                                withdrawn,
                                squat_suspect,
                                malware,
                                verified_repo: false,
                                exact_name: false,
                                contains_name: false,
                            };
                            assert_in_id8_bound(score(&factors, &FusionWeights::ID8));
                            checked += 1;
                        }
                    }
                }
            }
        }
    }
    assert_eq!(checked, 2 * 3 * 4 * 4 * 3 * 5);
}

#[test]
fn percentile_is_one_term_and_ignores_raw_mass() {
    let mut with_raw = zeroed();
    with_raw.popularity_pct = Some(pct(0.4));
    with_raw.downloads = Some(u64::MAX);
    with_raw.dependents = Some(u32::MAX);
    with_raw.bm25 = bm25(10.0);
    with_raw.quality_ppm = ppm(250_000);

    let mut bare = with_raw;
    bare.downloads = None;
    bare.dependents = None;
    assert_eq!(
        score(&with_raw, &FusionWeights::ID8),
        score(&bare, &FusionWeights::ID8),
        "a filled percentile must not stack on top of downloads or dependents"
    );

    let unit = f64::from(pct(0.4).get());
    let text = FusionWeights::TEXT * (10.0 / (10.0 + BM25_SATURATION));
    let quality = FusionWeights::QUALITY * (250_000.0 / 1_000_000.0);
    let popularity = (FusionWeights::DEPENDENTS + FusionWeights::DOWNLOADS) * unit;
    let expect = Score::new(popularity + text + quality);
    assert_eq!(score(&bare, &FusionWeights::ID8), expect);
}

#[test]
fn known_zero_percentile_is_not_missing_data() {
    let mut known = zeroed();
    known.popularity_pct = Some(pct(0.0));
    known.downloads = Some(POPULARITY_SATURATION);
    known.dependents = Some(10_000);

    let mut missing = known;
    missing.popularity_pct = None;

    assert_eq!(score(&known, &FusionWeights::ID8), Score::new(0.0));
    assert!(score(&missing, &FusionWeights::ID8) > score(&known, &FusionWeights::ID8));
}

#[test]
fn synthesized_mass_saturates_and_takes_the_max() {
    let mut at_cap = zeroed();
    at_cap.downloads = Some(POPULARITY_SATURATION);
    let mut over = zeroed();
    over.downloads = Some(POPULARITY_SATURATION.saturating_mul(4));
    let mut from_deps = zeroed();
    // 4_000 dependents * 2_500 = 10_000_000, the saturation constant.
    from_deps.dependents = Some(4_000);
    from_deps.downloads = Some(1);

    let cap = score(&at_cap, &FusionWeights::ID8);
    assert_eq!(cap, score(&over, &FusionWeights::ID8));
    assert_eq!(cap, score(&from_deps, &FusionWeights::ID8));
    assert_eq!(
        cap,
        Score::new(FusionWeights::DEPENDENTS + FusionWeights::DOWNLOADS)
    );
}

#[test]
fn name_flags_do_not_enter_the_sum() {
    let mut plain = zeroed();
    plain.quality_ppm = ppm(500_000);
    plain.bm25 = bm25(3.0);
    plain.popularity_pct = Some(pct(0.2));
    let mut flagged = plain;
    flagged.exact_name = true;
    flagged.contains_name = true;
    flagged.verified_repo = true;
    assert_eq!(
        score(&plain, &FusionWeights::ID8),
        score(&flagged, &FusionWeights::ID8)
    );
}

#[test]
fn gates_stack_and_stay_finite() {
    let mut clean = zeroed();
    clean.quality_ppm = ppm(1_000_000);
    clean.popularity_pct = Some(pct(1.0));
    let base = score(&clean, &FusionWeights::ID8);
    assert_in_id8_bound(base);

    let mut flagged = clean;
    flagged.malware = true;
    flagged.squat_suspect = true;
    flagged.withdrawn = true;
    let got = score(&flagged, &FusionWeights::ID8);
    let expect = Score::new(base.get() * Gate::MALWARE * Gate::SQUAT * Gate::WITHDRAWN);
    assert_eq!(got, expect);
    assert!(got.get().is_finite());
    assert!(got < base);
    assert!(got.get() > 0.0);
}

#[test]
fn non_finite_weights_cannot_produce_nan() {
    let mut weights = FusionWeights::ID8;
    weights.text = f64::NAN;
    weights.quality = f64::INFINITY;
    weights.dependents = f64::NEG_INFINITY;
    let mut factors = zeroed();
    factors.bm25 = bm25(5.0);
    factors.quality_ppm = ppm(1_000_000);
    factors.downloads = None;
    factors.popularity_pct = Some(pct(0.5));
    let got = score(&factors, &weights);
    assert!(got.get().is_finite());
    assert!(!got.get().is_nan());
}

#[test]
fn score_ord_is_total_over_non_finite_inputs() {
    let samples = [
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        -0.0,
        0.0,
        1.0,
        -1.0,
        f64::MIN,
        f64::MAX,
    ];
    let mut forward: Vec<Score> = samples.into_iter().map(Score::new).collect();
    let mut backward = forward.clone();
    forward.sort();
    backward.reverse();
    backward.sort();
    assert_eq!(forward, backward);
    for score in &forward {
        assert!(score.get().is_finite());
    }
    assert_eq!(Score::new(f64::NAN), Score::new(0.0));
    assert_eq!(Score::new(-0.0), Score::new(0.0));
}

#[test]
fn constructors_reject_the_outside_of_the_domain() {
    assert_eq!(QualityPpm::new(0).unwrap().get(), 0);
    assert_eq!(
        QualityPpm::new(1_000_000).unwrap().as_unit().to_bits(),
        1.0f64.to_bits()
    );
    assert_eq!(
        QualityPpm::new(500_000).unwrap().as_unit().to_bits(),
        0.5f64.to_bits()
    );
    assert!(QualityPpm::new(1_000_001).is_none());

    assert!(PopularityPct::new(0.0).is_some());
    assert!(PopularityPct::new(1.0).is_some());
    assert!(PopularityPct::new(-0.0).is_some());
    assert_eq!(PopularityPct::new(-0.0).unwrap().get().to_bits(), 0);
    assert!(PopularityPct::new(-0.01).is_none());
    assert!(PopularityPct::new(1.01).is_none());
    assert!(PopularityPct::new(f32::NAN).is_none());
    assert!(PopularityPct::new(f32::INFINITY).is_none());
    assert!(PopularityPct::new(f32::NEG_INFINITY).is_none());

    assert!(Bm25::new(0.0).is_some());
    assert_eq!(Bm25::new(-0.0).unwrap().get().to_bits(), 0);
    assert!(Bm25::new(-1.0).is_none());
    assert!(Bm25::new(f32::NAN).is_none());
    assert!(Bm25::new(f32::INFINITY).is_none());
    assert_eq!(Bm25::new(12.5).unwrap().get().to_bits(), 12.5f32.to_bits());
}

#[test]
fn id8_constants_match_the_plan() {
    assert_eq!(FusionWeights::DEPENDENTS.to_bits(), 0.45f64.to_bits());
    assert_eq!(FusionWeights::DOWNLOADS.to_bits(), 0.25f64.to_bits());
    assert_eq!(FusionWeights::TEXT.to_bits(), 0.20f64.to_bits());
    assert_eq!(FusionWeights::QUALITY.to_bits(), 0.10f64.to_bits());
    assert_eq!(FusionWeights::default(), FusionWeights::ID8);
    let sum = id8_ceiling();
    assert!((sum - 1.0).abs() < 1e-9);
    assert_eq!(Gate::WITHDRAWN.to_bits(), 0.25f64.to_bits());
    assert_eq!(DEPENDENT_DOWNLOAD_EQUIV, 2_500);
    assert_eq!(POPULARITY_SATURATION, 10_000_000);
}
